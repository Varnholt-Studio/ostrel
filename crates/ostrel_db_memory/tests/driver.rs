//! Behaviour of the in memory driver against the rules of the contract in `ostrel_db::api`.
//!
//! The adapter independent conformance suite lives in `tests/db-conformance/`; these tests cover
//! the same rules at the Rust level plus what is specific to this driver (URL, shared database,
//! first committer wins, failed transactions, order of mixed and missing values).

use std::future::Future;
use std::task::{Context, Poll, Waker};

use ostrel_db::api::{
    Connection, DbError, Dir, Driver, FieldId, MigrationPlan, ModelId, Query, RowId, Rows,
    ServerSeq, StoredOp, Value, Write,
};
use ostrel_db_memory::MemoryDriver;

/// Runs a future that never waits. Every future of the in memory driver is ready on first poll.
fn block_on<T>(f: impl Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("in memory future was not ready"),
    }
}

fn connect() -> Box<dyn Connection> {
    let registry: Vec<Box<dyn Driver>> = vec![Box::new(MemoryDriver::new())];
    block_on(registry[0].connect("memory:")).unwrap()
}

fn all(model: ModelId) -> Query {
    query(model, vec![], None)
}

fn query(model: ModelId, order: Vec<(FieldId, Dir)>, limit: Option<u32>) -> Query {
    Query {
        model,
        order,
        limit,
    }
}

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn insert(model: ModelId, row: u128, fields: Vec<(FieldId, Value)>) -> Write {
    Write::Insert {
        model,
        row: RowId(row),
        fields,
    }
}

fn update(model: ModelId, row: u128, v: u64, fields: Vec<(FieldId, Value)>) -> Write {
    Write::Update {
        model,
        row: RowId(row),
        expect_version: v,
        fields,
    }
}

fn delete(model: ModelId, row: u128, v: u64) -> Write {
    Write::Delete {
        model,
        row: RowId(row),
        expect_version: v,
    }
}

fn ids(rows: Rows) -> Vec<u128> {
    rows.0.iter().map(|r| r.id.0).collect()
}

/// Commits `writes` in one transaction on `conn`.
fn commit(conn: &mut Box<dyn Connection>, writes: &[Write]) {
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(writes).await.unwrap();
        tx.commit().await.unwrap();
    });
}

#[test]
fn name_capabilities_and_url_scheme() {
    let d = MemoryDriver::new();
    assert_eq!(d.name(), "memory");
    assert!(d.capabilities().transactions);
    assert!(d.capabilities().sequences);
    for url in ["postgres://x", "sqlite:a.db", "", "memory", "MEMORY:"] {
        assert!(
            matches!(block_on(d.connect(url)), Err(DbError::Unsupported(_))),
            "{url}"
        );
    }
    assert!(block_on(d.connect("memory:")).is_ok());
}

#[test]
fn empty_migration_succeeds() {
    let mut conn = connect();
    assert_eq!(block_on(conn.migrate(&MigrationPlan::default())), Ok(()));
}

#[test]
fn insert_conflicts_on_live_and_deleted_ids_in_every_model() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[insert(1, 5, vec![])]).await.unwrap();
        assert_eq!(
            tx.apply(&[insert(1, 5, vec![])]).await,
            Err(DbError::Conflict)
        );
        tx.rollback().await.unwrap();

        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[insert(1, 5, vec![])]).await.unwrap();
        tx.commit().await.unwrap();
        for model in [1, 2] {
            let mut tx = conn.begin().await.unwrap();
            assert_eq!(
                tx.apply(&[insert(model, 5, vec![])]).await,
                Err(DbError::Conflict)
            );
            tx.rollback().await.unwrap();
        }
    });
    commit(&mut conn, &[delete(1, 5, 1)]);
    block_on(async {
        for model in [1, 2] {
            let mut tx = conn.begin().await.unwrap();
            assert_eq!(
                tx.apply(&[insert(model, 5, vec![])]).await,
                Err(DbError::Conflict)
            );
            tx.rollback().await.unwrap();
        }
        assert!(conn.query(&all(1)).await.unwrap().0.is_empty());
    });
}

