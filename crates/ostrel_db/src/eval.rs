//! Query evaluation for drivers that keep rows outside SQL (D129, ARCHITECTURE 6.2).
//!
//! A driver that cannot translate a [`Query`] into its own query language loads the live rows
//! of the queried model as [`LiveRow`]s, gives [`run`] a [`Lookup`] for rows reached through
//! hops, and gets the result rows back. The rules are those of the contract (`api` query
//! module): two valued logic, fail closed on a hop that does not reach a live row, enum columns
//! by declaration order, `Bytes` byte by byte, everything else by [`compare_key`], keyset
//! cursor and limit. [`check_enums`], [`apply_collection`] and the row transitions of
//! [`LiveRow`] (D139) are the matching write rules, so such a driver keeps only its storage
//! (including which ids are taken), the trait implementations and its op log.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::api::{
    CmpOp, CollectionChange, CollectionState, DbError, Dir, EnumColumn, Expr, FieldId, Hop,
    ModelId, Path, Query, Row, RowId, Rows, SetChange, SetTag, Value, Write, compare_key,
};

/// A live row as a driver holds it. A field that was never written is absent.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveRow {
    pub model: ModelId,
    pub version: u64,
    pub fields: BTreeMap<FieldId, Value>,
    /// `Set` tags and `Map` entries in [`compare_key`] order of elements and keys (D61, D79).
    pub collections: BTreeMap<FieldId, CollectionState>,
}

/// The rules of [`Write`] on one row that do not depend on where a driver stores its rows
/// (D139). Whether an id is taken, by a live row or by a deleted one, and removing a deleted row
/// are rules of the driver's storage and stay in the driver.
impl LiveRow {
    /// The row a [`Write::Insert`] creates: version 1, its fields and its collection changes
    /// applied to empty collections.
    pub fn inserted(
        model: ModelId,
        fields: &[(FieldId, Value)],
        collections: &[(FieldId, CollectionChange)],
    ) -> Self {
        let mut row = LiveRow {
            model,
            version: 1,
            fields: fields.iter().cloned().collect(),
            collections: BTreeMap::new(),
        };
        for (f, c) in collections {
            apply_collection(&mut row.collections, *f, c);
        }
        row
    }

    /// Checks the live row stored under the id of a [`Write::Update`] or [`Write::Delete`]: a
    /// row of another model is [`DbError::NotFound`], like a missing one, and a row whose
    /// version is not `expect_version` is [`DbError::VersionMismatch`].
    pub fn check(&self, model: ModelId, expect_version: u64) -> Result<(), DbError> {
        if self.model != model {
            return Err(DbError::NotFound);
        }
        if self.version != expect_version {
            return Err(DbError::VersionMismatch);
        }
        Ok(())
    }

    /// Applies a [`Write::Update`] that passed [`LiveRow::check`]: the version grows by one,
    /// the given fields replace the stored ones and the collection changes are applied.
    pub fn update(
        &mut self,
        fields: &[(FieldId, Value)],
        collections: &[(FieldId, CollectionChange)],
    ) {
        self.version += 1;
        self.fields.extend(fields.iter().cloned());
        for (f, c) in collections {
            apply_collection(&mut self.collections, *f, c);
        }
    }
}

/// Where hops read rows from.
pub trait Lookup {
    /// The live row with id `id`, in any model, or `None` when no live row has that id (never
    /// written, or deleted).
    fn live(&self, id: RowId) -> Result<Option<Rc<LiveRow>>, DbError>;
}

/// Live rows of the queried model in query order, after filter and cursor, at most `limit`.
///
/// `rows` must be exactly the live rows of `q.model`, in any order. The query is checked with
/// [`Query::check`] first, so an invalid query is refused before any row is looked at. An error
/// of `look` ends the query with that error; a hop that finds no live row only drops the row.
pub fn run(
    enums: &[EnumColumn],
    look: &dyn Lookup,
    q: &Query,
    rows: Vec<(RowId, Rc<LiveRow>)>,
) -> Result<Rows, DbError> {
    q.check(enums)?;
    let ctx = Ctx { enums, look };
    let mut keyed: Vec<(Vec<Value>, RowId, Rc<LiveRow>)> = Vec::new();
    for (id, r) in rows {
        if let Some(f) = &q.filter {
            match ctx.eval(id, &r, f) {
                Ok(v) if truthy(&v) => {}
                Ok(_) | Err(Stop::Hop) => continue,
                Err(Stop::Db(e)) => return Err(e),
            }
        }
        let values: Vec<Value> = q
            .order
            .iter()
            .map(|(f, _)| r.fields.get(f).cloned().unwrap_or(Value::Null))
            .collect();
        if let Some(c) = &q.after
            && ctx.compare_pos(q, (&values, id), (&c.values, c.id)).is_le()
        {
            continue;
        }
        keyed.push((values, id, r));
    }
    keyed.sort_by(|(va, a, _), (vb, b, _)| ctx.compare_pos(q, (va, *a), (vb, *b)));
    keyed.truncate(q.limit.map_or(usize::MAX, |n| n as usize));
    Ok(Rows(
        keyed
            .into_iter()
            .map(|(_, id, r)| Row {
                id,
                version: r.version,
                fields: r.fields.iter().map(|(f, v)| (*f, v.clone())).collect(),
                collections: r.collections.iter().map(|(f, c)| (*f, c.clone())).collect(),
            })
            .collect(),
    ))
}

