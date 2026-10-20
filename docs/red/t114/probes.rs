//! Red review T147: probes against the SQLite driver of T114 (AC-31, D20).
//!
//! Not part of any crate. To run: copy to crates/ostrel_db_sqlite/tests/ on a checkout of
//! 5e105a4 and run `cargo test -p ostrel_db_sqlite --test probes`. Expected: 5 passed.

use std::future::Future;
use std::path::PathBuf;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use ostrel_db::api::{
    CollectionChange, Connection, DbError, Driver, Hlc, MapEntry, OpId, Query, ReplicaId, RowId,
    SetChange, Value, Write,
};
use ostrel_db_sqlite::SqliteDriver;

fn block_on<T>(f: impl Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("not ready"),
    }
}

fn fresh(name: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("probe_t147_{name}.db"));
    for s in ["", "-wal", "-shm", "-journal"] {
        let mut q = p.clone().into_os_string();
        q.push(s);
        let _ = std::fs::remove_file(q);
    }
    p
}

fn open(p: &std::path::Path) -> Box<dyn Connection> {
    let d = SqliteDriver::with_busy_timeout(Duration::from_millis(50));
    block_on(d.connect(&format!("sqlite:{}", p.display()))).unwrap()
}

fn rid(n: u128) -> RowId {
    RowId::from_hex(&format!("{n:032x}")).unwrap()
}
fn tag(r: u64, s: u32) -> OpId {
    OpId { replica: ReplicaId(r), seq: s }
}
fn text(s: &str) -> Value {
    Value::Text(s.into())
}

fn ins_full(model: u32, row: u128) -> Write {
    Write::Insert {
        model,
        row: rid(row),
        fields: vec![(0, text("new"))],
        collections: vec![
            (1, CollectionChange::Set(vec![SetChange::Add { elem: text("e"), tag: tag(9, 1) }])),
            (
                2,
                CollectionChange::Map(vec![MapEntry {
                    key: text("k"),
                    value: Some(text("v")),
                    hlc: Hlc::from_hex("00000000000000010000000000000009").unwrap(),
                }]),
            ),
        ],
    }
}
fn ins(model: u32, row: u128) -> Write {
    Write::Insert { model, row: rid(row), fields: vec![(0, text("old"))], collections: vec![] }
}
fn del(model: u32, row: u128, v: u64) -> Write {
    Write::Delete { model, row: rid(row), expect_version: v }
}
fn upd_full(model: u32, row: u128, v: u64) -> Write {
    Write::Update {
        model,
        row: rid(row),
        expect_version: v,
        fields: vec![(0, text("new"))],
        collections: vec![(
            1,
            CollectionChange::Set(vec![SetChange::Add { elem: text("e"), tag: tag(9, 2) }]),
        )],
    }
}

fn apply(c: &mut Box<dyn Connection>, w: &[Write]) -> Result<(), DbError> {
    block_on(async {
        let mut tx = c.begin().await?;
        match tx.apply(w).await {
            Ok(()) => tx.commit().await,
            Err(e) => {
                tx.rollback().await.unwrap();
                Err(e)
            }
        }
    })
}

fn count(p: &std::path::Path) -> (i64, i64, i64, i64, i64) {
    let c = rusqlite::Connection::open(p).unwrap();
    let n = |t: &str| -> i64 { c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0)).unwrap() };
    (
        n("ostrel_rows"),
        n("ostrel_fields"),
        n("ostrel_collections"),
        n("ostrel_set_tags"),
        n("ostrel_map_entries"),
    )
}

fn live(c: &mut Box<dyn Connection>, m: u32) -> usize {
    block_on(c.query(&Query::all(m))).unwrap().0.len()
}