#[test]
fn update_and_delete_of_missing_dead_or_foreign_rows_are_not_found() {
    let mut conn = connect();
    commit(&mut conn, &[insert(1, 3, vec![]), insert(1, 4, vec![])]);
    commit(&mut conn, &[delete(1, 4, 1)]);
    let cases = [
        update(1, 9, 1, vec![]),
        delete(1, 9, 1),
        update(2, 3, 1, vec![]),
        delete(2, 3, 1),
        update(1, 4, 2, vec![]),
        delete(1, 4, 2),
    ];
    for w in cases {
        block_on(async {
            let mut tx = conn.begin().await.unwrap();
            assert_eq!(
                tx.apply(std::slice::from_ref(&w)).await,
                Err(DbError::NotFound),
                "{w:?}"
            );
            tx.rollback().await.unwrap();
        });
    }
}

#[test]
fn stale_versions_are_rejected_and_change_nothing() {
    let mut conn = connect();
    commit(&mut conn, &[insert(1, 1, vec![(0, text("a"))])]);
    for w in [
        update(1, 1, 2, vec![(0, text("b"))]),
        update(1, 1, 0, vec![]),
        delete(1, 1, 9),
    ] {
        block_on(async {
            let mut tx = conn.begin().await.unwrap();
            assert_eq!(tx.apply(&[w]).await, Err(DbError::VersionMismatch));
            tx.rollback().await.unwrap();
        });
    }
    let rows = block_on(conn.query(&all(1))).unwrap().0;
    assert_eq!(rows[0].version, 1);
    assert_eq!(rows[0].fields, vec![(0, text("a"))]);
}

#[test]
fn update_changes_only_listed_fields_and_bumps_version() {
    let mut conn = connect();
    commit(
        &mut conn,
        &[insert(1, 1, vec![(0, text("a")), (1, Value::Int(1))])],
    );
    commit(
        &mut conn,
        &[
            update(1, 1, 1, vec![(1, Value::Int(2))]),
            update(1, 1, 2, vec![]),
        ],
    );
    let rows = block_on(conn.query(&all(1))).unwrap().0;
    assert_eq!(rows[0].version, 3);
    assert_eq!(rows[0].fields, vec![(0, text("a")), (1, Value::Int(2))]);
}

#[test]
fn values_round_trip_unchanged() {
    let mut conn = connect();
    let values = vec![
        (0, Value::Null),
        (1, Value::Bool(true)),
        (2, Value::Int(i64::MIN)),
        (3, Value::Int(i64::MAX)),
        (4, text("\u{0}\u{1}'; DROP TABLE x; --\"\\%_\u{10FFFF}")),
        (5, Value::Text("x".repeat(1 << 20))),
    ];
    commit(&mut conn, &[insert(1, u128::MAX, values.clone())]);
    let rows = block_on(conn.query(&all(1))).unwrap().0;
    assert_eq!(rows[0].id, RowId(u128::MAX));
    assert_eq!(rows[0].fields, values);
}

#[test]
fn transaction_isolation_and_drop_is_rollback() {
    let driver = MemoryDriver::new();
    let mut a = block_on(driver.connect("memory:")).unwrap();
    let mut b = block_on(driver.connect("memory:x")).unwrap();
    block_on(async {
        let mut tx = a.begin().await.unwrap();
        tx.apply(&[insert(1, 1, vec![])]).await.unwrap();
        assert_eq!(tx.query(&all(1)).await.unwrap().0.len(), 1);
        assert!(b.query(&all(1)).await.unwrap().0.is_empty());
        drop(tx);
        assert!(a.query(&all(1)).await.unwrap().0.is_empty());

        let mut tx = a.begin().await.unwrap();
        tx.apply(&[insert(1, 1, vec![])]).await.unwrap();
        tx.commit().await.unwrap();
        // Connections of one driver share the database.
        assert_eq!(ids(b.query(&all(1)).await.unwrap()), vec![1]);
    });
    // Another driver has its own database.
    assert!(block_on(connect().query(&all(1))).unwrap().0.is_empty());
}

