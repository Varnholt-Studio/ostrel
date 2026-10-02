//! Shows that the contract in `ostrel_db::api` can be implemented by a crate outside
//! `ostrel_db`, is object safe, and that the documented rules hold for a minimal driver.
//!
//! The driver here is a test fixture only. It knows enum declaration order only from
//! `MigrationPlan::enums` (D79), like every driver. The in memory driver for users lives in
//! `ostrel_db_memory`.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::future::Future;
use std::task::{Context, Poll, Waker};

use ostrel_db::api::{
    AppliedSchema, BoxFuture, Capabilities, CmpOp, CollectionChange, CollectionState, Connection,
    Cursor, DbError, Dir, Driver, EnumColumn, Expr, FieldId, Hlc, Hop, MAX_FILTER_NODES, MapEntry,
    MigrationPlan, ModelId, NewOp, OpId, Path, Query, ReplicaId, Row, RowId, Rows, SchemaHash,
    ServerSeq, SetChange, SetTag, StoredOp, Transaction, Value, Write, check_writes, compare_key,
};

#[derive(Clone)]
struct Stored {
    model: ModelId,
    version: u64,
    deleted: bool,
    fields: BTreeMap<FieldId, Value>,
    collections: BTreeMap<FieldId, CollectionState>,
}

#[derive(Default, Clone)]
struct State {
    rows: BTreeMap<RowId, Stored>,
    ops: Vec<StoredOp>,
    seqs: BTreeMap<String, u64>,
    schema: Option<AppliedSchema>,
    /// Declaration order per enum column, from the applied plan.
    enums: BTreeMap<(ModelId, FieldId), Vec<String>>,
}

impl State {
    fn variants(&self, model: ModelId, field: FieldId) -> Option<&[String]> {
        self.enums.get(&(model, field)).map(Vec::as_slice)
    }
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

/// Ordinal of an enum value in declaration order.
fn ordinal(variants: &[String], v: &Value) -> Option<usize> {
    match v {
        Value::Enum(name) => variants.iter().position(|x| x == name),
        _ => None,
    }
}

/// Column order of the contract (api query module documentation). `variants` is the
/// declaration order when the column is enum typed. Writes refuse undeclared variants, so the
/// name fallback only keeps the order total.
fn column_order(variants: Option<&[String]>, a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Bytes(x), Value::Bytes(y)) => x.cmp(y),
        (Value::Enum(_), Value::Enum(_)) => {
            match variants.map(|vs| (ordinal(vs, a), ordinal(vs, b))) {
                Some((Some(x), Some(y))) => x.cmp(&y),
                _ => compare_key(a, b),
            }
        }
        _ => compare_key(a, b),
    }
}

/// The enum column an operand of an ordered comparison names, if any.
fn enum_side<'s>(s: &'s State, model: ModelId, e: &Expr) -> Option<&'s [String]> {
    let target = |p: &Path| p.hops.last().map_or(model, |h| h.model);
    match e {
        Expr::Field(p) => s.variants(target(p), p.field),
        Expr::MapGet { map, .. } => s.variants(target(map), map.field),
        Expr::Coalesce(a, _) => enum_side(s, model, a),
        _ => None,
    }
}

/// Ordered comparison of two non null values under the contract rules.
fn ordered(
    s: &State,
    model: ModelId,
    a: &Expr,
    b: &Expr,
    x: &Value,
    y: &Value,
) -> Option<Ordering> {
    if let (Value::Enum(_), Value::Enum(_)) = (x, y) {
        let vs = enum_side(s, model, a).or_else(|| enum_side(s, model, b))?;
        return Some(ordinal(vs, x)?.cmp(&ordinal(vs, y)?));
    }
    Some(column_order(None, x, y))
}

/// Marker: a hop did not reach a live row, so the whole filter is false.
struct HopFailed;

fn follow<'s>(s: &'s State, start: &'s Stored, hops: &[Hop]) -> Result<&'s Stored, HopFailed> {
    let mut row = start;
    for hop in hops {
        let Some(Value::Ref(id)) = row.fields.get(&hop.field) else {
            return Err(HopFailed);
        };
        row = s
            .rows
            .get(id)
            .filter(|r| r.model == hop.model && !r.deleted)
            .ok_or(HopFailed)?;
    }
    Ok(row)
}

fn row_id_after(s: &State, id: RowId, hops: &[Hop]) -> Result<RowId, HopFailed> {
    let mut id = id;
    for hop in hops {
        let Some(Value::Ref(next)) = s.rows.get(&id).and_then(|r| r.fields.get(&hop.field)) else {
            return Err(HopFailed);
        };
        if !s
            .rows
            .get(next)
            .is_some_and(|r| r.model == hop.model && !r.deleted)
        {
            return Err(HopFailed);
        }
        id = *next;
    }
    Ok(id)
}

fn truthy(v: &Value) -> bool {
    *v == Value::Bool(true)
}

fn eval(s: &State, id: RowId, row: &Stored, e: &Expr) -> Result<Value, HopFailed> {
    let collection = |p: &Path| -> Result<Option<CollectionState>, HopFailed> {
        Ok(follow(s, row, &p.hops)?.collections.get(&p.field).cloned())
    };
    Ok(match e {
        Expr::Const(v) => v.clone(),
        Expr::Field(p) => follow(s, row, &p.hops)?
            .fields
            .get(&p.field)
            .cloned()
            .unwrap_or(Value::Null),
        Expr::RowId(hops) => Value::Ref(row_id_after(s, id, hops)?),
        Expr::Cmp(op, a, b) => {
            let (x, y) = (eval(s, id, row, a)?, eval(s, id, row, b)?);
            let r = match op {
                CmpOp::Eq => {
                    compare_key(&x, &y).is_eq() && (x == Value::Null) == (y == Value::Null)
                }
                CmpOp::Ne => {
                    !(compare_key(&x, &y).is_eq() && (x == Value::Null) == (y == Value::Null))
                }
                _ if x == Value::Null || y == Value::Null => false,
                _ => match ordered(s, row.model, a, b, &x, &y) {
                    None => false,
                    Some(o) => match op {
                        CmpOp::Lt => o.is_lt(),
                        CmpOp::Le => o.is_le(),
                        CmpOp::Gt => o.is_gt(),
                        _ => o.is_ge(),
                    },
                },
            };
            Value::Bool(r)
        }
        Expr::And(items) => {
            let mut all = true;
            for i in items {
                all &= truthy(&eval(s, id, row, i)?);
            }
            Value::Bool(all)
        }
        Expr::Or(items) => {
            let mut any = false;
            for i in items {
                any |= truthy(&eval(s, id, row, i)?);
            }
            Value::Bool(any)
        }
        Expr::Not(a) => Value::Bool(!truthy(&eval(s, id, row, a)?)),
        Expr::InSet { elem, set } => {
            let x = eval(s, id, row, elem)?;
            Value::Bool(match collection(set)? {
                Some(CollectionState::Set(tags)) => {
                    tags.iter().any(|t| compare_key(&t.elem, &x).is_eq())
                }
                _ => false,
            })
        }
        Expr::InMap { key, map } => {
            let k = eval(s, id, row, key)?;
            Value::Bool(map_get(collection(map)?, &k).is_some())
        }
        Expr::MapGet { map, key } => {
            let k = eval(s, id, row, key)?;
            map_get(collection(map)?, &k).unwrap_or(Value::Null)
        }
        Expr::Coalesce(a, c) => match eval(s, id, row, a)? {
            Value::Null => c.clone(),
            v => v,
        },
    })
}

