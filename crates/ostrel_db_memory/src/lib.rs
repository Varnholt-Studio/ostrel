//! In memory driver for the database interface of the Ostrel programming language.
//!
//! The driver serves the URL scheme `memory:`. It keeps all data in the process and loses it when
//! the last connection is dropped, which makes it the driver for tests and quick local runs. It is
//! also the template for community drivers (AC-18): every rule of the contract in
//! [`ostrel_db::api`] is implemented here in plain Rust, in the order a new driver meets them.
//!
//! Model of the data:
//!
//! * All connections opened from one [`MemoryDriver`] share one database. A connection reads
//!   committed state.
//! * A transaction works on a private copy of the committed state and replaces it on commit, so
//!   it sees its own writes and nobody else does. Two transactions that both change data and
//!   overlap in time cannot both commit: the second commit fails with [`DbError::Backend`]
//!   (first committer wins), and the caller retries. A migration counts as such a change.
//! * After a call inside a transaction failed, every further call except rollback fails too, so
//!   half applied writes can never be committed by mistake.
//! * The applied schema and its enum columns are part of the database, so every connection of
//!   the driver sees them (D87).
//!
//! Ordering of query results follows the column order of the contract (`ostrel_db::api` query
//! module): `Null` and missing fields first, enum columns by declaration order, `Bytes` byte by
//! byte, everything else by [`compare_key`]. `Set` tags and `Map` entries are kept in
//! [`compare_key`] order of their elements and keys (D61, D79).

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::rc::Rc;

use ostrel_db::api::{
    AppliedSchema, BoxFuture, Capabilities, CmpOp, CollectionChange, CollectionState, Connection,
    DbError, Dir, Driver, EnumColumn, Expr, FieldId, Hop, MigrationPlan, ModelId, NewOp, Path,
    Query, Row, RowId, Rows, ServerSeq, SetChange, SetTag, StoredOp, Transaction, Value, Write,
    check_writes, compare_key,
};

/// The URL scheme this driver serves.
pub const SCHEME: &str = "memory:";

/// One stored row. `deleted` rows are tombstones: they keep the id taken (rule "ids are never
/// reused") and hold no fields or collections.
#[derive(Clone, Debug)]
struct Stored {
    model: ModelId,
    version: u64,
    deleted: bool,
    fields: BTreeMap<FieldId, Value>,
    collections: BTreeMap<FieldId, CollectionState>,
}

/// Everything a transaction or a migration may change. Cloned at the start of every
/// transaction.
#[derive(Clone, Debug, Default)]
struct State {
    rows: BTreeMap<RowId, Stored>,
    /// The op log. The op at index `i` has the position `i + 1`.
    ops: Vec<StoredOp>,
    sequences: BTreeMap<String, u64>,
    /// What the last successful migration stored; `None` before the first one.
    schema: Option<AppliedSchema>,
    /// Enum columns of the applied schema, stored with it (D87).
    enums: Vec<EnumColumn>,
}

impl State {
    /// Declaration order of an enum column, if `(model, field)` is one.
    fn variants(&self, model: ModelId, field: FieldId) -> Option<&[String]> {
        self.enums
            .iter()
            .find(|c| c.model == model && c.field == field)
            .map(|c| c.variants.as_slice())
    }
}

/// The committed state plus a counter of commits that changed it.
#[derive(Debug, Default)]
struct Database {
    state: State,
    generation: u64,
}

/// Entry point of the in memory driver.
#[derive(Debug, Default)]
pub struct MemoryDriver {
    db: Rc<RefCell<Database>>,
}

impl MemoryDriver {
    /// A driver with a new, empty database.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Driver for MemoryDriver {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transactions: true,
            sequences: true,
            subqueries: false,
            json_fields: false,
            max_in_list: u32::MAX,
        }
    }

    fn connect<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Box<dyn Connection>, DbError>> {
        Box::pin(async move {
            if !url.starts_with(SCHEME) {
                return Err(DbError::Unsupported("url scheme, expected memory:"));
            }
            let db = Rc::clone(&self.db);
            Ok(Box::new(MemoryConnection { db }) as Box<dyn Connection>)
        })
    }
}

