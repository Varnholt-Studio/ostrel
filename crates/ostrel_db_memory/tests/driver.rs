//! Behaviour of the in memory driver against the rules of the contract in `ostrel_db::api`.
//!
//! The adapter independent conformance suite lives in `tests/db-conformance/`; these tests cover
//! the same rules at the Rust level plus what is specific to this driver (URL, shared database,
//! first committer wins, failed transactions, order of mixed and missing values, the applied
//! schema shared by all connections).

use std::future::Future;
use std::task::{Context, Poll, Waker};

use ostrel_db::api::{
    AppliedSchema, CmpOp, CollectionChange, CollectionState, Connection, Cursor, DbError, Dir,
    Driver, EnumColumn, Expr, FieldId, Hlc, Hop, MapEntry, MigrationPlan, ModelId, NewOp, OpId,
    Path, Query, ReplicaId, RowId, Rows, SchemaHash, ServerSeq, SetChange, SetTag, StoredOp, Value,
    Write,
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
        order,
        limit,
        ..Query::all(model)
    }
}

fn filtered(model: ModelId, filter: Expr) -> Query {
    Query {
        filter: Some(filter),
        ..Query::all(model)
    }
}

/// The row id with the raw value `n`. Ids have no constructor from a raw `u128`, so the test
/// goes through the wire form.
fn rid(n: u128) -> RowId {
    RowId::from_hex(&format!("{n:032x}")).unwrap()
}

/// Range of `Int` (ARCHITECTURE 7.4).
const INT_MAX: i64 = (1 << 53) - 1;
const INT_MIN: i64 = -INT_MAX;

fn int(v: i64) -> Value {
    Value::int(v).unwrap()
}

fn en(name: &str) -> Value {
    Value::Enum(name.to_string())
}

fn field(f: FieldId) -> Box<Expr> {
    Box::new(Expr::Field(Path {
        hops: vec![],
        field: f,
    }))
}

fn konst(v: Value) -> Box<Expr> {
    Box::new(Expr::Const(v))
}

fn plan(from: Option<u8>, to: u8) -> MigrationPlan {
    MigrationPlan {
        from: from.map(|b| SchemaHash([b; 32])),
        to: SchemaHash([to; 32]),
        schema: format!("{{\"v\":{to}}}"),
        enums: vec![],
        steps: vec![],
    }
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

/// Model 1 field 0 is an enum (backlog, todo, done) whose declaration order differs from the
/// name order; model 1 field 3 is a `Map` with enum values (guest, member, admin).
fn enum_plan() -> MigrationPlan {
    MigrationPlan {
        enums: vec![
            EnumColumn {
                model: 1,
                field: 0,
                variants: strings(&["backlog", "todo", "done"]),
            },
            EnumColumn {
                model: 1,
                field: 3,
                variants: strings(&["guest", "member", "admin"]),
            },
        ],
        ..plan(None, 1)
    }
}

fn tag(replica: u64, seq: u32) -> OpId {
    OpId {
        replica: ReplicaId(replica),
        seq,
    }
}

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn insert(model: ModelId, row: u128, fields: Vec<(FieldId, Value)>) -> Write {
    Write::Insert {
        model,
        row: rid(row),
        fields,
        collections: vec![],
    }
}

fn update(model: ModelId, row: u128, v: u64, fields: Vec<(FieldId, Value)>) -> Write {
    Write::Update {
        model,
        row: rid(row),
        expect_version: v,
        fields,
        collections: vec![],
    }
}

fn delete(model: ModelId, row: u128, v: u64) -> Write {
    Write::Delete {
        model,
        row: rid(row),
        expect_version: v,
    }
}

fn ids(rows: Rows) -> Vec<u128> {
    rows.0.iter().map(|r| r.id.as_u128()).collect()
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
    assert_eq!(block_on(conn.migrate(&plan(None, 1))), Ok(()));
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
        &[insert(1, 1, vec![(0, text("a")), (1, int(1))])],
    );
    commit(
        &mut conn,
        &[update(1, 1, 1, vec![(1, int(2))]), update(1, 1, 2, vec![])],
    );
    let rows = block_on(conn.query(&all(1))).unwrap().0;
    assert_eq!(rows[0].version, 3);
    assert_eq!(rows[0].fields, vec![(0, text("a")), (1, int(2))]);
}