#[test]
fn first_committer_wins_between_connections() {
    let driver = MemoryDriver::new();
    let mut a = block_on(driver.connect("memory:")).unwrap();
    let mut b = block_on(driver.connect("memory:")).unwrap();
    block_on(async {
        let mut ta = a.begin().await.unwrap();
        let mut tb = b.begin().await.unwrap();
        ta.apply(&[insert(1, 1, vec![])]).await.unwrap();
        tb.apply(&[insert(1, 2, vec![])]).await.unwrap();
        ta.commit().await.unwrap();
        assert!(matches!(tb.commit().await, Err(DbError::Backend(_))));
        assert_eq!(ids(a.query(&all(1)).await.unwrap()), vec![1]);

        // A transaction that only read commits even after a concurrent change.
        let mut ta = a.begin().await.unwrap();
        ta.query(&all(1)).await.unwrap();
        let mut tb = b.begin().await.unwrap();
        tb.apply(&[insert(1, 3, vec![])]).await.unwrap();
        tb.commit().await.unwrap();
        assert_eq!(ta.commit().await, Ok(()));
        assert_eq!(ids(a.query(&all(1)).await.unwrap()), vec![1, 3]);
    });
}

#[test]
fn failed_transaction_refuses_everything_but_rollback() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        // The first write succeeds, the second fails: nothing may be committed.
        let ws = [insert(1, 1, vec![]), update(1, 7, 1, vec![])];
        assert_eq!(tx.apply(&ws).await, Err(DbError::NotFound));
        assert!(matches!(tx.query(&all(1)).await, Err(DbError::Backend(_))));
        assert!(matches!(
            tx.next_in_sequence("k").await,
            Err(DbError::Backend(_))
        ));
        assert!(matches!(tx.commit().await, Err(DbError::Backend(_))));
        assert!(conn.query(&all(1)).await.unwrap().0.is_empty());

        let mut tx = conn.begin().await.unwrap();
        assert_eq!(
            tx.apply(&[update(1, 7, 1, vec![])]).await,
            Err(DbError::NotFound)
        );
        assert_eq!(tx.rollback().await, Ok(()));
    });
}

#[test]
fn query_order_ties_limits_and_models() {
    let mut conn = connect();
    commit(
        &mut conn,
        &[
            insert(1, 3, vec![(0, Value::Int(1)), (1, text("b"))]),
            insert(1, 1, vec![(0, Value::Int(2)), (1, text("a"))]),
            insert(1, 2, vec![(0, Value::Int(1)), (1, text("a"))]),
            insert(2, 9, vec![(0, Value::Int(0))]),
        ],
    );
    let mut q = |order, limit| block_on(conn.query(&query(1, order, limit))).map(ids);
    assert_eq!(q(vec![], None), Ok(vec![1, 2, 3]));
    assert_eq!(q(vec![(0, Dir::Asc)], None), Ok(vec![2, 3, 1]));
    assert_eq!(q(vec![(0, Dir::Desc)], Some(2)), Ok(vec![1, 3]));
    assert_eq!(
        q(vec![(0, Dir::Asc), (1, Dir::Desc)], None),
        Ok(vec![3, 2, 1])
    );
    assert_eq!(
        q(vec![(1, Dir::Asc), (0, Dir::Desc)], None),
        Ok(vec![1, 2, 3])
    );
    assert_eq!(q(vec![], Some(0)), Ok(vec![]));
    assert_eq!(block_on(conn.query(&all(7))).map(ids), Ok(vec![]));
}

#[test]
fn text_orders_by_code_point() {
    let mut conn = connect();
    // UTF-16 unit order would put U+1F600 before U+FF01 (D50).
    let words = [
        "\u{1F600}",
        "\u{FF01}",
        "a",
        "",
        "\u{0}",
        "\u{1}",
        "ab",
        "B",
    ];
    let rows: Vec<Write> = (0u128..)
        .zip(words)
        .map(|(i, w)| insert(1, i, vec![(0, text(w))]))
        .collect();
    commit(&mut conn, &rows);
    let got = block_on(conn.query(&query(1, vec![(0, Dir::Asc)], None))).unwrap();
    assert_eq!(ids(got), vec![3, 4, 5, 7, 2, 6, 1, 0]);
}