/// Refuses values of enum columns, and of `Map` fields whose values are enums, that are not
/// declared variants (contract rules in `api`). `Null`, `Set` elements and `Map` keys are not
/// checked (D87). A driver calls it with [`check_writes`](crate::api::check_writes) before the
/// first write of a batch.
pub fn check_enums(enums: &[EnumColumn], writes: &[Write]) -> Result<(), DbError> {
    let declared = |model: ModelId, f: FieldId, v: &Value| match variants(enums, model, f) {
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

/// Applies the changes of one collection field to the collections of a row, keeping the order
/// of [`LiveRow::collections`]. A field stored as the other collection kind starts over as the
/// new kind; the compiler never emits that.
pub fn apply_collection(
    collections: &mut BTreeMap<FieldId, CollectionState>,
    field: FieldId,
    change: &CollectionChange,
) {
    let state = collections.entry(field).or_insert_with(|| empty(change));
    if matches!(
        (change, &*state),
        (CollectionChange::Set(_), CollectionState::Map(_))
            | (CollectionChange::Map(_), CollectionState::Set(_))
    ) {
        *state = empty(change);
    }
    match (change, state) {
        (CollectionChange::Set(changes), CollectionState::Set(tags)) => {
            changes.iter().for_each(|c| apply_set_change(tags, c));
            tags.sort_by(|a, b| compare_key(&a.elem, &b.elem).then(a.tag.cmp(&b.tag)));
        }
        (CollectionChange::Map(entries), CollectionState::Map(stored)) => {
            for e in entries {
                stored.retain(|x| compare_key(&x.key, &e.key).is_ne());
                stored.push(e.clone());
            }
            stored.sort_by(|a, b| compare_key(&a.key, &b.key));
        }
        // Unreachable: the kind was made to match above.
        _ => {}
    }
}

fn empty(change: &CollectionChange) -> CollectionState {
    match change {
        CollectionChange::Set(_) => CollectionState::Set(Vec::new()),
        CollectionChange::Map(_) => CollectionState::Map(Vec::new()),
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
        // Only the full tuple (element, replica, seq) names a live tag (D96).
        SetChange::Remove { elem, tag } => {
            tags.retain(|t| !(compare_key(&t.elem, elem).is_eq() && t.tag == *tag));
        }
    }
}

/// Declaration order of an enum column, if `(model, field)` is one.
fn variants(enums: &[EnumColumn], model: ModelId, field: FieldId) -> Option<&[String]> {
    enums
        .iter()
        .find(|c| c.model == model && c.field == field)
        .map(|c| c.variants.as_slice())
}

/// Ordinal of an enum value in declaration order.
fn ordinal(variants: &[String], v: &Value) -> Option<usize> {
    match v {
        Value::Enum(name) => variants.iter().position(|x| x == name),
        _ => None,
    }
}

/// Why evaluation stopped early.
enum Stop {
    /// A hop did not reach a live row, so the whole filter is false (fail closed).
    Hop,
    /// Reading a row through a hop failed.
    Db(DbError),
}

impl From<DbError> for Stop {
    fn from(e: DbError) -> Self {
        Stop::Db(e)
    }
}

struct Ctx<'a> {
    enums: &'a [EnumColumn],
    look: &'a dyn Lookup,
}

fn directed(dir: Dir, o: Ordering) -> Ordering {
    match dir {
        Dir::Asc => o,
        Dir::Desc => o.reverse(),
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

fn truthy(v: &Value) -> bool {
    *v == Value::Bool(true)
}

/// `Eq` of the contract: equal under [`compare_key`], and `Null` equals only `Null`.
fn equal(x: &Value, y: &Value) -> bool {
    compare_key(x, y).is_eq() && (*x == Value::Null) == (*y == Value::Null)
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

impl Ctx<'_> {
    /// Query order of two row positions `(sort key values, id)`. [`Query::check`] has made sure
    /// a cursor has one value per sort key.
    fn compare_pos(&self, q: &Query, a: (&[Value], RowId), b: (&[Value], RowId)) -> Ordering {
        for ((f, dir), (x, y)) in q.order.iter().zip(a.0.iter().zip(b.0)) {
            let o = directed(*dir, column_order(variants(self.enums, q.model, *f), x, y));
            if o.is_ne() {
                return o;
            }
        }
        let tie = q.order.last().map_or(Dir::Asc, |(_, d)| *d);
        directed(tie, a.1.cmp(&b.1))
    }

    /// The enum column an operand of a comparison names, if any. [`Query::check`] has refused
    /// two enum columns with different variants (D87), so either side gives the same order.
    fn enum_side(&self, model: ModelId, e: &Expr) -> Option<&[String]> {
        let target = |p: &Path| p.hops.last().map_or(model, |h| h.model);
        match e {
            Expr::Field(p) => variants(self.enums, target(p), p.field),
            Expr::MapGet { map, .. } => variants(self.enums, target(map), map.field),
            Expr::Coalesce(a, _) => self.enum_side(model, a),
            _ => None,
        }
    }

    /// Ordered comparison of two non null values; `None` makes the comparison false.
    fn ordered(
        &self,
        model: ModelId,
        a: &Expr,
        b: &Expr,
        x: &Value,
        y: &Value,
    ) -> Option<Ordering> {
        if let (Value::Enum(_), Value::Enum(_)) = (x, y) {
            let vs = self
                .enum_side(model, a)
                .or_else(|| self.enum_side(model, b))?;
            return Some(ordinal(vs, x)?.cmp(&ordinal(vs, y)?));
        }
        Some(column_order(None, x, y))
    }

    /// Id and row reached from the row `id` through `hops`.
    fn follow(
        &self,
        id: RowId,
        start: &Rc<LiveRow>,
        hops: &[Hop],
    ) -> Result<(RowId, Rc<LiveRow>), Stop> {
        let (mut id, mut row) = (id, Rc::clone(start));
        for hop in hops {
            let Some(Value::Ref(next)) = row.fields.get(&hop.field) else {
                return Err(Stop::Hop);
            };
            let next = *next;
            row = self
                .look
                .live(next)?
                .filter(|r| r.model == hop.model)
                .ok_or(Stop::Hop)?;
            id = next;
        }
        Ok((id, row))
    }

    /// Evaluates a filter expression on the row `id`. [`Query::check`] bounds the size of the
    /// expression, so the recursion is bounded too.
    fn eval(&self, id: RowId, row: &Rc<LiveRow>, e: &Expr) -> Result<Value, Stop> {
        let reach =
            |p: &Path| -> Result<Rc<LiveRow>, Stop> { Ok(self.follow(id, row, &p.hops)?.1) };
        Ok(match e {
            Expr::Const(v) => v.clone(),
            Expr::Field(p) => reach(p)?
                .fields
                .get(&p.field)
                .cloned()
                .unwrap_or(Value::Null),
            Expr::RowId(hops) => Value::Ref(self.follow(id, row, hops)?.0),
            Expr::Cmp(op, a, b) => {
                let (x, y) = (self.eval(id, row, a)?, self.eval(id, row, b)?);
                let r = match op {
                    CmpOp::Eq => equal(&x, &y),
                    CmpOp::Ne => !equal(&x, &y),
                    _ if x == Value::Null || y == Value::Null => false,
                    _ => self
                        .ordered(row.model, a, b, &x, &y)
                        .is_some_and(|o| match op {
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
                    all &= truthy(&self.eval(id, row, i)?);
                }
                Value::Bool(all)
            }
            Expr::Or(items) => {
                let mut any = false;
                for i in items {
                    any |= truthy(&self.eval(id, row, i)?);
                }
                Value::Bool(any)
            }
            Expr::Not(a) => Value::Bool(!truthy(&self.eval(id, row, a)?)),
            Expr::InSet { elem, set } => {
                let x = self.eval(id, row, elem)?;
                Value::Bool(match reach(set)?.collections.get(&set.field) {
                    Some(CollectionState::Set(tags)) => {
                        tags.iter().any(|t| compare_key(&t.elem, &x).is_eq())
                    }
                    _ => false,
                })
            }
            Expr::InMap { key, map } => {
                let k = self.eval(id, row, key)?;
                Value::Bool(map_get(reach(map)?.collections.get(&map.field), &k).is_some())
            }
            Expr::MapGet { map, key } => {
                let k = self.eval(id, row, key)?;
                map_get(reach(map)?.collections.get(&map.field), &k).unwrap_or(Value::Null)
            }
            Expr::Coalesce(a, c) => match self.eval(id, row, a)? {
                Value::Null => c.clone(),
                v => v,
            },
        })
    }
}