fn map_get(state: Option<CollectionState>, key: &Value) -> Option<Value> {
    match state {
        Some(CollectionState::Map(entries)) => entries
            .into_iter()
            .find(|e| compare_key(&e.key, key).is_eq())
            .and_then(|e| e.value),
        _ => None,
    }
}

fn sort_values(r: &Row, order: &[(FieldId, Dir)]) -> Vec<Value> {
    order
        .iter()
        .map(|(f, _)| {
            r.fields
                .iter()
                .find(|(k, _)| k == f)
                .map_or(Value::Null, |(_, v)| v.clone())
        })
        .collect()
}

/// Query order of a row position `(values, id)`.
fn compare_pos(s: &State, q: &Query, a: (&[Value], RowId), b: (&[Value], RowId)) -> Ordering {
    let order = &q.order;
    for (i, (f, dir)) in order.iter().enumerate() {
        let o = column_order(s.variants(q.model, *f), &a.0[i], &b.0[i]);
        let o = if *dir == Dir::Desc { o.reverse() } else { o };
        if o.is_ne() {
            return o;
        }
    }
    let tie = order.last().map_or(Dir::Asc, |(_, d)| *d);
    if tie == Dir::Desc {
        b.1.cmp(&a.1)
    } else {
        a.1.cmp(&b.1)
    }
}

fn scan(s: &State, q: &Query) -> Result<Rows, DbError> {
    q.check()?;
    let mut rows: Vec<Row> = Vec::new();
    for (id, r) in &s.rows {
        if r.model != q.model || r.deleted {
            continue;
        }
        if let Some(f) = &q.filter
            && !matches!(eval(s, *id, r, f), Ok(v) if truthy(&v))
        {
            continue;
        }
        rows.push(Row {
            id: *id,
            version: r.version,
            fields: r.fields.iter().map(|(f, v)| (*f, v.clone())).collect(),
            collections: r.collections.iter().map(|(f, c)| (*f, c.clone())).collect(),
        });
    }
    let keyed: Vec<(Vec<Value>, Row)> = rows
        .into_iter()
        .map(|r| (sort_values(&r, &q.order), r))
        .collect();
    let mut keyed: Vec<(Vec<Value>, Row)> = match &q.after {
        Some(c) => keyed
            .into_iter()
            .filter(|(v, r)| compare_pos(s, q, (v, r.id), (&c.values, c.id)).is_gt())
            .collect(),
        None => keyed,
    };
    keyed.sort_by(|(va, a), (vb, b)| compare_pos(s, q, (va, a.id), (vb, b.id)));
    keyed.truncate(q.limit.map_or(usize::MAX, |n| n as usize));
    Ok(Rows(keyed.into_iter().map(|(_, r)| r).collect()))
}

fn apply_collection(stored: &mut Stored, field: FieldId, change: &CollectionChange) {
    match change {
        CollectionChange::Set(changes) => {
            let state = stored
                .collections
                .entry(field)
                .or_insert(CollectionState::Set(Vec::new()));
            let CollectionState::Set(tags) = state else {
                return;
            };
            for c in changes {
                match c {
                    SetChange::Add { elem, tag } => {
                        let same = |t: &SetTag| {
                            compare_key(&t.elem, elem).is_eq() && t.tag.replica == tag.replica
                        };
                        match tags.iter_mut().find(|t| same(t)) {
                            Some(t) if t.tag.seq >= tag.seq => {}
                            Some(t) => t.tag = *tag,
                            None => tags.push(SetTag {
                                elem: elem.clone(),
                                tag: *tag,
                            }),
                        }
                    }
                    SetChange::Remove { elem, tag } => {
                        tags.retain(|t| !(compare_key(&t.elem, elem).is_eq() && t.tag == *tag));
                    }
                }
            }
            tags.sort_by(|a, b| compare_key(&a.elem, &b.elem).then(a.tag.cmp(&b.tag)));
        }
        CollectionChange::Map(entries) => {
            let state = stored
                .collections
                .entry(field)
                .or_insert(CollectionState::Map(Vec::new()));
            let CollectionState::Map(stored_entries) = state else {
                return;
            };
            for e in entries {
                stored_entries.retain(|x| !compare_key(&x.key, &e.key).is_eq());
                stored_entries.push(e.clone());
            }
            stored_entries.sort_by(|a, b| compare_key(&a.key, &b.key));
        }
    }
}

fn apply_one(s: &mut State, w: &Write) -> Result<(), DbError> {
    match w {
        Write::Insert {
            model,
            row,
            fields,
            collections,
        } => {
            if s.rows.contains_key(row) {
                return Err(DbError::Conflict);
            }
            let mut stored = Stored {
                model: *model,
                version: 1,
                deleted: false,
                fields: fields.iter().cloned().collect(),
                collections: BTreeMap::new(),
            };
            for (f, c) in collections {
                apply_collection(&mut stored, *f, c);
            }
            s.rows.insert(*row, stored);
        }
        Write::Update {
            model,
            row,
            expect_version,
            fields,
            collections,
        } => {
            let r = live(s, *model, *row)?;
            if r.version != *expect_version {
                return Err(DbError::VersionMismatch);
            }
            r.version += 1;
            r.fields.extend(fields.iter().cloned());
            for (f, c) in collections {
                apply_collection(r, *f, c);
            }
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
            r.collections.clear();
        }
    }
    Ok(())
}

