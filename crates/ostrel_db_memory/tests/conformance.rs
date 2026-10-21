//! The adapter independent conformance cases of `tests/db-conformance/` against the in memory
//! driver. Including the harness here also runs its own tests (`selftest.rs`). The evidence
//! test for AC-18 comes with the adapter guide (T71).

#[path = "../../../tests/db-conformance/harness/mod.rs"]
mod harness;

use ostrel_db::api::{Connection, DbError, Driver, Hlc, NewOp, OpId, ReplicaId, RowId, ServerSeq};
use ostrel_db_memory::MemoryDriver;

#[test]
fn shared_conformance_cases_pass_on_the_memory_driver() {
    let dir = harness::cases_dir();
    let fresh = harness::new_driver_per_case(MemoryDriver::default, "memory:");
    harness::block_on(harness::run_dir(&dir, &fresh)).assert_passed();
}

fn op(replica: u64) -> NewOp {
    NewOp {
        id: OpId {
            replica: ReplicaId(replica),
            seq: 1,
        },
        hlc: Hlc::new(10, 0, ReplicaId(replica)).unwrap(),
        model: 0,
        row: RowId::from_hex(&format!("{:032x}", 1)).unwrap(),
        body: b"{}".to_vec(),
    }
}

fn log(conn: &mut dyn Connection) -> Vec<(ServerSeq, ReplicaId)> {
    harness::block_on(async {
        let mut tx = conn.begin().await.unwrap();
        let ops = tx.ops_since(ServerSeq(0), 100).await.unwrap();
        tx.rollback().await.unwrap();
        ops.into_iter().map(|o| (o.seq, o.op.id.replica)).collect()
    })
}

/// Position contract of `append_ops` (D154) for two open transactions. The memory driver gives
/// positions from the private copy of a transaction, so both get position 1. The copy of the
/// one that commits second is stale and must not commit, or position 1 would be given twice.
#[test]
fn ac_10_append_positions_stale_copy_does_not_commit() {
    let driver = MemoryDriver::default();
    let mut a = harness::block_on(driver.connect("memory:")).unwrap();
    let mut b = harness::block_on(driver.connect("memory:")).unwrap();
    harness::block_on(async {
        let mut ta = a.begin().await.unwrap();
        let mut tb = b.begin().await.unwrap();
        assert_eq!(ta.append_ops(&[op(1)]).await, Ok(ServerSeq(1)));
        assert_eq!(tb.append_ops(&[op(2)]).await, Ok(ServerSeq(1)));
        ta.commit().await.unwrap();
        assert!(matches!(tb.commit().await, Err(DbError::Backend(_))));
    });
    assert_eq!(log(a.as_mut()), [(ServerSeq(1), ReplicaId(1))]);
    // The retry starts after the commit of `a`, so its position is above.
    harness::block_on(async {
        let mut tb = b.begin().await.unwrap();
        assert_eq!(tb.append_ops(&[op(2)]).await, Ok(ServerSeq(2)));
        tb.commit().await.unwrap();
    });
    assert_eq!(
        log(b.as_mut()),
        [(ServerSeq(1), ReplicaId(1)), (ServerSeq(2), ReplicaId(2))]
    );
}