#[test]
fn p1_tombstone_any_model_with_collections_leaves_no_trace() {
    let p = fresh("p1");
    let mut c = open(&p);
    apply(&mut c, &[ins(1, 7)]).unwrap();
    apply(&mut c, &[del(1, 7, 1)]).unwrap();
    let before = count(&p);
    for m in [1, 2, 3] {
        assert_eq!(apply(&mut c, &[ins_full(m, 7)]), Err(DbError::Conflict));
        assert_eq!(apply(&mut c, &[upd_full(m, 7, 2)]), Err(DbError::NotFound));
        assert_eq!(apply(&mut c, &[upd_full(m, 7, 1)]), Err(DbError::NotFound));
        assert_eq!(apply(&mut c, &[del(m, 7, 2)]), Err(DbError::NotFound));
        assert_eq!(live(&mut c, m), 0);
    }
    assert_eq!(count(&p), before);
    drop(c);
    let mut c = open(&p);
    for m in [1, 2] {
        assert_eq!(apply(&mut c, &[ins_full(m, 7)]), Err(DbError::Conflict));
    }
    assert_eq!(count(&p), before);
}

#[test]
fn p2_live_row_of_other_model_is_unchanged() {
    let p = fresh("p2");
    let mut c = open(&p);
    apply(&mut c, &[ins(1, 7)]).unwrap();
    let before = count(&p);
    assert_eq!(apply(&mut c, &[ins_full(2, 7)]), Err(DbError::Conflict));
    assert_eq!(apply(&mut c, &[upd_full(2, 7, 1)]), Err(DbError::NotFound));
    assert_eq!(apply(&mut c, &[del(2, 7, 1)]), Err(DbError::NotFound));
    assert_eq!(count(&p), before);
    let rows = block_on(c.query(&Query::all(1))).unwrap().0;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].version, 1);
    assert_eq!(rows[0].fields, vec![(0, text("old"))]);
}

#[test]
fn p3_insert_delete_insert_in_one_batch_and_across_calls() {
    let p = fresh("p3");
    let mut c = open(&p);
    assert_eq!(apply(&mut c, &[ins(1, 7), del(1, 7, 1), ins_full(2, 7)]), Err(DbError::Conflict));
    assert_eq!(count(&p), (0, 0, 0, 0, 0));
    block_on(async {
        let mut tx = c.begin().await.unwrap();
        tx.apply(&[ins(1, 8)]).await.unwrap();
        tx.apply(&[del(1, 8, 1)]).await.unwrap();
        assert_eq!(tx.apply(&[ins_full(1, 8)]).await, Err(DbError::Conflict));
        assert!(tx.commit().await.is_err());
    });
    assert_eq!(count(&p), (0, 0, 0, 0, 0));
    // Ids of rolled back batches stay free.
    apply(&mut c, &[ins(1, 7), ins(1, 8)]).unwrap();
}

#[test]
fn p4_two_connections_same_id() {
    let p = fresh("p4");
    let mut a = open(&p);
    let mut b = open(&p);
    block_on(async {
        let mut ta = a.begin().await.unwrap();
        ta.apply(&[ins(1, 7)]).await.unwrap();
        // b cannot even begin while a holds the write lock.
        assert!(matches!(b.begin().await.err(), Some(DbError::Backend(_))));
        ta.commit().await.unwrap();
    });
    assert_eq!(apply(&mut b, &[ins_full(2, 7)]), Err(DbError::Conflict));
    apply(&mut a, &[del(1, 7, 1)]).unwrap();
    assert_eq!(apply(&mut b, &[ins_full(1, 7)]), Err(DbError::Conflict));
    assert_eq!(live(&mut b, 1), 0);
}

#[test]
fn p5_extreme_ids() {
    let p = fresh("p5");
    let mut c = open(&p);
    for id in [0u128, 1, u128::MAX, 1 << 64, (1 << 64) - 1] {
        apply(&mut c, &[ins(1, id)]).unwrap();
        apply(&mut c, &[del(1, id, 1)]).unwrap();
        assert_eq!(apply(&mut c, &[ins(2, id)]), Err(DbError::Conflict), "id {id}");
    }
}