/// Refuses enum values that are not declared variants of their column (api module rules).
fn check_enums(s: &State, ws: &[Write]) -> Result<(), DbError> {
    let declared = |model: ModelId, f: FieldId, v: &Value| match s.variants(model, f) {
        Some(vs) if *v != Value::Null => ordinal(vs, v).is_some(),
        _ => true,
    };
    for w in ws {
        let (Write::Insert {
            model,
            fields,
            collections,
            ..
        }
        | Write::Update {
            model,
            fields,
            collections,
            ..
        }) = w
        else {
            continue;
        };
        let bad_field = fields.iter().any(|(f, v)| !declared(*model, *f, v));
        let bad_value = collections.iter().any(|(f, c)| match c {
            CollectionChange::Map(entries) => entries
                .iter()
                .filter_map(|e| e.value.as_ref())
                .any(|v| !declared(*model, *f, v)),
            CollectionChange::Set(_) => false,
        });
        if bad_field || bad_value {
            return Err(DbError::Invalid("enum value is not a declared variant"));
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
    fn migrate<'a>(&'a mut self, p: &'a MigrationPlan) -> BoxFuture<'a, Result<(), DbError>> {
        Box::pin(async move {
            p.check()?;
            if p.from != self.state.schema.as_ref().map(|a| a.hash) {
                return Err(DbError::SchemaMismatch);
            }
            // A plan from the stored schema to itself with no steps changes nothing, not even
            // the stored schema text or the enum columns.
            if p.from == Some(p.to) && p.steps.is_empty() {
                return Ok(());
            }
            self.state.schema = Some(AppliedSchema {
                hash: p.to,
                schema: p.schema.clone(),
            });
            self.state.enums = p
                .enums
                .iter()
                .map(|c| ((c.model, c.field), c.variants.clone()))
                .collect();
            Ok(())
        })
    }
    fn applied_schema<'a>(&'a mut self) -> BoxFuture<'a, Result<Option<AppliedSchema>, DbError>> {
        Box::pin(async move { Ok(self.state.schema.clone()) })
    }
    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move { scan(&self.state, q) })
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
        Box::pin(async move { scan(&self.work, q) })
    }
    fn apply<'a>(&'a mut self, ws: &'a [Write]) -> BoxFuture<'a, Result<(), DbError>> {
        Box::pin(async move {
            check_writes(ws)?;
            check_enums(&self.work, ws)?;
            ws.iter().try_for_each(|w| apply_one(&mut self.work, w))
        })
    }
    fn append_ops<'a>(&'a mut self, ops: &'a [NewOp]) -> BoxFuture<'a, Result<ServerSeq, DbError>> {
        Box::pin(async move {
            for (i, op) in ops.iter().enumerate() {
                let in_log = self.work.ops.iter().any(|o| o.op.id == op.id);
                if in_log || ops[..i].iter().any(|o| o.id == op.id) {
                    return Err(DbError::Conflict);
                }
            }
            for op in ops {
                let seq = ServerSeq(self.work.ops.len() as u64 + 1);
                self.work.ops.push(StoredOp {
                    seq,
                    op: op.clone(),
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
            let ops = self.work.ops.iter().filter(|o| o.seq > after);
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

/// Row id made at wall time `n` by replica 1.
fn rid(n: u64) -> RowId {
    RowId::new(Hlc::new(n, 0, ReplicaId(1)).unwrap())
}

fn int(v: i64) -> Value {
    Value::int(v).unwrap()
}

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn insert(model: ModelId, row: u64, fields: Vec<(FieldId, Value)>) -> Write {
    Write::Insert {
        model,
        row: rid(row),
        fields,
        collections: vec![],
    }
}

fn update(model: ModelId, row: u64, v: u64, fields: Vec<(FieldId, Value)>) -> Write {
    Write::Update {
        model,
        row: rid(row),
        expect_version: v,
        fields,
        collections: vec![],
    }
}

fn delete(model: ModelId, row: u64, v: u64) -> Write {
    Write::Delete {
        model,
        row: rid(row),
        expect_version: v,
    }
}

fn sorted(model: ModelId, order: Vec<(FieldId, Dir)>) -> Query {
    Query {
        order,
        ..Query::all(model)
    }
}

fn filtered(model: ModelId, filter: Expr) -> Query {
    Query {
        filter: Some(filter),
        ..Query::all(model)
    }
}

/// Wall times of the returned row ids, which is how the tests name rows.
fn ids(rows: Rows) -> Vec<u64> {
    rows.0.iter().map(|r| r.id.hlc().wall_ms()).collect()
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

fn tag(replica: u64, seq: u32) -> OpId {
    OpId {
        replica: ReplicaId(replica),
        seq,
    }
}

fn new_op(replica: u64, seq: u32) -> NewOp {
    NewOp {
        id: tag(replica, seq),
        hlc: Hlc::new(u64::from(seq), 0, ReplicaId(replica)).unwrap(),
        model: 1,
        row: rid(1),
        body: b"{}".to_vec(),
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
        conn.migrate(&plan(None, 1)).await.unwrap();
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
        assert_eq!(
            tx.apply(&[update(1, 7, 9, vec![])]).await,
            Err(DbError::VersionMismatch)
        );
        tx.rollback().await.unwrap();

        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[delete(1, 7, 1)]).await.unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(conn.query(&Query::all(1)).await.unwrap().0.len(), 1);
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
        tx.apply(&[delete(1, 5, 1)]).await.unwrap();
        assert_eq!(
            tx.apply(&[insert(1, 5, vec![])]).await,
            Err(DbError::Conflict)
        );
        assert_eq!(
            tx.apply(&[insert(2, 5, vec![])]).await,
            Err(DbError::Conflict)
        );
        tx.commit().await.unwrap();
        assert!(conn.query(&Query::all(1)).await.unwrap().0.is_empty());
    });
}

#[test]
fn extreme_row_ids_are_distinct_and_ordered() {
    let mut conn = connect();
    let max_hlc = Hlc::new(ostrel_core::ids::MAX_WALL_MS, u16::MAX, ReplicaId(u64::MAX));
    let max = RowId::new(max_hlc.unwrap());
    let min = RowId::new(Hlc::new(0, 0, ReplicaId(0)).unwrap());
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        for row in [max, min] {
            let w = Write::Insert {
                model: 1,
                row,
                fields: vec![],
                collections: vec![],
            };
            tx.apply(&[w]).await.unwrap();
        }
        let again = Write::Insert {
            model: 2,
            row: max,
            fields: vec![],
            collections: vec![],
        };
        assert_eq!(tx.apply(&[again]).await, Err(DbError::Conflict));
        let rows = tx.query(&Query::all(1)).await.unwrap().0;
        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![min, max]
        );
        tx.rollback().await.unwrap();
    });
}

#[test]
fn update_and_delete_of_missing_or_dead_rows_are_not_found() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        assert_eq!(
            tx.apply(&[update(1, 3, 1, vec![])]).await,
            Err(DbError::NotFound)
        );
        assert_eq!(tx.apply(&[delete(1, 3, 1)]).await, Err(DbError::NotFound));
        tx.apply(&[insert(1, 3, vec![])]).await.unwrap();
        // Right id, wrong model.
        assert_eq!(
            tx.apply(&[update(2, 3, 1, vec![])]).await,
            Err(DbError::NotFound)
        );
        assert_eq!(tx.apply(&[delete(2, 3, 1)]).await, Err(DbError::NotFound));
        tx.apply(&[delete(1, 3, 1)]).await.unwrap();
        // A deleted row is not found, even with its current version.
        assert_eq!(
            tx.apply(&[update(1, 3, 2, vec![])]).await,
            Err(DbError::NotFound)
        );
        assert_eq!(tx.apply(&[delete(1, 3, 2)]).await, Err(DbError::NotFound));
        tx.rollback().await.unwrap();
    });
}