/// An open connection to the shared database of one [`MemoryDriver`].
#[derive(Debug)]
pub struct MemoryConnection {
    db: Rc<RefCell<Database>>,
}

impl Connection for MemoryConnection {
    fn migrate<'a>(&'a mut self, plan: &'a MigrationPlan) -> BoxFuture<'a, Result<(), DbError>> {
        Box::pin(async move {
            plan.check()?;
            let mut db = self.db.borrow_mut();
            if plan.from != db.state.schema.as_ref().map(|a| a.hash) {
                return Err(DbError::SchemaMismatch);
            }
            // A plan from the stored schema to itself with no steps changes nothing, not even
            // the stored schema text or the enum columns.
            if plan.from == Some(plan.to) && plan.steps.is_empty() {
                return Ok(());
            }
            // Rows are schema free maps, so there is nothing to create. A step this driver does
            // not know must not be skipped silently once steps exist.
            if let Some(step) = plan.steps.first() {
                match *step {}
            }
            db.state.schema = Some(AppliedSchema {
                hash: plan.to,
                schema: plan.schema.clone(),
            });
            db.state.enums = plan.enums.clone();
            // An open transaction copied the old schema; it must not write it back.
            db.generation += 1;
            Ok(())
        })
    }

    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move { run_query(&self.db.borrow().state, q) })
    }

    fn applied_schema<'a>(&'a mut self) -> BoxFuture<'a, Result<Option<AppliedSchema>, DbError>> {
        Box::pin(async move { Ok(self.db.borrow().state.schema.clone()) })
    }

    fn begin<'c>(&'c mut self) -> BoxFuture<'c, Result<Box<dyn Transaction<'c> + 'c>, DbError>> {
        Box::pin(async move {
            let (work, base) = {
                let db = self.db.borrow();
                (db.state.clone(), db.generation)
            };
            let tx = MemoryTransaction {
                conn: self,
                work,
                base,
                dirty: false,
                failed: false,
            };
            Ok(Box::new(tx) as Box<dyn Transaction<'c> + 'c>)
        })
    }
}

/// A transaction: a private copy of the state, written back on commit.
struct MemoryTransaction<'c> {
    conn: &'c mut MemoryConnection,
    work: State,
    /// Generation of the committed state the copy was taken from.
    base: u64,
    /// Whether this transaction changed anything.
    dirty: bool,
    /// Whether a call failed; then only rollback is allowed.
    failed: bool,
}

impl MemoryTransaction<'_> {
    /// Runs `f` on the working copy unless an earlier call failed, and remembers a failure.
    fn step<T>(&mut self, f: impl FnOnce(&mut State) -> Result<T, DbError>) -> Result<T, DbError> {
        if self.failed {
            return Err(failed_earlier());
        }
        let result = f(&mut self.work);
        self.failed = result.is_err();
        result
    }
}

