//! Shows that the contract in `ostrel_db::api` can be implemented by a crate outside
//! `ostrel_db`, is object safe, and that the documented rules hold for a minimal driver.
//!
//! The driver here is a test fixture only. The in memory driver for users lives in
//! `ostrel_db_memory`.

use std::collections::BTreeMap;
use std::future::Future;
use std::task::{Context, Poll, Waker};

use ostrel_db::api::{
    BoxFuture, Capabilities, Connection, DbError, Dir, Driver, FieldId, MigrationPlan, ModelId,
    Query, Row, RowId, Rows, ServerSeq, StoredOp, Transaction, Value, Write,
};

#[derive(Clone)]
struct Stored {
    model: ModelId,
    version: u64,
    deleted: bool,
    fields: BTreeMap<FieldId, Value>,
}

#[derive(Default, Clone)]
struct State {
    rows: BTreeMap<RowId, Stored>,
    ops: Vec<StoredOp>,
    seqs: BTreeMap<String, u64>,
}

struct FixtureDriver;
struct FixtureConn {
    state: State,
}
struct FixtureTx<'c> {
    conn: &'c mut FixtureConn,
    work: State,
}

impl Driver for FixtureDriver {
    fn name(&self) -> &'static str {
        "fixture"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transactions: true,
            sequences: true,
            subqueries: false,
            json_fields: false,
            max_in_list: 1000,
        }
    }
    fn connect<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Box<dyn Connection>, DbError>> {
        Box::pin(async move {
            if url != "fixture:" {
                return Err(DbError::Unsupported("url scheme"));
            }
            Ok(Box::new(FixtureConn {
                state: State::default(),
            }) as Box<dyn Connection>)
        })
    }
}

fn compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Text(x), Value::Text(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

fn scan(s: &State, q: &Query) -> Rows {
    let mut rows: Vec<Row> = s
        .rows
        .iter()
        .filter(|(_, r)| r.model == q.model && !r.deleted)
        .map(|(id, r)| Row {
            id: *id,
            version: r.version,
            fields: r.fields.iter().map(|(f, v)| (*f, v.clone())).collect(),
        })
        .collect();
    let field = |r: &Row, f: FieldId| {
        r.fields
            .iter()
            .find(|(k, _)| *k == f)
            .map(|(_, v)| v.clone())
    };
    let tie = q.order.last().map_or(Dir::Asc, |(_, d)| *d);
    rows.sort_by(|a, b| {
        for (f, dir) in &q.order {
            let (x, y) = (
                field(a, *f).unwrap_or(Value::Null),
                field(b, *f).unwrap_or(Value::Null),
            );
            let o = compare(&x, &y);
            let o = if *dir == Dir::Desc { o.reverse() } else { o };
            if o.is_ne() {
                return o;
            }
        }
        if tie == Dir::Desc {
            b.id.cmp(&a.id)
        } else {
            a.id.cmp(&b.id)
        }
    });
    rows.truncate(q.limit.map_or(usize::MAX, |n| n as usize));
    Rows(rows)
}

fn apply_one(s: &mut State, w: &Write) -> Result<(), DbError> {
    match w {
        Write::Insert { model, row, fields } => {
            if s.rows.contains_key(row) {
                return Err(DbError::Conflict);
            }
            let fields = fields.iter().cloned().collect();
            s.rows.insert(
                *row,
                Stored {
                    model: *model,
                    version: 1,
                    deleted: false,
                    fields,
                },
            );
        }
        Write::Update {
            model,
            row,
            expect_version,
            fields,
        } => {
            let r = live(s, *model, *row)?;
            if r.version != *expect_version {
                return Err(DbError::VersionMismatch);
            }
            r.version += 1;
            r.fields.extend(fields.iter().cloned());
        }
        Write::Delete {
            model,
            row,
            expect_version,
        } => {
            let r = live(s, *model, *row)?;
            if r.version != *expect_version {
                return Err(DbError::VersionMismatch);
            }
            r.version += 1;
            r.deleted = true;
            r.fields.clear();
        }
    }
    Ok(())
}

fn live(s: &mut State, model: ModelId, row: RowId) -> Result<&mut Stored, DbError> {
    s.rows
        .get_mut(&row)
        .filter(|r| r.model == model && !r.deleted)
        .ok_or(DbError::NotFound)
}

impl Connection for FixtureConn {
    fn migrate<'a>(&'a mut self, _p: &'a MigrationPlan) -> BoxFuture<'a, Result<(), DbError>> {
        Box::pin(async { Ok(()) })
    }
    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move { Ok(scan(&self.state, q)) })
    }
    fn begin<'c>(&'c mut self) -> BoxFuture<'c, Result<Box<dyn Transaction<'c> + 'c>, DbError>> {
        Box::pin(async move {
            let work = self.state.clone();
            Ok(Box::new(FixtureTx { conn: self, work }) as Box<dyn Transaction<'c> + 'c>)
        })
    }
}

