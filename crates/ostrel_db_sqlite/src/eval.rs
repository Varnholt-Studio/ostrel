//! Filter, order, cursor and limit of a [`Query`] over rows loaded from the database.
//!
//! The rows of this driver are stored schema free (crate documentation), so filters are
//! evaluated here and not translated to SQL. The rules are those of the contract
//! (`ostrel_db::api`, query module) and match the in memory driver case by case: two valued
//! logic, fail closed on a hop that does not reach a live row, enum columns by declaration order,
//! `Bytes` byte by byte, everything else by [`compare_key`].

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::rc::Rc;

use ostrel_db::api::{
    CmpOp, CollectionState, DbError, Dir, EnumColumn, Expr, FieldId, Hop, ModelId, Path, Query,
    Row, RowId, Rows, Value, compare_key,
};

/// A live row as loaded from the database.
#[derive(Clone, Debug)]
pub struct Stored {
    pub model: ModelId,
    pub version: u64,
    pub fields: BTreeMap<FieldId, Value>,
    pub collections: BTreeMap<FieldId, CollectionState>,
}

/// Where hops read rows from: the live row with an id, in any model, or `None`.
pub trait Lookup {
    fn live(&self, id: RowId) -> Result<Option<Rc<Stored>>, DbError>;
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

/// Live rows of the queried model in query order, after filter and cursor, at most `limit`.
/// `rows` are the live rows of `q.model`; [`Query::check`] has passed.
pub fn run(
    enums: &[EnumColumn],
    look: &dyn Lookup,
    q: &Query,
    rows: Vec<(RowId, Rc<Stored>)>,
) -> Result<Rows, DbError> {
    let ctx = Ctx { enums, look };
    let mut keyed: Vec<(Vec<Value>, RowId, Rc<Stored>)> = Vec::new();
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

/// Declaration order of an enum column, if `(model, field)` is one.
pub fn variants(enums: &[EnumColumn], model: ModelId, field: FieldId) -> Option<&[String]> {
    enums
        .iter()
        .find(|c| c.model == model && c.field == field)
        .map(|c| c.variants.as_slice())
}

/// Ordinal of an enum value in declaration order.
pub fn ordinal(variants: &[String], v: &Value) -> Option<usize> {
    match v {
        Value::Enum(name) => variants.iter().position(|x| x == name),
        _ => None,
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
        start: &Rc<Stored>,
        hops: &[Hop],
    ) -> Result<(RowId, Rc<Stored>), Stop> {
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
    fn eval(&self, id: RowId, row: &Rc<Stored>, e: &Expr) -> Result<Value, Stop> {
        let reach = |p: &Path| -> Result<Rc<Stored>, Stop> { Ok(self.follow(id, row, &p.hops)?.1) };
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