#[test]
fn update_changes_only_listed_fields_and_bumps_version() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[insert(1, 1, vec![(0, text("a")), (1, int(1))])])
            .await
            .unwrap();
        tx.apply(&[update(1, 1, 1, vec![(1, int(2))])])
            .await
            .unwrap();
        // An update without fields still bumps the version.
        tx.apply(&[update(1, 1, 2, vec![])]).await.unwrap();
        // A field written as null is listed, not dropped.
        tx.apply(&[update(1, 1, 3, vec![(2, Value::Null)])])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let rows = conn.query(&Query::all(1)).await.unwrap().0;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].version, 4);
        assert_eq!(
            rows[0].fields,
            vec![(0, text("a")), (1, int(2)), (2, Value::Null)]
        );
    });
}

#[test]
fn transaction_sees_own_writes_connection_sees_only_committed() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[insert(1, 1, vec![])]).await.unwrap();
        assert_eq!(tx.query(&Query::all(1)).await.unwrap().0.len(), 1);
        drop(tx); // dropping without commit is a rollback
        assert!(conn.query(&Query::all(1)).await.unwrap().0.is_empty());
    });
}

#[test]
fn query_order_ties_and_limits() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[
            insert(1, 3, vec![(0, int(1))]),
            insert(1, 1, vec![(0, int(2))]),
            insert(1, 2, vec![(0, int(1))]),
            insert(2, 9, vec![(0, int(0))]),
        ])
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(ids(conn.query(&Query::all(1)).await.unwrap()), [1, 2, 3]);
        let asc = sorted(1, vec![(0, Dir::Asc)]);
        assert_eq!(ids(conn.query(&asc).await.unwrap()), [2, 3, 1]);
        // Ties on the sort key are broken by id in the direction of the key.
        let desc = sorted(1, vec![(0, Dir::Desc)]);
        assert_eq!(ids(conn.query(&desc).await.unwrap()), [1, 3, 2]);
        let desc2 = Query {
            limit: Some(2),
            ..desc
        };
        assert_eq!(ids(conn.query(&desc2).await.unwrap()), [1, 3]);
        let none = Query {
            limit: Some(0),
            ..Query::all(1)
        };
        assert!(conn.query(&none).await.unwrap().0.is_empty());
        assert!(conn.query(&Query::all(7)).await.unwrap().0.is_empty());
    });
}

#[test]
fn null_and_missing_fields_sort_first_ascending_and_last_descending() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[
            insert(1, 1, vec![(0, int(-5))]),
            insert(1, 2, vec![(0, Value::Null)]),
            insert(1, 3, vec![]),
            insert(1, 4, vec![(0, int(ostrel_core::value::INT_MIN))]),
            insert(1, 5, vec![(0, int(ostrel_core::value::INT_MAX))]),
        ])
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let asc = ids(conn.query(&sorted(1, vec![(0, Dir::Asc)])).await.unwrap());
        assert_eq!(asc, [2, 3, 4, 1, 5]);
        let mut desc = ids(conn.query(&sorted(1, vec![(0, Dir::Desc)])).await.unwrap());
        // Descending is the exact reverse of ascending, nulls included.
        desc.reverse();
        assert_eq!(desc, asc);
    });
}

#[test]
fn cursor_pages_return_every_row_once_also_after_the_cursor_row_is_gone() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        let writes: Vec<Write> = (1..=7)
            .map(|n| {
                let v = if n % 3 == 0 {
                    Value::Null
                } else {
                    int((n % 2) as i64)
                };
                insert(1, n, vec![(0, v)])
            })
            .collect();
        tx.apply(&writes).await.unwrap();
        tx.commit().await.unwrap();

        for dir in [Dir::Asc, Dir::Desc] {
            let full = ids(conn.query(&sorted(1, vec![(0, dir)])).await.unwrap());
            let mut paged = Vec::new();
            let mut after: Option<Cursor> = None;
            loop {
                let q = Query {
                    limit: Some(2),
                    after: after.clone(),
                    ..sorted(1, vec![(0, dir)])
                };
                let rows = conn.query(&q).await.unwrap().0;
                let Some(last) = rows.last() else { break };
                after = Some(Cursor {
                    values: sort_values(last, &q.order),
                    id: last.id,
                });
                paged.extend(rows.iter().map(|r| r.id.hlc().wall_ms()));
            }
            assert_eq!(paged, full, "{dir:?}");
        }

        // The cursor is a position: deleting its row does not change the next page.
        let q = Query {
            limit: Some(2),
            after: Some(Cursor {
                values: vec![int(1)],
                id: rid(1),
            }),
            ..sorted(1, vec![(0, Dir::Asc)])
        };
        let before = ids(conn.query(&q).await.unwrap());
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[delete(1, 1, 1)]).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(ids(conn.query(&q).await.unwrap()), before);
        assert_eq!(before, [5, 7]);
    });
}