#[test]
fn values_round_trip_unchanged() {
    let mut conn = connect();
    let values = vec![
        (0, Value::Null),
        (1, Value::Bool(true)),
        (2, int(INT_MIN)),
        (3, int(INT_MAX)),
        (4, text("\u{0}\u{1}'; DROP TABLE x; --\"\\%_\u{10FFFF}")),
        (5, Value::Text("x".repeat(1 << 20))),
    ];
    commit(&mut conn, &[insert(1, u128::MAX, values.clone())]);
    let rows = block_on(conn.query(&all(1))).unwrap().0;
    assert_eq!(rows[0].id, rid(u128::MAX));
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
            insert(1, 3, vec![(0, int(1)), (1, text("b"))]),
            insert(1, 1, vec![(0, int(2)), (1, text("a"))]),
            insert(1, 2, vec![(0, int(1)), (1, text("a"))]),
            insert(2, 9, vec![(0, int(0))]),
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
            insert(1, 2, vec![(0, int(-1))]),
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
fn op_log_positions_boundaries_duplicates_and_rollback() {
    let mut conn = connect();
    let op = |seq: u32| NewOp {
        id: tag(1, seq),
        hlc: Hlc::new(u64::from(seq), 0, ReplicaId(1)).unwrap(),
        model: 1,
        row: rid(u128::from(seq)),
        body: vec![seq as u8],
    };
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        assert_eq!(tx.append_ops(&[]).await, Ok(ServerSeq(0)));
        assert_eq!(tx.ops_since(ServerSeq(0), 10).await, Ok(vec![]));
        assert_eq!(tx.append_ops(&[op(1), op(2)]).await, Ok(ServerSeq(2)));
        assert_eq!(tx.append_ops(&[op(3)]).await, Ok(ServerSeq(3)));
        tx.commit().await.unwrap();

        let mut tx = conn.begin().await.unwrap();
        let seqs = |ops: Vec<StoredOp>| ops.iter().map(|o| o.seq.0).collect::<Vec<_>>();
        let all_ops = tx.ops_since(ServerSeq(0), 10).await.unwrap();
        assert_eq!(seqs(all_ops.clone()), vec![1, 2, 3]);
        assert_eq!(all_ops[2].op, op(3));
        assert_eq!(seqs(tx.ops_since(ServerSeq(1), 1).await.unwrap()), vec![2]);
        assert_eq!(tx.ops_since(ServerSeq(0), 0).await, Ok(vec![]));
        assert_eq!(tx.ops_since(ServerSeq(3), 10).await, Ok(vec![]));
        assert_eq!(
            tx.ops_since(ServerSeq(u64::MAX), u32::MAX).await,
            Ok(vec![])
        );
        assert_eq!(tx.append_ops(&[op(4)]).await, Ok(ServerSeq(4)));
        tx.rollback().await.unwrap();

        // An id already in the log, or twice in one call, appends nothing of the call.
        for ops in [vec![op(5), op(2)], vec![op(5), op(6), op(5)]] {
            let mut tx = conn.begin().await.unwrap();
            assert_eq!(tx.append_ops(&ops).await, Err(DbError::Conflict));
            tx.rollback().await.unwrap();
        }
        let mut tx = conn.begin().await.unwrap();
        assert_eq!(tx.append_ops(&[]).await, Ok(ServerSeq(3)));
        // The same seq of another replica is another op.
        let other = NewOp {
            id: tag(2, 1),
            ..op(1)
        };
        assert_eq!(tx.append_ops(&[other]).await, Ok(ServerSeq(4)));
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

#[test]
fn applied_schema_and_enum_columns_are_shared_by_all_connections() {
    let driver = MemoryDriver::new();
    let mut a = block_on(driver.connect("memory:")).unwrap();
    block_on(async {
        assert_eq!(a.applied_schema().await, Ok(None));
        assert_eq!(
            a.migrate(&plan(Some(1), 2)).await,
            Err(DbError::SchemaMismatch)
        );
        assert_eq!(a.applied_schema().await, Ok(None));
        a.migrate(&enum_plan()).await.unwrap();
        // A plan that breaks the plan rules is refused before the schema is compared.
        let mut twice = enum_plan();
        twice.from = Some(SchemaHash([1; 32]));
        twice.enums.push(twice.enums[0].clone());
        assert!(matches!(a.migrate(&twice).await, Err(DbError::Invalid(_))));
    });
    // A connection opened after the migration sees schema and enum columns (D87).
    let mut b = block_on(driver.connect("memory:")).unwrap();
    block_on(async {
        let applied = AppliedSchema {
            hash: SchemaHash([1; 32]),
            schema: "{\"v\":1}".to_string(),
        };
        assert_eq!(b.applied_schema().await, Ok(Some(applied.clone())));
        let mut tx = b.begin().await.unwrap();
        assert!(matches!(
            tx.apply(&[insert(1, 1, vec![(0, en("nope"))])]).await,
            Err(DbError::Invalid(_))
        ));
        tx.rollback().await.unwrap();

        // A no op plan changes nothing, not even with other text and no enum columns.
        let noop = MigrationPlan {
            schema: "other".to_string(),
            ..plan(Some(1), 1)
        };
        assert_eq!(b.migrate(&noop).await, Ok(()));
        assert_eq!(a.applied_schema().await, Ok(Some(applied)));
        let mut tx = a.begin().await.unwrap();
        assert!(matches!(
            tx.apply(&[insert(1, 1, vec![(0, en("nope"))])]).await,
            Err(DbError::Invalid(_))
        ));
        tx.rollback().await.unwrap();

        // The next plan replaces the enum columns.
        b.migrate(&plan(Some(1), 2)).await.unwrap();
        let mut tx = a.begin().await.unwrap();
        tx.apply(&[insert(1, 1, vec![(0, en("nope"))])])
            .await
            .unwrap();
        tx.commit().await.unwrap();
    });
}

#[test]
fn a_migration_ends_open_writing_transactions() {
    let driver = MemoryDriver::new();
    let mut a = block_on(driver.connect("memory:")).unwrap();
    let mut b = block_on(driver.connect("memory:")).unwrap();
    block_on(async {
        let mut tx = a.begin().await.unwrap();
        tx.apply(&[insert(1, 1, vec![])]).await.unwrap();
        b.migrate(&enum_plan()).await.unwrap();
        // Committing would write the copied, unmigrated schema back.
        assert!(matches!(tx.commit().await, Err(DbError::Backend(_))));
        assert!(b.applied_schema().await.unwrap().is_some());
        assert!(b.query(&all(1)).await.unwrap().0.is_empty());
    });
}

#[test]
fn rows_sort_enums_by_declaration_and_bytes_bytewise() {
    let mut conn = connect();
    block_on(conn.migrate(&enum_plan())).unwrap();
    commit(
        &mut conn,
        &[
            insert(1, 1, vec![(0, en("done")), (1, Value::Bytes(vec![0xF8]))]),
            insert(
                1,
                2,
                vec![(0, en("backlog")), (1, Value::Bytes(vec![0x00]))],
            ),
            insert(1, 3, vec![(0, en("todo")), (1, Value::Bytes(vec![]))]),
            insert(1, 4, vec![(0, Value::Null)]),
        ],
    );
    let mut by = |f, d| block_on(conn.query(&query(1, vec![(f, d)], None))).map(ids);
    assert_eq!(by(0, Dir::Asc), Ok(vec![4, 2, 3, 1]));
    assert_eq!(by(0, Dir::Desc), Ok(vec![1, 3, 2, 4]));
    assert_eq!(by(1, Dir::Asc), Ok(vec![4, 3, 2, 1]));
    assert_eq!(by(1, Dir::Desc), Ok(vec![1, 2, 3, 4]));

    let page = Query {
        after: Some(Cursor {
            values: vec![en("todo")],
            id: rid(3),
        }),
        ..query(1, vec![(0, Dir::Asc)], None)
    };
    assert_eq!(block_on(conn.query(&page)).map(ids), Ok(vec![1]));
    let ge = filtered(1, Expr::Cmp(CmpOp::Ge, field(0), konst(en("todo"))));
    assert_eq!(block_on(conn.query(&ge)).map(ids), Ok(vec![1, 3]));
    let unknown = filtered(1, Expr::Cmp(CmpOp::Ge, field(0), konst(en("nope"))));
    assert_eq!(block_on(conn.query(&unknown)).map(ids), Ok(vec![]));
    // Two enum columns with different variants cannot be compared (D87).
    let get = Box::new(Expr::MapGet {
        map: Path {
            hops: vec![],
            field: 3,
        },
        key: konst(Value::Ref(rid(9))),
    });
    let mixed = filtered(1, Expr::Cmp(CmpOp::Lt, field(0), get));
    assert!(matches!(
        block_on(conn.query(&mixed)),
        Err(DbError::Invalid(_))
    ));
}

#[test]
fn cursor_pages_return_every_row_once_also_after_the_cursor_row_is_gone() {
    let mut conn = connect();
    // Model 1 is paged ascending, model 2 descending; ids are never reused across models.
    for (model, base) in [(1, 0), (2, 100)] {
        let rows: Vec<Write> = (1..=7)
            .map(|i| insert(model, base + i, vec![(0, int((i % 3) as i64))]))
            .collect();
        commit(&mut conn, &rows);
    }
    for (model, base, dir) in [(1, 0, Dir::Asc), (2, 100, Dir::Desc)] {
        let mut seen = Vec::new();
        let mut after = None;
        loop {
            let q = Query {
                after: after.clone(),
                ..query(model, vec![(0, dir)], Some(2))
            };
            let page = block_on(conn.query(&q)).unwrap().0;
            let Some(last) = page.last() else { break };
            after = Some(Cursor {
                values: vec![last.fields[0].1.clone()],
                id: last.id,
            });
            seen.extend(page.iter().map(|r| r.id.as_u128()));
            // Deleting the row the cursor points at must not lose or repeat rows.
            commit(&mut conn, &[delete(model, last.id.as_u128(), last.version)]);
        }
        let mut expect: Vec<u128> = (1..=7).map(|i| base + i).collect();
        expect.sort_by_key(|i| ((i - base) % 3, *i));
        if dir == Dir::Desc {
            expect.reverse();
        }
        assert_eq!(seen, expect, "{dir:?}");
    }
    // A cursor with the wrong number of values is refused.
    let bad = Query {
        after: Some(Cursor {
            values: vec![],
            id: rid(1),
        }),
        ..query(1, vec![(0, Dir::Asc)], None)
    };
    assert!(matches!(
        block_on(conn.query(&bad)),
        Err(DbError::Invalid(_))
    ));
}

#[test]
fn filters_are_two_valued_and_fail_closed_on_broken_hops() {
    let mut conn = connect();
    commit(
        &mut conn,
        &[
            insert(2, 10, vec![(0, text("alice"))]),
            insert(2, 11, vec![(0, text("bob"))]),
            insert(1, 1, vec![(0, Value::Ref(rid(10))), (1, int(5))]),
            insert(1, 2, vec![(0, Value::Ref(rid(11))), (1, Value::Null)]),
            insert(1, 3, vec![(0, Value::Null), (1, int(7))]),
            insert(1, 4, vec![(0, Value::Ref(rid(12))), (1, int(9))]),
        ],
    );
    let owner = |f| {
        Box::new(Expr::Field(Path {
            hops: vec![Hop { field: 0, model: 2 }],
            field: f,
        }))
    };
    let q =
        |conn: &mut Box<dyn Connection>, e: Expr| block_on(conn.query(&filtered(1, e))).map(ids);
    assert_eq!(
        q(
            &mut conn,
            Expr::Cmp(CmpOp::Eq, owner(0), konst(text("alice")))
        ),
        Ok(vec![1])
    );
    // Not of a comparison with Null is true; a broken hop is false also under Not and Or.
    assert_eq!(
        q(
            &mut conn,
            Expr::Not(Box::new(Expr::Cmp(CmpOp::Gt, field(1), konst(int(6)))))
        ),
        Ok(vec![1, 2])
    );
    assert_eq!(
        q(
            &mut conn,
            Expr::Not(Box::new(Expr::Cmp(
                CmpOp::Eq,
                owner(0),
                konst(text("alice"))
            )))
        ),
        Ok(vec![2])
    );
    assert_eq!(
        q(
            &mut conn,
            Expr::Or(vec![
                Expr::Cmp(CmpOp::Eq, owner(0), konst(text("x"))),
                Expr::Cmp(CmpOp::Ge, field(1), konst(int(0))),
            ])
        ),
        Ok(vec![1])
    );
    assert_eq!(
        q(
            &mut conn,
            Expr::Cmp(
                CmpOp::Eq,
                Box::new(Expr::RowId(vec![Hop { field: 0, model: 2 }])),
                konst(Value::Ref(rid(11)))
            )
        ),
        Ok(vec![2])
    );
    assert_eq!(
        q(
            &mut conn,
            Expr::Cmp(
                CmpOp::Eq,
                Box::new(Expr::Coalesce(field(1), int(0))),
                konst(int(0))
            )
        ),
        Ok(vec![2])
    );
    assert_eq!(q(&mut conn, Expr::And(vec![])), Ok(vec![1, 2, 3, 4]));
    assert_eq!(q(&mut conn, Expr::Or(vec![])), Ok(vec![]));
    assert_eq!(q(&mut conn, Expr::Const(int(1))), Ok(vec![]));
    // A hostile filter is refused before it is evaluated, however deep it is nested.
    let deep = (0..200_000).fold(Expr::Const(Value::Bool(true)), |e, _| {
        Expr::Not(Box::new(e))
    });
    let mut hostile = filtered(1, deep);
    let refused = block_on(conn.query(&hostile));
    // Drop the deep filter iteratively; the derived drop recurses once per level (F1 of T69).
    let mut stack: Vec<Expr> = hostile.filter.take().into_iter().collect();
    while let Some(e) = stack.pop() {
        if let Expr::Not(inner) = e {
            stack.push(*inner);
        }
    }
    assert!(matches!(refused, Err(DbError::Invalid(_))));
}

#[test]
fn set_tags_and_map_entries_are_stored_as_the_contract_says() {
    let mut conn = connect();
    let stamp = |n| Hlc::new(n, 0, ReplicaId(1)).unwrap();
    let set = |changes| (1, CollectionChange::Set(changes));
    let add = |e: &str, r, q| SetChange::Add {
        elem: text(e),
        tag: tag(r, q),
    };
    let remove = |e: &str, r, q| SetChange::Remove {
        elem: text(e),
        tag: tag(r, q),
    };
    let entry = |k: &str, v: Option<i64>, n| MapEntry {
        key: text(k),
        value: v.map(int),
        hlc: stamp(n),
    };
    let write = |v, collections| Write::Update {
        model: 1,
        row: rid(1),
        expect_version: v,
        fields: vec![],
        collections,
    };
    commit(
        &mut conn,
        &[Write::Insert {
            model: 1,
            row: rid(1),
            fields: vec![],
            collections: vec![set(vec![add("b", 1, 1), add("a", 2, 1)])],
        }],
    );
    commit(
        &mut conn,
        &[
            // Replay of an older add of replica 1 is ignored, a newer one replaces it.
            write(1, vec![set(vec![add("b", 1, 1), add("b", 1, 3)])]),
            // A remove of the replaced tag is ignored; the live one of replica 2 goes.
            write(2, vec![set(vec![remove("b", 1, 1), remove("a", 2, 1)])]),
            write(
                3,
                vec![(
                    2,
                    CollectionChange::Map(vec![entry("y", Some(1), 1), entry("x", Some(2), 1)]),
                )],
            ),
            write(
                4,
                vec![(2, CollectionChange::Map(vec![entry("y", None, 2)]))],
            ),
        ],
    );
    let rows = block_on(conn.query(&all(1))).unwrap().0;
    assert_eq!(rows[0].version, 5);
    assert_eq!(
        rows[0].collections,
        vec![
            (
                1,
                CollectionState::Set(vec![SetTag {
                    elem: text("b"),
                    tag: tag(1, 3),
                }])
            ),
            (
                2,
                CollectionState::Map(vec![entry("x", Some(2), 1), entry("y", None, 2)])
            ),
        ]
    );
    let in_set = |e: &str| Expr::InSet {
        elem: konst(text(e)),
        set: Path {
            hops: vec![],
            field: 1,
        },
    };
    let in_map = |k: &str| Expr::InMap {
        key: konst(text(k)),
        map: Path {
            hops: vec![],
            field: 2,
        },
    };
    let mut q = |e| block_on(conn.query(&filtered(1, e))).map(ids);
    assert_eq!(q(in_set("b")), Ok(vec![1]));
    assert_eq!(q(in_set("a")), Ok(vec![]));
    assert_eq!(q(in_map("x")), Ok(vec![1]));
    assert_eq!(q(in_map("y")), Ok(vec![]));
}

#[test]
fn refused_writes_change_nothing() {
    let mut conn = connect();
    block_on(conn.migrate(&enum_plan())).unwrap();
    commit(&mut conn, &[insert(1, 1, vec![(0, en("todo"))])]);
    let entry = |v: Value| MapEntry {
        key: text("k"),
        value: Some(v),
        hlc: Hlc::new(1, 0, ReplicaId(1)).unwrap(),
    };
    let refused = [
        // Field twice in one write.
        Write::Update {
            model: 1,
            row: rid(1),
            expect_version: 1,
            fields: vec![(1, int(1)), (1, int(2))],
            collections: vec![],
        },
        // Set value in a column.
        update(1, 1, 1, vec![(1, Value::Set(vec![]))]),
        // Remove in an insert.
        Write::Insert {
            model: 1,
            row: rid(2),
            fields: vec![],
            collections: vec![(
                2,
                CollectionChange::Set(vec![SetChange::Remove {
                    elem: int(1),
                    tag: tag(1, 1),
                }]),
            )],
        },
        // Undeclared variant in an enum column and as an enum map value.
        update(1, 1, 1, vec![(0, en("nope"))]),
        update(1, 1, 1, vec![(0, text("todo"))]),
        Write::Update {
            model: 1,
            row: rid(1),
            expect_version: 1,
            fields: vec![],
            collections: vec![(3, CollectionChange::Map(vec![entry(en("owner"))]))],
        },
    ];
    for w in refused {
        block_on(async {
            let mut tx = conn.begin().await.unwrap();
            // The valid insert before the refused write is not applied either.
            let batch = [insert(1, 5, vec![]), w.clone()];
            assert!(
                matches!(tx.apply(&batch).await, Err(DbError::Invalid(_))),
                "{w:?}"
            );
            tx.rollback().await.unwrap();
        });
    }
    let rows = block_on(conn.query(&all(1))).unwrap().0;
    assert_eq!(ids(Rows(rows.clone())), vec![1]);
    assert_eq!(rows[0].version, 1);
    // Null and declared variants are accepted, also as map values; set elements are not
    // checked against the variants (D87).
    commit(
        &mut conn,
        &[Write::Update {
            model: 1,
            row: rid(1),
            expect_version: 1,
            fields: vec![(0, Value::Null)],
            collections: vec![(3, CollectionChange::Map(vec![entry(en("admin"))]))],
        }],
    );
}