#[test]
fn null_and_missing_sort_first_then_bool_int_text() {
    let mut conn = connect();
    commit(
        &mut conn,
        &[
            insert(1, 1, vec![(0, text("a"))]),
            insert(1, 2, vec![(0, Value::Int(-1))]),
            insert(1, 3, vec![(0, Value::Bool(true))]),
            insert(1, 4, vec![(0, Value::Null)]),
            insert(1, 5, vec![]),
            insert(1, 6, vec![(0, Value::Bool(false))]),
        ],
    );
    let mut q = |dir| block_on(conn.query(&query(1, vec![(0, dir)], None))).map(ids);
    assert_eq!(q(Dir::Asc), Ok(vec![4, 5, 6, 3, 2, 1]));
    assert_eq!(q(Dir::Desc), Ok(vec![1, 2, 3, 6, 5, 4]));
}

#[test]
fn op_log_positions_boundaries_and_rollback() {
    let mut conn = connect();
    let op = |row| StoredOp {
        seq: None,
        model: 1,
        row: RowId(row),
        payload: vec![row as u8],
    };
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        assert_eq!(tx.append_ops(&[]).await, Ok(ServerSeq(0)));
        assert_eq!(tx.ops_since(ServerSeq(0), 10).await, Ok(vec![]));
        assert_eq!(tx.append_ops(&[op(1), op(2)]).await, Ok(ServerSeq(2)));
        // A position given by the caller is replaced by the driver's.
        let given = StoredOp {
            seq: Some(ServerSeq(99)),
            ..op(3)
        };
        assert_eq!(tx.append_ops(&[given]).await, Ok(ServerSeq(3)));
        tx.commit().await.unwrap();

        let mut tx = conn.begin().await.unwrap();
        let seqs = |ops: Vec<StoredOp>| ops.iter().map(|o| o.seq.map(|s| s.0)).collect::<Vec<_>>();
        let all_ops = tx.ops_since(ServerSeq(0), 10).await.unwrap();
        assert_eq!(seqs(all_ops.clone()), vec![Some(1), Some(2), Some(3)]);
        assert_eq!(all_ops[2].payload, vec![3]);
        assert_eq!(
            seqs(tx.ops_since(ServerSeq(1), 1).await.unwrap()),
            vec![Some(2)]
        );
        assert_eq!(tx.ops_since(ServerSeq(0), 0).await, Ok(vec![]));
        assert_eq!(tx.ops_since(ServerSeq(3), 10).await, Ok(vec![]));
        assert_eq!(
            tx.ops_since(ServerSeq(u64::MAX), u32::MAX).await,
            Ok(vec![])
        );
        assert_eq!(tx.append_ops(&[op(4)]).await, Ok(ServerSeq(4)));
        tx.rollback().await.unwrap();

        let mut tx = conn.begin().await.unwrap();
        assert_eq!(tx.append_ops(&[]).await, Ok(ServerSeq(3)));
        tx.rollback().await.unwrap();
    });
}

#[test]
fn sequences_are_per_key_and_rolled_back_numbers_may_return() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        assert_eq!(tx.next_in_sequence("Team/1/key").await, Ok(1));
        assert_eq!(tx.next_in_sequence("Team/1/key").await, Ok(2));
        assert_eq!(tx.next_in_sequence("Team/2/key").await, Ok(1));
        assert_eq!(tx.next_in_sequence("").await, Ok(1));
        tx.commit().await.unwrap();

        let mut tx = conn.begin().await.unwrap();
        assert_eq!(tx.next_in_sequence("Team/1/key").await, Ok(3));
        tx.rollback().await.unwrap();

        let mut tx = conn.begin().await.unwrap();
        assert_eq!(tx.next_in_sequence("Team/1/key").await, Ok(3));
        tx.commit().await.unwrap();
    });
}