#[test]
fn filters_are_two_valued_and_fail_closed_on_broken_hops() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[
            // Model 2 rows: 10 live, 11 deleted below.
            insert(2, 10, vec![(0, text("room"))]),
            insert(2, 11, vec![(0, text("gone"))]),
            insert(1, 1, vec![(0, int(1)), (1, Value::Ref(rid(10)))]),
            insert(1, 2, vec![(0, Value::Null), (1, Value::Ref(rid(11)))]),
            insert(1, 3, vec![(1, Value::Null)]),
        ])
        .await
        .unwrap();
        tx.apply(&[delete(2, 11, 1)]).await.unwrap();
        tx.commit().await.unwrap();
        let mut run = async |e: Expr| ids(conn.query(&filtered(1, e)).await.unwrap());

        let lt = Expr::Cmp(CmpOp::Lt, field(0), konst(int(5)));
        assert_eq!(run(lt.clone()).await, [1]);
        // Not of a comparison with null is true (two valued logic, unlike SQL).
        assert_eq!(run(Expr::Not(Box::new(lt))).await, [2, 3]);
        assert_eq!(
            run(Expr::Cmp(CmpOp::Eq, field(0), konst(Value::Null))).await,
            [2, 3]
        );
        assert_eq!(
            run(Expr::Cmp(CmpOp::Ne, field(0), konst(Value::Null))).await,
            [1]
        );
        assert_eq!(run(Expr::And(vec![])).await, [1, 2, 3]);
        assert!(run(Expr::Or(vec![])).await.is_empty());
        // A non boolean filter selects nothing.
        assert!(run(Expr::Const(int(1))).await.is_empty());

        let hop = |f| Path {
            hops: vec![Hop { field: 1, model: 2 }],
            field: f,
        };
        let room_named = Expr::Cmp(
            CmpOp::Eq,
            Box::new(Expr::Field(hop(0))),
            konst(text("room")),
        );
        assert_eq!(run(room_named.clone()).await, [1]);
        // Row 2 hops to a deleted row and row 3 through null: false even under Not and Or.
        assert!(
            run(Expr::Not(Box::new(room_named.clone())))
                .await
                .is_empty()
        );
        let or = Expr::Or(vec![Expr::And(vec![]), Expr::Not(Box::new(room_named))]);
        // Row 1 passes through the empty And; rows 2 and 3 stay out although it is true.
        assert_eq!(run(or).await, [1]);
        // A hop into the wrong model fails as well.
        let wrong = Expr::RowId(vec![Hop { field: 1, model: 3 }]);
        assert!(
            run(Expr::Cmp(
                CmpOp::Eq,
                Box::new(wrong),
                konst(Value::Ref(rid(10)))
            ))
            .await
            .is_empty()
        );
        let own = Expr::Cmp(
            CmpOp::Eq,
            Box::new(Expr::RowId(vec![])),
            konst(Value::Ref(rid(3))),
        );
        assert_eq!(run(own).await, [3]);
        let coalesce = Expr::Cmp(
            CmpOp::Ge,
            Box::new(Expr::Coalesce(field(0), int(7))),
            konst(int(7)),
        );
        assert_eq!(run(coalesce).await, [2, 3]);
    });
}

#[test]
fn set_tags_follow_d49_add_wins_and_replay_is_harmless() {
    let mut conn = connect();
    let set = |changes| vec![(5, CollectionChange::Set(changes))];
    let add = |e: &str, r, s| SetChange::Add {
        elem: text(e),
        tag: tag(r, s),
    };
    let remove = |e: &str, r, s| SetChange::Remove {
        elem: text(e),
        tag: tag(r, s),
    };
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[Write::Insert {
            model: 1,
            row: rid(1),
            fields: vec![],
            collections: set(vec![add("a", 1, 1), add("a", 2, 1)]),
        }])
        .await
        .unwrap();
        let upd = |v, changes| Write::Update {
            model: 1,
            row: rid(1),
            expect_version: v,
            fields: vec![],
            collections: set(changes),
        };
        // Replica 1 removes the tag it saw; the concurrent add of replica 2 survives.
        tx.apply(&[upd(1, vec![remove("a", 1, 1)])]).await.unwrap();
        // Replica 2 re-adds (newer tag), then an old remove naming its first tag is replayed.
        tx.apply(&[upd(2, vec![add("a", 2, 4), remove("a", 2, 1)])])
            .await
            .unwrap();
        // A stale add of replica 2 and a remove of an unknown tag change nothing.
        tx.apply(&[upd(
            3,
            vec![add("a", 2, 3), remove("b", 9, 9), add("b", 1, 2)],
        )])
        .await
        .unwrap();
        // The same change again is harmless.
        tx.apply(&[upd(4, vec![add("b", 1, 2)])]).await.unwrap();
        tx.commit().await.unwrap();

        let rows = conn.query(&Query::all(1)).await.unwrap().0;
        assert_eq!(
            rows[0].version, 5,
            "collection only updates bump the version"
        );
        let want = vec![
            SetTag {
                elem: text("a"),
                tag: tag(2, 4),
            },
            SetTag {
                elem: text("b"),
                tag: tag(1, 2),
            },
        ];
        assert_eq!(rows[0].collections, vec![(5, CollectionState::Set(want))]);

        let member = |e: &str| Expr::InSet {
            elem: konst(text(e)),
            set: Path {
                hops: vec![],
                field: 5,
            },
        };
        let mut run = async |e: Expr| ids(conn.query(&filtered(1, e)).await.unwrap());
        assert_eq!(run(member("a")).await, [1]);
        assert!(run(member("c")).await.is_empty());
    });
}

#[test]
fn map_entries_replace_per_key_and_removed_keys_stay_as_tombstones() {
    let mut conn = connect();
    let entry = |k: &str, v: Option<i64>, t| MapEntry {
        key: text(k),
        value: v.map(int),
        hlc: Hlc::new(t, 0, ReplicaId(1)).unwrap(),
    };
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[Write::Insert {
            model: 1,
            row: rid(1),
            fields: vec![],
            collections: vec![(
                6,
                CollectionChange::Map(vec![entry("x", Some(1), 1), entry("y", Some(2), 1)]),
            )],
        }])
        .await
        .unwrap();
        tx.apply(&[Write::Update {
            model: 1,
            row: rid(1),
            expect_version: 1,
            fields: vec![],
            collections: vec![(
                6,
                CollectionChange::Map(vec![entry("y", None, 2), entry("x", Some(3), 2)]),
            )],
        }])
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let rows = conn.query(&Query::all(1)).await.unwrap().0;
        let want = CollectionState::Map(vec![entry("x", Some(3), 2), entry("y", None, 2)]);
        assert_eq!(rows[0].collections, vec![(6, want)]);

        let map = Path {
            hops: vec![],
            field: 6,
        };
        let has = |k: &str| Expr::InMap {
            key: konst(text(k)),
            map: map.clone(),
        };
        let get = |k: &str| Expr::MapGet {
            map: map.clone(),
            key: konst(text(k)),
        };
        let mut run = async |e: Expr| ids(conn.query(&filtered(1, e)).await.unwrap());
        assert_eq!(run(has("x")).await, [1]);
        assert!(
            run(has("y")).await.is_empty(),
            "removed key is not in the map"
        );
        let role = Expr::Cmp(
            CmpOp::Ge,
            Box::new(Expr::Coalesce(Box::new(get("y")), int(0))),
            konst(int(0)),
        );
        assert_eq!(run(role).await, [1]);
        let eq3 = Expr::Cmp(CmpOp::Eq, Box::new(get("x")), konst(int(3)));
        assert_eq!(run(eq3).await, [1]);
    });
}