impl<'c> Transaction<'c> for MemoryTransaction<'c> {
    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move { self.step(|s| run_query(s, q)) })
    }

    fn apply<'a>(&'a mut self, w: &'a [Write]) -> BoxFuture<'a, Result<(), DbError>> {
        self.dirty = true;
        Box::pin(async move {
            self.step(|s| {
                // Both checks run before the first write, so a refused batch changes nothing.
                check_writes(w)?;
                check_enums(s, w)?;
                w.iter().try_for_each(|w| apply_write(s, w))
            })
        })
    }

    fn append_ops<'a>(&'a mut self, ops: &'a [NewOp]) -> BoxFuture<'a, Result<ServerSeq, DbError>> {
        self.dirty = true;
        Box::pin(async move {
            self.step(|s| {
                // Every id is checked before the first op is appended, so a refused call
                // appends nothing.
                for (i, op) in ops.iter().enumerate() {
                    let in_log = s.ops.iter().any(|o| o.op.id == op.id);
                    let earlier = ops.get(..i).unwrap_or_default();
                    if in_log || earlier.iter().any(|o| o.id == op.id) {
                        return Err(DbError::Conflict);
                    }
                }
                for op in ops {
                    // The driver assigns the position.
                    let seq = ServerSeq(s.ops.len() as u64 + 1);
                    s.ops.push(StoredOp {
                        seq,
                        op: op.clone(),
                    });
                }
                Ok(ServerSeq(s.ops.len() as u64))
            })
        })
    }

    fn ops_since<'a>(
        &'a mut self,
        after: ServerSeq,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<StoredOp>, DbError>> {
        Box::pin(async move {
            self.step(|s| {
                // Positions start at 1, so the ops after position `after` start at index `after`.
                let start = usize::try_from(after.0).unwrap_or(usize::MAX);
                let rest = s.ops.get(start..).unwrap_or_default();
                Ok(rest.iter().take(limit as usize).cloned().collect())
            })
        })
    }

    fn next_in_sequence<'a>(&'a mut self, key: &'a str) -> BoxFuture<'a, Result<u64, DbError>> {
        self.dirty = true;
        Box::pin(async move {
            self.step(|s| {
                let n = s.sequences.entry(key.to_owned()).or_insert(0);
                *n = n
                    .checked_add(1)
                    .ok_or(DbError::Backend("sequence exhausted".into()))?;
                Ok(*n)
            })
        })
    }

    fn commit(self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>> {
        Box::pin(async move {
            if self.failed {
                return Err(failed_earlier());
            }
            if !self.dirty {
                return Ok(());
            }
            let mut db = self.conn.db.borrow_mut();
            if db.generation != self.base {
                return Err(DbError::Backend("concurrent commit, retry".into()));
            }
            db.state = self.work;
            db.generation += 1;
            Ok(())
        })
    }

    fn rollback(self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>> {
        // The working copy is simply dropped; dropping without commit does the same.
        Box::pin(async move { Ok(()) })
    }
}

fn failed_earlier() -> DbError {
    DbError::Backend("transaction failed earlier, roll it back".into())
}

// ---------------------------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------------------------

/// Applies one write to the state, following the rules of [`Write`].
fn apply_write(s: &mut State, w: &Write) -> Result<(), DbError> {
    match w {
        Write::Insert {
            model,
            row,
            fields,
            collections,
        } => {
            // The id is taken while a live row or a tombstone holds it, in any model.
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
            let r = live_row(s, *model, *row, *expect_version)?;
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
            let r = live_row(s, *model, *row, *expect_version)?;
            r.version += 1;
            r.deleted = true;
            r.fields.clear();
            r.collections.clear();
        }
    }
    Ok(())
}

/// The live row `row` of `model`, checked against `expect_version`.
fn live_row(
    s: &mut State,
    model: ModelId,
    row: RowId,
    expect_version: u64,
) -> Result<&mut Stored, DbError> {
    let r = s
        .rows
        .get_mut(&row)
        .filter(|r| r.model == model && !r.deleted)
        .ok_or(DbError::NotFound)?;
    if r.version != expect_version {
        return Err(DbError::VersionMismatch);
    }
    Ok(r)
}

/// Applies the changes of one collection field. A field stored as the other collection kind
/// starts over as the new kind; the compiler never emits that.
fn apply_collection(stored: &mut Stored, field: FieldId, change: &CollectionChange) {
    let state = stored
        .collections
        .entry(field)
        .or_insert_with(|| match change {
            CollectionChange::Set(_) => CollectionState::Set(Vec::new()),
            CollectionChange::Map(_) => CollectionState::Map(Vec::new()),
        });
    match (change, state) {
        (CollectionChange::Set(changes), CollectionState::Set(tags)) => {
            changes.iter().for_each(|c| apply_set_change(tags, c));
            tags.sort_by(|a, b| compare_key(&a.elem, &b.elem).then(a.tag.cmp(&b.tag)));
        }
        (CollectionChange::Map(entries), CollectionState::Map(stored_entries)) => {
            for e in entries {
                stored_entries.retain(|x| compare_key(&x.key, &e.key).is_ne());
                stored_entries.push(e.clone());
            }
            stored_entries.sort_by(|a, b| compare_key(&a.key, &b.key));
        }
        (_, state) => {
            *state = match change {
                CollectionChange::Set(_) => CollectionState::Set(Vec::new()),
                CollectionChange::Map(_) => CollectionState::Map(Vec::new()),
            };
            apply_collection(stored, field, change);
        }
    }
}

/// One tag change of a `Set` field: at most one live tag per (element, replica) (D49).
fn apply_set_change(tags: &mut Vec<SetTag>, change: &SetChange) {
    match change {
        SetChange::Add { elem, tag } => {
            let same =
                |t: &SetTag| compare_key(&t.elem, elem).is_eq() && t.tag.replica == tag.replica;
            match tags.iter_mut().find(|t| same(t)) {
                // Replay of this add or of an older one of the same replica.
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

/// Refuses values of enum columns, and of `Map` fields whose values are enums, that are not
/// declared variants (contract rules in `ostrel_db::api`). `Set` elements and `Map` keys are
/// not checked (D87).
fn check_enums(s: &State, writes: &[Write]) -> Result<(), DbError> {
    let declared = |model: ModelId, f: FieldId, v: &Value| match s.variants(model, f) {
        Some(vs) if *v != Value::Null => ordinal(vs, v).is_some(),
        _ => true,
    };
    for w in writes {
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

// ---------------------------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------------------------

/// Live rows of the queried model in query order, after filter and cursor, at most `limit`.
fn run_query(s: &State, q: &Query) -> Result<Rows, DbError> {
    q.check(&s.enums)?;
    let mut keyed: Vec<(Vec<Value>, Row)> = Vec::new();
    for (id, r) in &s.rows {
        if r.model != q.model || r.deleted {
            continue;
        }
        if let Some(f) = &q.filter
            && !matches!(eval(s, *id, r, f), Ok(v) if truthy(&v))
        {
            continue;
        }
        let values: Vec<Value> = q
            .order
            .iter()
            .map(|(f, _)| r.fields.get(f).cloned().unwrap_or(Value::Null))
            .collect();
        if let Some(c) = &q.after
            && compare_pos(s, q, (&values, *id), (&c.values, c.id)).is_le()
        {
            continue;
        }
        let row = Row {
            id: *id,
            version: r.version,
            fields: r.fields.iter().map(|(f, v)| (*f, v.clone())).collect(),
            collections: r.collections.iter().map(|(f, c)| (*f, c.clone())).collect(),
        };
        keyed.push((values, row));
    }
    keyed.sort_by(|(va, a), (vb, b)| compare_pos(s, q, (va, a.id), (vb, b.id)));
    keyed.truncate(q.limit.map_or(usize::MAX, |n| n as usize));
    Ok(Rows(keyed.into_iter().map(|(_, r)| r).collect()))
}

/// Query order of two row positions `(sort key values, id)`. [`Query::check`] has made sure a
/// cursor has one value per sort key.
fn compare_pos(s: &State, q: &Query, a: (&[Value], RowId), b: (&[Value], RowId)) -> Ordering {
    for ((f, dir), (x, y)) in q.order.iter().zip(a.0.iter().zip(b.0)) {
        let o = directed(*dir, column_order(s.variants(q.model, *f), x, y));
        if o.is_ne() {
            return o;
        }
    }
    let tie = q.order.last().map_or(Dir::Asc, |(_, d)| *d);
    directed(tie, a.1.cmp(&b.1))
}

fn directed(dir: Dir, o: Ordering) -> Ordering {
    match dir {
        Dir::Asc => o,
        Dir::Desc => o.reverse(),
    }
}

/// Ordinal of an enum value in declaration order.
fn ordinal(variants: &[String], v: &Value) -> Option<usize> {
    match v {
        Value::Enum(name) => variants.iter().position(|x| x == name),
        _ => None,
    }
}

/// Column order of the contract. `variants` is the declaration order when the column is enum
/// typed. Writes refuse undeclared variants, so the name fallback only keeps the order total.
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

/// The enum column an operand of a comparison names, if any. [`Query::check`] has refused two
/// enum columns with different variants (D87), so either side gives the same order.
fn enum_side<'s>(s: &'s State, model: ModelId, e: &Expr) -> Option<&'s [String]> {
    let target = |p: &Path| p.hops.last().map_or(model, |h| h.model);
    match e {
        Expr::Field(p) => s.variants(target(p), p.field),
        Expr::MapGet { map, .. } => s.variants(target(map), map.field),
        Expr::Coalesce(a, _) => enum_side(s, model, a),
        _ => None,
    }
}

/// Ordered comparison of two non null values; `None` makes the comparison false.
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

/// Marker: a hop did not reach a live row, so the whole filter is false (fail closed).
struct HopFailed;

/// Id and row reached from the row `id` through `hops`.
fn follow<'s>(
    s: &'s State,
    id: RowId,
    start: &'s Stored,
    hops: &[Hop],
) -> Result<(RowId, &'s Stored), HopFailed> {
    let (mut id, mut row) = (id, start);
    for hop in hops {
        let Some(Value::Ref(next)) = row.fields.get(&hop.field) else {
            return Err(HopFailed);
        };
        row = s
            .rows
            .get(next)
            .filter(|r| r.model == hop.model && !r.deleted)
            .ok_or(HopFailed)?;
        id = *next;
    }
    Ok((id, row))
}

fn truthy(v: &Value) -> bool {
    *v == Value::Bool(true)
}

/// `Eq` of the contract: equal under [`compare_key`], and `Null` equals only `Null`.
fn equal(x: &Value, y: &Value) -> bool {
    compare_key(x, y).is_eq() && (*x == Value::Null) == (*y == Value::Null)
}

/// Evaluates a filter expression on the row `id`. [`Query::check`] bounds the size of the
/// expression, so the recursion is bounded too.
fn eval(s: &State, id: RowId, row: &Stored, e: &Expr) -> Result<Value, HopFailed> {
    let reach = |p: &Path| -> Result<&Stored, HopFailed> { Ok(follow(s, id, row, &p.hops)?.1) };
    Ok(match e {
        Expr::Const(v) => v.clone(),
        Expr::Field(p) => reach(p)?
            .fields
            .get(&p.field)
            .cloned()
            .unwrap_or(Value::Null),
        Expr::RowId(hops) => Value::Ref(follow(s, id, row, hops)?.0),
        Expr::Cmp(op, a, b) => {
            let (x, y) = (eval(s, id, row, a)?, eval(s, id, row, b)?);
            let r = match op {
                CmpOp::Eq => equal(&x, &y),
                CmpOp::Ne => !equal(&x, &y),
                _ if x == Value::Null || y == Value::Null => false,
                _ => ordered(s, row.model, a, b, &x, &y).is_some_and(|o| match op {
                    CmpOp::Lt => o.is_lt(),
                    CmpOp::Le => o.is_le(),
                    CmpOp::Gt => o.is_gt(),
                    _ => o.is_ge(),
                }),
            };
            Value::Bool(r)
        }
        // Every operand is evaluated, so a failed hop anywhere fails the whole filter.
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
            Value::Bool(match reach(set)?.collections.get(&set.field) {
                Some(CollectionState::Set(tags)) => {
                    tags.iter().any(|t| compare_key(&t.elem, &x).is_eq())
                }
                _ => false,
            })
        }
        Expr::InMap { key, map } => {
            let k = eval(s, id, row, key)?;
            Value::Bool(map_get(reach(map)?.collections.get(&map.field), &k).is_some())
        }
        Expr::MapGet { map, key } => {
            let k = eval(s, id, row, key)?;
            map_get(reach(map)?.collections.get(&map.field), &k).unwrap_or(Value::Null)
        }
        Expr::Coalesce(a, c) => match eval(s, id, row, a)? {
            Value::Null => c.clone(),
            v => v,
        },
    })
}

/// Value of the not removed entry for `key`.
fn map_get(state: Option<&CollectionState>, key: &Value) -> Option<Value> {
    match state {
        Some(CollectionState::Map(entries)) => entries
            .iter()
            .find(|e| compare_key(&e.key, key).is_eq())
            .and_then(|e| e.value.clone()),
        _ => None,
    }
}