impl<'c> Transaction<'c> for FixtureTx<'c> {
    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move { Ok(scan(&self.work, q)) })
    }
    fn apply<'a>(&'a mut self, ws: &'a [Write]) -> BoxFuture<'a, Result<(), DbError>> {
        Box::pin(async move { ws.iter().try_for_each(|w| apply_one(&mut self.work, w)) })
    }
    fn append_ops<'a>(
        &'a mut self,
        ops: &'a [StoredOp],
    ) -> BoxFuture<'a, Result<ServerSeq, DbError>> {
        Box::pin(async move {
            for op in ops {
                let seq = ServerSeq(self.work.ops.len() as u64 + 1);
                self.work.ops.push(StoredOp {
                    seq: Some(seq),
                    ..op.clone()
                });
            }
            Ok(ServerSeq(self.work.ops.len() as u64))
        })
    }
    fn ops_since<'a>(
        &'a mut self,
        after: ServerSeq,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<StoredOp>, DbError>> {
        Box::pin(async move {
            let ops = self.work.ops.iter().filter(|o| o.seq > Some(after));
            Ok(ops.take(limit as usize).cloned().collect())
        })
    }
    fn next_in_sequence<'a>(&'a mut self, key: &'a str) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async move {
            let n = self.work.seqs.entry(key.to_string()).or_insert(0);
            *n += 1;
            Ok(*n)
        })
    }
    fn commit(self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>> {
        Box::pin(async move {
            let tx = *self;
            tx.conn.state = tx.work;
            Ok(())
        })
    }
    fn rollback(self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>> {
        Box::pin(async move {
            drop(self);
            Ok(())
        })
    }
}

/// Runs a future that never waits. Every future of the fixture driver is ready on first poll.
fn block_on<T>(f: impl Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("fixture future was not ready"),
    }
}

fn connect() -> Box<dyn Connection> {
    let registry: Vec<Box<dyn Driver>> = vec![Box::new(FixtureDriver)];
    block_on(registry[0].connect("fixture:")).unwrap()
}

fn all(model: ModelId) -> Query {
    Query {
        model,
        order: vec![],
        limit: None,
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

#[test]
fn driver_selection_refuses_unknown_url() {
    let d: Box<dyn Driver> = Box::new(FixtureDriver);
    assert_eq!(d.name(), "fixture");
    assert!(matches!(
        block_on(d.connect("postgres://x")),
        Err(DbError::Unsupported(_))
    ));
    assert!(matches!(
        block_on(d.connect("")),
        Err(DbError::Unsupported(_))
    ));
}

#[test]
fn insert_update_commit_rollback_through_dyn_registry() {
    let mut conn = connect();
    block_on(async {
        conn.migrate(&MigrationPlan::default()).await.unwrap();
        let r = RowId(7);
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[insert(1, 7, vec![(0, text("a"))])])
            .await
            .unwrap();
        assert_eq!(
            tx.apply(&[insert(1, 7, vec![])]).await,
            Err(DbError::Conflict)
        );
        tx.commit().await.unwrap();

        let mut tx = conn.begin().await.unwrap();
        let stale = Write::Update {
            model: 1,
            row: r,
            expect_version: 9,
            fields: vec![],
        };
        assert_eq!(tx.apply(&[stale]).await, Err(DbError::VersionMismatch));
        tx.rollback().await.unwrap();

        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[Write::Delete {
            model: 1,
            row: r,
            expect_version: 1,
        }])
        .await
        .unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(conn.query(&all(1)).await.unwrap().0.len(), 1);
    });
}

#[test]
fn deleted_id_is_never_reused_in_any_model() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[insert(1, 5, vec![])]).await.unwrap();
        // An id is unique across models, not only within one.
        assert_eq!(
            tx.apply(&[insert(2, 5, vec![])]).await,
            Err(DbError::Conflict)
        );
        tx.apply(&[Write::Delete {
            model: 1,
            row: RowId(5),
            expect_version: 1,
        }])
        .await
        .unwrap();
        assert_eq!(
            tx.apply(&[insert(1, 5, vec![])]).await,
            Err(DbError::Conflict)
        );
        assert_eq!(
            tx.apply(&[insert(2, 5, vec![])]).await,
            Err(DbError::Conflict)
        );
        tx.commit().await.unwrap();
        assert!(conn.query(&all(1)).await.unwrap().0.is_empty());
    });
}