#[test]
fn malformed_writes_are_refused_before_anything_changes() {
    let set_add = CollectionChange::Set(vec![SetChange::Add {
        elem: int(1),
        tag: tag(1, 1),
    }]);
    let set_remove = CollectionChange::Set(vec![SetChange::Remove {
        elem: int(1),
        tag: tag(1, 1),
    }]);
    let entry = |k| MapEntry {
        key: int(k),
        value: None,
        hlc: Hlc::new(1, 0, ReplicaId(1)).unwrap(),
    };
    let ins = |fields, collections| Write::Insert {
        model: 1,
        row: rid(1),
        fields,
        collections,
    };
    let bad = [
        ins(vec![(0, int(1)), (0, int(2))], vec![]),
        ins(vec![(0, int(1))], vec![(0, set_add.clone())]),
        ins(vec![], vec![(1, set_add.clone()), (1, set_add.clone())]),
        ins(vec![(0, Value::Set(vec![]))], vec![]),
        ins(vec![(0, Value::Map(vec![]))], vec![]),
        ins(vec![], vec![(1, set_remove.clone())]),
        ins(
            vec![],
            vec![(1, CollectionChange::Map(vec![entry(1), entry(2), entry(1)]))],
        ),
    ];
    for w in &bad {
        assert!(
            matches!(
                check_writes(std::slice::from_ref(w)),
                Err(DbError::Invalid(_))
            ),
            "{w:?}"
        );
    }
    let good = [
        ins(
            vec![(0, Value::List(vec![int(1)])), (1, Value::Null)],
            vec![(2, set_add.clone())],
        ),
        Write::Update {
            model: 1,
            row: rid(1),
            expect_version: 1,
            fields: vec![],
            collections: vec![
                (1, set_remove),
                (2, CollectionChange::Map(vec![entry(1), entry(2)])),
            ],
        },
    ];
    assert_eq!(check_writes(&good), Ok(()));
    assert_eq!(check_writes(&[]), Ok(()));

    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        let batch = [insert(1, 2, vec![]), bad[0].clone()];
        assert!(matches!(tx.apply(&batch).await, Err(DbError::Invalid(_))));
        assert!(tx.query(&Query::all(1)).await.unwrap().0.is_empty());
        tx.rollback().await.unwrap();
    });
}

#[test]
fn query_check_bounds_filter_size_hops_and_cursor() {
    let leaf = || Expr::Const(Value::Bool(true));
    let and = |n| Expr::And((0..n).map(|_| leaf()).collect());
    // The And node counts too.
    assert_eq!(filtered(1, and(MAX_FILTER_NODES - 1)).check(), Ok(()));
    assert!(matches!(
        filtered(1, and(MAX_FILTER_NODES)).check(),
        Err(DbError::Invalid(_))
    ));
    // A deep chain is refused without walking it recursively.
    let mut deep = leaf();
    for _ in 0..10_000 {
        deep = Expr::Not(Box::new(deep));
    }
    assert!(matches!(
        filtered(1, deep).check(),
        Err(DbError::Invalid(_))
    ));

    let hops = |n| {
        Expr::Field(Path {
            hops: vec![Hop { field: 0, model: 0 }; n],
            field: 0,
        })
    };
    assert_eq!(filtered(1, hops(2)).check(), Ok(()));
    assert!(matches!(
        filtered(1, hops(3)).check(),
        Err(DbError::Invalid(_))
    ));
    let deep_set = Expr::InSet {
        elem: konst(int(1)),
        set: Path {
            hops: vec![Hop { field: 0, model: 0 }; 3],
            field: 0,
        },
    };
    assert!(matches!(
        filtered(1, deep_set).check(),
        Err(DbError::Invalid(_))
    ));
    assert!(matches!(
        filtered(1, Expr::Const(Value::List(vec![]))).check(),
        Err(DbError::Invalid(_))
    ));
    assert!(matches!(
        filtered(1, Expr::Coalesce(field(0), Value::Set(vec![]))).check(),
        Err(DbError::Invalid(_))
    ));

    let cursor = |n| Query {
        after: Some(Cursor {
            values: vec![Value::Null; n],
            id: rid(1),
        }),
        ..sorted(1, vec![(0, Dir::Asc)])
    };
    assert_eq!(cursor(1).check(), Ok(()));
    assert!(matches!(cursor(0).check(), Err(DbError::Invalid(_))));
    assert!(matches!(cursor(2).check(), Err(DbError::Invalid(_))));

    let mut conn = connect();
    assert!(matches!(
        block_on(conn.query(&cursor(2))),
        Err(DbError::Invalid(_))
    ));
}

