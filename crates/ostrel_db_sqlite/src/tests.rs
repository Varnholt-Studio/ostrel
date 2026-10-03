//! Tests that need the private parts of the driver: states a caller cannot reach through the
//! contract, such as an exhausted sequence or a damaged file.

use std::future::Future;
use std::task::{Context, Poll, Waker};

use ostrel_db::api::{
    CollectionChange, Connection, DbError, MapEntry, OpId, Query, ReplicaId, RowId, SetChange,
    Value, Write,
};

use super::{MEMORY, SCHEME, SqliteConnection, SqliteDriver};

fn block_on<T>(f: impl Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("sqlite future was not ready"),
    }
}

fn open() -> SqliteConnection {
    SqliteDriver::new()
        .open(&format!("{SCHEME}{MEMORY}"))
        .unwrap()
}

fn rid(n: u128) -> RowId {
    RowId::from_hex(&format!("{n:032x}")).unwrap()
}

fn insert(row: u128, fields: Vec<(u32, Value)>) -> Write {
    Write::Insert {
        model: 1,
        row: rid(row),
        fields,
        collections: vec![],
    }
}

fn count(c: &SqliteConnection, table: &str) -> i64 {
    c.conn
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn an_exhausted_sequence_fails_clearly_and_keeps_its_value() {
    let mut c = open();
    c.conn
        .execute(
            "INSERT INTO ostrel_sequences (key, last) VALUES ('k', ?1)",
            [i64::MAX - 1],
        )
        .unwrap();
    block_on(async {
        let mut tx = c.begin().await.unwrap();
        assert_eq!(tx.next_in_sequence("k").await, Ok(i64::MAX as u64));
        assert_eq!(
            tx.next_in_sequence("k").await,
            Err(DbError::Backend("sequence exhausted".into()))
        );
        tx.rollback().await.unwrap();
    });
    let last: i64 = c
        .conn
        .query_row(
            "SELECT last FROM ostrel_sequences WHERE key = 'k'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(last, i64::MAX - 1);
}

#[test]
fn sequence_keys_with_nul_are_distinct_keys() {
    let mut c = open();
    block_on(async {
        let mut tx = c.begin().await.unwrap();
        for key in ["a\u{0}b", "a\u{0}c", "a", "a\u{0}"] {
            assert_eq!(tx.next_in_sequence(key).await, Ok(1), "{key:?}");
        }
        tx.commit().await.unwrap();
    });
    assert_eq!(count(&c, "ostrel_sequences"), 4);
}

#[test]
fn a_damaged_stored_value_is_a_backend_error_not_a_panic() {
    let mut c = open();
    let w = [insert(1, vec![(0, Value::Bool(true))])];
    block_on(async {
        let mut tx = c.begin().await.unwrap();
        tx.apply(&w).await.unwrap();
        tx.commit().await.unwrap();
    });
    for damage in [&[1u8, 7][..], &[][..], &[99][..]] {
        c.conn
            .execute("UPDATE ostrel_fields SET value = ?1", [damage])
            .unwrap();
        assert!(
            matches!(block_on(c.query(&Query::all(1))), Err(DbError::Backend(_))),
            "{damage:?}"
        );
    }
    c.conn
        .execute("UPDATE ostrel_rows SET version = -1", [])
        .unwrap();
    c.conn
        .execute("UPDATE ostrel_fields SET value = ?1", [&[1u8, 1][..]])
        .unwrap();
    assert!(matches!(
        block_on(c.query(&Query::all(1))),
        Err(DbError::Backend(_))
    ));
}

#[test]
fn a_value_nested_too_deeply_is_refused_before_anything_is_written() {
    let mut c = open();
    let mut deep = Value::Null;
    for _ in 0..=crate::codec::MAX_DEPTH {
        deep = Value::List(vec![deep]);
    }
    let set = CollectionChange::Set(vec![SetChange::Add {
        elem: deep.clone(),
        tag: OpId {
            replica: ReplicaId(1),
            seq: 1,
        },
    }]);
    let map = CollectionChange::Map(vec![MapEntry {
        key: Value::Null,
        value: Some(deep.clone()),
        hlc: rid(1).hlc(),
    }]);
    let batches = [
        vec![insert(1, vec![]), insert(2, vec![(0, deep)])],
        vec![
            insert(1, vec![]),
            Write::Insert {
                model: 1,
                row: rid(2),
                fields: vec![],
                collections: vec![(0, set)],
            },
        ],
        vec![
            insert(1, vec![]),
            Write::Insert {
                model: 1,
                row: rid(2),
                fields: vec![],
                collections: vec![(0, map)],
            },
        ],
    ];
    for b in batches {
        block_on(async {
            let mut tx = c.begin().await.unwrap();
            assert_eq!(
                tx.apply(&b).await,
                Err(DbError::Invalid("value nested too deeply"))
            );
            tx.rollback().await.unwrap();
        });
        assert_eq!(count(&c, "ostrel_rows"), 0);
    }
}

#[test]
fn a_refused_call_is_undone_inside_the_transaction() {
    let mut c = open();
    block_on(async {
        let mut tx = c.begin().await.unwrap();
        // The second write conflicts with the first: the savepoint takes both back, so not even
        // this transaction sees the first one.
        assert_eq!(
            tx.apply(&[insert(1, vec![]), insert(1, vec![])]).await,
            Err(DbError::Conflict)
        );
        tx.rollback().await.unwrap();
    });
    assert_eq!(count(&c, "ostrel_rows"), 0);
    let tx = block_on(c.begin()).unwrap();
    drop(tx);
    // Dropping the transaction ended it: the connection is out of any transaction.
    assert!(c.conn.is_autocommit());
}

#[test]
fn equal_elements_of_different_variants_share_one_tag_and_keep_the_first_value() {
    let mut c = open();
    let add = |elem: Value, seq: u32| SetChange::Add {
        elem,
        tag: OpId {
            replica: ReplicaId(1),
            seq,
        },
    };
    let w = [Write::Insert {
        model: 1,
        row: rid(1),
        fields: vec![],
        collections: vec![(
            0,
            CollectionChange::Set(vec![
                add(Value::int(1).unwrap(), 1),
                add(Value::float(1.0).unwrap(), 2),
                add(Value::time(1).unwrap(), 1),
            ]),
        )],
    }];
    block_on(async {
        let mut tx = c.begin().await.unwrap();
        tx.apply(&w).await.unwrap();
        tx.commit().await.unwrap();
    });
    let rows = block_on(c.query(&Query::all(1))).unwrap().0;
    let expected = ostrel_db::api::CollectionState::Set(vec![ostrel_db::api::SetTag {
        elem: Value::int(1).unwrap(),
        tag: OpId {
            replica: ReplicaId(1),
            seq: 2,
        },
    }]);
    assert_eq!(rows[0].collections, vec![(0, expected)]);
}