#[test]
fn update_and_delete_of_missing_or_dead_rows_are_not_found() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        let upd = |model, v| Write::Update {
            model,
            row: RowId(3),
            expect_version: v,
            fields: vec![],
        };
        let del = |model, v| Write::Delete {
            model,
            row: RowId(3),
            expect_version: v,
        };
        assert_eq!(tx.apply(&[upd(1, 1)]).await, Err(DbError::NotFound));
        assert_eq!(tx.apply(&[del(1, 1)]).await, Err(DbError::NotFound));
        tx.apply(&[insert(1, 3, vec![])]).await.unwrap();
        // Right id, wrong model.
        assert_eq!(tx.apply(&[upd(2, 1)]).await, Err(DbError::NotFound));
        assert_eq!(tx.apply(&[del(2, 1)]).await, Err(DbError::NotFound));
        tx.apply(&[del(1, 1)]).await.unwrap();
        // A deleted row is not found, even with its current version.
        assert_eq!(tx.apply(&[upd(1, 2)]).await, Err(DbError::NotFound));
        assert_eq!(tx.apply(&[del(1, 2)]).await, Err(DbError::NotFound));
        tx.rollback().await.unwrap();
    });
}

#[test]
fn update_changes_only_listed_fields_and_bumps_version() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[insert(1, 1, vec![(0, text("a")), (1, Value::Int(1))])])
            .await
            .unwrap();
        let w = Write::Update {
            model: 1,
            row: RowId(1),
            expect_version: 1,
            fields: vec![(1, Value::Int(2))],
        };
        tx.apply(&[w]).await.unwrap();
        // An update without fields still bumps the version.
        let w = Write::Update {
            model: 1,
            row: RowId(1),
            expect_version: 2,
            fields: vec![],
        };
        tx.apply(&[w]).await.unwrap();
        tx.commit().await.unwrap();
        let rows = conn.query(&all(1)).await.unwrap().0;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].version, 3);
        assert_eq!(rows[0].fields, vec![(0, text("a")), (1, Value::Int(2))]);
    });
}

#[test]
fn transaction_sees_own_writes_connection_sees_only_committed() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[insert(1, 1, vec![])]).await.unwrap();
        assert_eq!(tx.query(&all(1)).await.unwrap().0.len(), 1);
        drop(tx); // dropping without commit is a rollback
        assert!(conn.query(&all(1)).await.unwrap().0.is_empty());
    });
}

#[test]
fn query_order_ties_and_limits() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[
            insert(1, 3, vec![(0, Value::Int(1))]),
            insert(1, 1, vec![(0, Value::Int(2))]),
            insert(1, 2, vec![(0, Value::Int(1))]),
            insert(2, 9, vec![(0, Value::Int(0))]),
        ])
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let ids = |rows: Rows| rows.0.iter().map(|r| r.id.0).collect::<Vec<_>>();

        assert_eq!(ids(conn.query(&all(1)).await.unwrap()), vec![1, 2, 3]);
        let asc = Query {
            model: 1,
            order: vec![(0, Dir::Asc)],
            limit: None,
        };
        assert_eq!(ids(conn.query(&asc).await.unwrap()), vec![2, 3, 1]);
        let desc = Query {
            model: 1,
            order: vec![(0, Dir::Desc)],
            limit: Some(2),
        };
        assert_eq!(ids(conn.query(&desc).await.unwrap()), vec![1, 3]);
        let none = Query {
            model: 1,
            order: vec![],
            limit: Some(0),
        };
        assert!(conn.query(&none).await.unwrap().0.is_empty());
        assert!(conn.query(&all(7)).await.unwrap().0.is_empty());
    });
}

#[test]
fn op_log_positions_and_boundaries() {
    let mut conn = connect();
    block_on(async {
        let op = |row| StoredOp {
            seq: None,
            model: 1,
            row: RowId(row),
            payload: vec![1, 2],
        };
        let mut tx = conn.begin().await.unwrap();
        assert_eq!(tx.append_ops(&[]).await, Ok(ServerSeq(0)));
        assert_eq!(tx.ops_since(ServerSeq(0), 10).await, Ok(vec![]));
        assert_eq!(tx.append_ops(&[op(1), op(2)]).await, Ok(ServerSeq(2)));
        assert_eq!(tx.append_ops(&[]).await, Ok(ServerSeq(2)));
        assert_eq!(tx.append_ops(&[op(3)]).await, Ok(ServerSeq(3)));

        let seqs = |ops: Vec<StoredOp>| ops.iter().map(|o| o.seq.map(|s| s.0)).collect::<Vec<_>>();
        assert_eq!(
            seqs(tx.ops_since(ServerSeq(0), 10).await.unwrap()),
            vec![Some(1), Some(2), Some(3)]
        );
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
        assert_eq!(
            tx.ops_since(ServerSeq(2), 10).await.unwrap()[0].payload,
            vec![1, 2]
        );
        tx.commit().await.unwrap();
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
fn errors_display_without_panicking() {
    let errs = [
        DbError::Conflict,
        DbError::VersionMismatch,
        DbError::NotFound,
        DbError::Unsupported("x"),
        DbError::Backend(String::new()),
    ];
    for e in errs {
        assert!(!e.to_string().is_empty());
    }
}