#[test]
fn op_log_positions_boundaries_and_duplicate_ids() {
    let mut conn = connect();
    block_on(async {
        let mut tx = conn.begin().await.unwrap();
        assert_eq!(tx.append_ops(&[]).await, Ok(ServerSeq(0)));
        assert_eq!(tx.ops_since(ServerSeq(0), 10).await, Ok(vec![]));
        assert_eq!(
            tx.append_ops(&[new_op(1, 1), new_op(1, 2)]).await,
            Ok(ServerSeq(2))
        );
        assert_eq!(tx.append_ops(&[]).await, Ok(ServerSeq(2)));
        // An op id already in the log, or twice in one call: nothing of the call is appended.
        assert_eq!(
            tx.append_ops(&[new_op(2, 1), new_op(1, 2)]).await,
            Err(DbError::Conflict)
        );
        assert_eq!(
            tx.append_ops(&[new_op(2, 1), new_op(2, 1)]).await,
            Err(DbError::Conflict)
        );
        assert_eq!(tx.append_ops(&[]).await, Ok(ServerSeq(2)));
        assert_eq!(tx.append_ops(&[new_op(2, 1)]).await, Ok(ServerSeq(3)));

        let seqs = |ops: Vec<StoredOp>| ops.iter().map(|o| o.seq.0).collect::<Vec<_>>();
        assert_eq!(
            seqs(tx.ops_since(ServerSeq(0), 10).await.unwrap()),
            [1, 2, 3]
        );
        assert_eq!(seqs(tx.ops_since(ServerSeq(1), 1).await.unwrap()), [2]);
        assert_eq!(tx.ops_since(ServerSeq(0), 0).await, Ok(vec![]));
        assert_eq!(tx.ops_since(ServerSeq(3), 10).await, Ok(vec![]));
        assert_eq!(
            tx.ops_since(ServerSeq(u64::MAX), u32::MAX).await,
            Ok(vec![])
        );
        let third = tx.ops_since(ServerSeq(2), 10).await.unwrap();
        assert_eq!(third[0].op, new_op(2, 1));
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
fn migration_plans_must_start_from_the_stored_schema() {
    let mut conn = connect();
    block_on(async {
        // A fresh database has no schema: a plan from some schema is refused.
        assert_eq!(
            conn.migrate(&plan(Some(1), 2)).await,
            Err(DbError::SchemaMismatch)
        );
        conn.migrate(&plan(None, 1)).await.unwrap();
        // Replaying the first plan is refused, the stored schema stays.
        assert_eq!(
            conn.migrate(&plan(None, 1)).await,
            Err(DbError::SchemaMismatch)
        );
        assert_eq!(
            conn.migrate(&plan(Some(2), 3)).await,
            Err(DbError::SchemaMismatch)
        );
        // A plan from the stored schema to itself is a no op.
        conn.migrate(&plan(Some(1), 1)).await.unwrap();
        conn.migrate(&plan(Some(1), 2)).await.unwrap();
        assert_eq!(
            conn.migrate(&plan(Some(1), 3)).await,
            Err(DbError::SchemaMismatch)
        );
    });
}

fn en(name: &str) -> Value {
    Value::Enum(name.to_string())
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

/// Model 1: field 0 `status` (backlog, todo, done), field 1 `Bytes`, field 2 `Set` of enum
/// elements, field 3 `Map` of user to role (guest, member, admin). Declaration order differs
/// from name order in both enums.
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

#[test]
fn applied_schema_reports_the_last_successful_migration_only() {
    let mut conn = connect();
    block_on(async {
        assert_eq!(conn.applied_schema().await, Ok(None), "never migrated");
        // A refused plan of a fresh database leaves it unmigrated.
        assert_eq!(
            conn.migrate(&plan(Some(1), 2)).await,
            Err(DbError::SchemaMismatch)
        );
        assert_eq!(conn.applied_schema().await, Ok(None));

        conn.migrate(&plan(None, 1)).await.unwrap();
        let first = AppliedSchema {
            hash: SchemaHash([1; 32]),
            schema: "{\"v\":1}".to_string(),
        };
        assert_eq!(conn.applied_schema().await, Ok(Some(first.clone())));

        // SchemaMismatch and an invalid plan change nothing.
        assert_eq!(
            conn.migrate(&plan(Some(2), 3)).await,
            Err(DbError::SchemaMismatch)
        );
        assert_eq!(
            conn.migrate(&plan(None, 3)).await,
            Err(DbError::SchemaMismatch)
        );
        let invalid = MigrationPlan {
            enums: vec![EnumColumn {
                model: 1,
                field: 0,
                variants: vec![],
            }],
            ..plan(Some(1), 3)
        };
        assert!(matches!(
            conn.migrate(&invalid).await,
            Err(DbError::Invalid(_))
        ));
        assert_eq!(conn.applied_schema().await, Ok(Some(first.clone())));

        // A no op plan keeps the stored schema text of the first migration.
        let noop = MigrationPlan {
            schema: "other".to_string(),
            ..plan(Some(1), 1)
        };
        conn.migrate(&noop).await.unwrap();
        assert_eq!(conn.applied_schema().await, Ok(Some(first.clone())));

        conn.migrate(&plan(Some(1), 2)).await.unwrap();
        let second = conn.applied_schema().await.unwrap().unwrap();
        assert_eq!(second.hash, SchemaHash([2; 32]));
        assert_eq!(second.schema, "{\"v\":2}");
    });
}

#[test]
fn a_no_op_migration_keeps_the_stored_schema_and_enum_columns() {
    let mut conn = connect();
    block_on(async {
        conn.migrate(&enum_plan()).await.unwrap();
        let stored = conn.applied_schema().await.unwrap();
        // Same hash, no steps, but a different schema text and no enum columns.
        let noop = MigrationPlan {
            schema: "other".to_string(),
            enums: vec![],
            ..plan(Some(1), 1)
        };
        conn.migrate(&noop).await.unwrap();
        assert_eq!(conn.applied_schema().await.unwrap(), stored);

        // The enum columns of the first migration still validate writes.
        let mut tx = conn.begin().await.unwrap();
        assert!(matches!(
            tx.apply(&[insert(1, 9, vec![(0, en("nope"))])]).await,
            Err(DbError::Invalid(_))
        ));
        drop(tx);

        // And they still give the declaration order.
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[
            insert(1, 1, vec![(0, en("done"))]),
            insert(1, 2, vec![(0, en("backlog"))]),
            insert(1, 3, vec![(0, en("todo"))]),
        ])
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let by = sorted(1, vec![(0, Dir::Asc)]);
        assert_eq!(ids(conn.query(&by).await.unwrap()), [2, 3, 1]);

        // An invalid no op plan is still refused by the check.
        let invalid = MigrationPlan {
            enums: vec![EnumColumn {
                model: 1,
                field: 0,
                variants: vec![],
            }],
            ..plan(Some(1), 1)
        };
        assert!(matches!(
            conn.migrate(&invalid).await,
            Err(DbError::Invalid(_))
        ));
        assert_eq!(conn.applied_schema().await.unwrap(), stored);
    });
}

#[test]
fn migration_plan_check_refuses_ambiguous_enum_columns() {
    let column = |field, variants: &[&str]| EnumColumn {
        model: 1,
        field,
        variants: strings(variants),
    };
    let with = |enums| MigrationPlan {
        enums,
        ..plan(None, 1)
    };
    assert_eq!(with(vec![]).check(), Ok(()));
    assert_eq!(with(vec![column(0, &["a"])]).check(), Ok(()));
    assert_eq!(
        with(vec![column(0, &["a"]), column(1, &["a"])]).check(),
        Ok(())
    );
    assert!(with(vec![column(0, &[])]).check().is_err());
    assert!(with(vec![column(0, &["a", "b", "a"])]).check().is_err());
    assert!(
        with(vec![column(0, &["a"]), column(0, &["b"])])
            .check()
            .is_err()
    );
    let p = enum_plan();
    assert_eq!(
        p.enum_variants(1, 0),
        Some(&strings(&["backlog", "todo", "done"])[..])
    );
    assert_eq!(p.enum_variants(1, 1), None);
    assert_eq!(p.enum_variants(2, 0), None);
}

#[test]
fn rows_sort_enums_by_declaration_and_bytes_bytewise_unlike_collections() {
    // compare_key (D61) and the row order disagree on exactly these values (D79).
    assert_eq!(compare_key(&en("todo"), &en("done")), Ordering::Greater);
    let (lo, hi) = (Value::Bytes(vec![0x00]), Value::Bytes(vec![0xF8]));
    assert_eq!(compare_key(&lo, &hi), Ordering::Greater, "AA after -A");

    let mut conn = connect();
    block_on(async {
        conn.migrate(&enum_plan()).await.unwrap();
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[
            insert(1, 1, vec![(0, en("done")), (1, hi.clone())]),
            insert(1, 2, vec![(0, en("backlog")), (1, lo.clone())]),
            insert(1, 3, vec![(0, en("todo")), (1, Value::Bytes(vec![]))]),
            insert(1, 4, vec![(0, Value::Null)]),
        ])
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let by = |f, d| sorted(1, vec![(f, d)]);
        // Declaration order, not name order (which would be backlog, done, todo).
        assert_eq!(
            ids(conn.query(&by(0, Dir::Asc)).await.unwrap()),
            [4, 2, 3, 1]
        );
        assert_eq!(
            ids(conn.query(&by(0, Dir::Desc)).await.unwrap()),
            [1, 3, 2, 4]
        );
        // Bytewise: empty, 0x00, 0xF8; a never written field first.
        assert_eq!(
            ids(conn.query(&by(1, Dir::Asc)).await.unwrap()),
            [4, 3, 2, 1]
        );
        assert_eq!(
            ids(conn.query(&by(1, Dir::Desc)).await.unwrap()),
            [1, 2, 3, 4]
        );

        // A cursor after `todo` continues in declaration order.
        let page = Query {
            after: Some(Cursor {
                values: vec![en("todo")],
                id: rid(3),
            }),
            ..by(0, Dir::Asc)
        };
        assert_eq!(ids(conn.query(&page).await.unwrap()), [1]);
        let page = Query {
            after: Some(Cursor {
                values: vec![lo.clone()],
                id: rid(2),
            }),
            ..by(1, Dir::Asc)
        };
        assert_eq!(ids(conn.query(&page).await.unwrap()), [1]);

        // Ordered filters use the declaration order of the column side.
        let ge = |v: &str| filtered(1, Expr::Cmp(CmpOp::Ge, field(0), konst(en(v))));
        assert_eq!(ids(conn.query(&ge("todo")).await.unwrap()), [1, 3]);
        let le = filtered(1, Expr::Cmp(CmpOp::Le, konst(en("todo")), field(0)));
        assert_eq!(ids(conn.query(&le).await.unwrap()), [1, 3]);
        // Unknown variant or no enum column on either side: false, also for Lt.
        assert!(conn.query(&ge("nope")).await.unwrap().0.is_empty());
        let consts = Expr::Cmp(CmpOp::Lt, konst(en("a")), konst(en("b")));
        assert!(conn.query(&filtered(1, consts)).await.unwrap().0.is_empty());
        let lt = filtered(1, Expr::Cmp(CmpOp::Lt, field(1), konst(hi.clone())));
        assert_eq!(ids(conn.query(&lt).await.unwrap()), [2, 3]);
    });
}

#[test]
fn collections_keep_name_and_base64url_order_and_map_values_use_the_column_order() {
    let mut conn = connect();
    let add = |v: Value, seq| SetChange::Add {
        elem: v,
        tag: tag(1, seq),
    };
    let role = |user: u64, r: Option<&str>| MapEntry {
        key: Value::Ref(rid(user)),
        value: r.map(en),
        hlc: Hlc::new(1, 0, ReplicaId(1)).unwrap(),
    };
    block_on(async {
        conn.migrate(&enum_plan()).await.unwrap();
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[
            Write::Insert {
                model: 1,
                row: rid(1),
                fields: vec![],
                collections: vec![
                    (
                        2,
                        CollectionChange::Set(vec![
                            add(en("todo"), 1),
                            add(en("backlog"), 2),
                            add(en("done"), 3),
                        ]),
                    ),
                    (3, CollectionChange::Map(vec![role(7, Some("admin"))])),
                ],
            },
            Write::Insert {
                model: 1,
                row: rid(2),
                fields: vec![],
                collections: vec![
                    (
                        2,
                        CollectionChange::Set(vec![
                            add(Value::Bytes(vec![0x00]), 1),
                            add(Value::Bytes(vec![0xF8]), 2),
                        ]),
                    ),
                    (3, CollectionChange::Map(vec![role(7, Some("guest"))])),
                ],
            },
            Write::Insert {
                model: 1,
                row: rid(3),
                fields: vec![],
                collections: vec![(3, CollectionChange::Map(vec![role(7, None)]))],
            },
        ])
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let rows = conn.query(&Query::all(1)).await.unwrap().0;
        let elems = |r: &Row| match &r.collections[0].1 {
            CollectionState::Set(tags) => tags.iter().map(|t| t.elem.clone()).collect::<Vec<_>>(),
            CollectionState::Map(_) => vec![],
        };
        // Set elements by variant name and by base64url text (D61), not by row order.
        assert_eq!(elems(&rows[0]), [en("backlog"), en("done"), en("todo")]);
        assert_eq!(
            elems(&rows[1]),
            [Value::Bytes(vec![0xF8]), Value::Bytes(vec![0x00])]
        );

        // (roles[me] ?? guest) >= member in declaration order: only the admin row. By name,
        // admin < guest < member would select none of the rows.
        let rule = Expr::Cmp(
            CmpOp::Ge,
            Box::new(Expr::Coalesce(
                Box::new(Expr::MapGet {
                    map: Path {
                        hops: vec![],
                        field: 3,
                    },
                    key: konst(Value::Ref(rid(7))),
                }),
                en("guest"),
            )),
            konst(en("member")),
        );
        assert_eq!(ids(conn.query(&filtered(1, rule)).await.unwrap()), [1]);
    });
}

#[test]
fn undeclared_enum_values_are_refused_before_anything_changes() {
    let mut conn = connect();
    block_on(async {
        conn.migrate(&enum_plan()).await.unwrap();
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[insert(1, 1, vec![(0, en("todo"))])])
            .await
            .unwrap();
        let bad_column = [
            insert(1, 2, vec![(0, en("backlog"))]),
            update(1, 1, 1, vec![(0, en("Todo"))]),
        ];
        assert!(matches!(
            tx.apply(&bad_column).await,
            Err(DbError::Invalid(_))
        ));
        let bad_map_value = [Write::Update {
            model: 1,
            row: rid(1),
            expect_version: 1,
            fields: vec![],
            collections: vec![(
                3,
                CollectionChange::Map(vec![MapEntry {
                    key: Value::Ref(rid(7)),
                    value: Some(en("owner")),
                    hlc: Hlc::new(1, 0, ReplicaId(1)).unwrap(),
                }]),
            )],
        }];
        assert!(matches!(
            tx.apply(&bad_map_value).await,
            Err(DbError::Invalid(_))
        ));
        // Null, a non enum column and any Set element are not checked against variants.
        tx.apply(&[update(1, 1, 1, vec![(0, Value::Null), (5, en("any"))])])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let rows = conn.query(&Query::all(1)).await.unwrap().0;
        assert_eq!(
            ids(Rows(rows.clone())),
            [1],
            "row 2 of the refused batch is absent"
        );
        assert_eq!(rows[0].version, 2);
    });
}

#[test]
fn errors_display_without_panicking() {
    let errs = [
        DbError::Conflict,
        DbError::VersionMismatch,
        DbError::NotFound,
        DbError::SchemaMismatch,
        DbError::Invalid("x"),
        DbError::Unsupported("x"),
        DbError::Backend(String::new()),
    ];
    for e in errs {
        assert!(!e.to_string().is_empty());
    }
}
