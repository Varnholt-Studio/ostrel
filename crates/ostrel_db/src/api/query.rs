//! Typed queries: the logical query IR a driver translates (ARCHITECTURE 6.1, 9).
//!
//! # Column order
//!
//! Sorting, ordered comparisons and cursors use one order per field type: [`Value::Null`] (and
//! a field that was never written) before every other value; `false` before `true`; `Int`,
//! `Float` and `Time` numerically; `Text` and `Rank` by code point (D50); `Bytes` byte by byte;
//! references by [`RowId`]; enum values by their declaration order (D79), which the driver
//! knows from [`MigrationPlan::enums`](super::MigrationPlan::enums) of the applied schema and
//! never from the variant name. `List`, `Set` and `Map` values are not sortable. A key sorted
//! [`Dir::Desc`] is the exact reverse, so `Null` comes last there. On PostgreSQL this means
//! `NULLS FIRST` for ascending and `NULLS LAST` for descending keys.
//!
//! This row order differs from the element and key order of `Set` and `Map`
//! ([`compare_key`](super::compare_key), D61) in two places, both intended (D79): enum values
//! (declaration order here, variant name there) and `Bytes` (byte by byte here, base64url
//! text there, so `[0x00]` sorts before `[0xF8]` here and after it there).
//!
//! An ordered comparison ([`CmpOp::Lt`] and the others) of two enum values uses the declaration
//! order of the enum column on one side: an [`Expr::Field`] or [`Expr::MapGet`] of an enum
//! column, also inside [`Expr::Coalesce`]. It is false when no side names an enum column or a
//! value is not a variant of that column, like a comparison with `Null`. The planner never
//! emits such a comparison; the rule only makes the result defined.
//!
//! # Filter semantics
//!
//! A filter selects a row when it evaluates to `Bool(true)`. Logic is two valued: wherever a
//! boolean is expected (the filter itself and the operands of `And`, `Or` and `Not`), any value
//! other than `Bool(true)`, including `Null`, counts as false. So `Not` of a comparison with
//! `Null` is true, unlike plain SQL. A path whose hop does not reach a live row of the hop's
//! model (the reference is `Null`, the row was deleted or never existed) makes the whole filter
//! false for that row, also under `Not` and `Or` (fail closed, docs/db-mapping.md A5).

use super::{DbError, FieldId, ModelId, RowId, Value};

/// Largest number of [`Expr`] nodes in one filter (ARCHITECTURE 5.9, "Filter size").
pub const MAX_FILTER_NODES: usize = 64;

/// Largest number of reference hops in one [`Path`] (ARCHITECTURE 9).
pub const MAX_HOPS: usize = 2;

/// Sort direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Asc,
    Desc,
}

/// A query over the live rows of one model.
///
/// Rows are ordered by the keys of `order` in priority order, each in its direction and in
/// column order (module documentation). Rows equal on all keys are ordered by id in the
/// direction of the last key, ascending when there is no key, so the order is total.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub model: ModelId,
    /// Only rows for which the filter is true; every live row of the model when `None`.
    pub filter: Option<Expr>,
    /// Sort keys in priority order.
    pub order: Vec<(FieldId, Dir)>,
    /// At most this many rows, counted after filter and cursor. `Some(0)` returns no rows.
    pub limit: Option<u32>,
    /// Only rows strictly after this position in query order.
    pub after: Option<Cursor>,
}

/// A position in query order: the sort key values and id of a row, usually the last row of
/// the previous page. The row itself need not exist any more; rows are compared with the
/// position, not looked up by id.
#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    /// One value per entry of [`Query::order`], in the same order.
    pub values: Vec<Value>,
    pub id: RowId,
}

/// One reference hop: the reference field of the current row and the model it points to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hop {
    pub field: FieldId,
    pub model: ModelId,
}

/// A field of the queried row (`hops` empty) or of a row reached through up to [`MAX_HOPS`]
/// references.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub hops: Vec<Hop>,
    pub field: FieldId,
}

/// Comparison operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    /// Equal under [`compare_key`](super::compare_key); `Null` equals only `Null`.
    Eq,
    /// Not [`CmpOp::Eq`].
    Ne,
    /// Ordered comparisons in column order; false when either side is `Null`.
    Lt,
    Le,
    Gt,
    Ge,
}

/// A filter expression of the pushdown subset (ARCHITECTURE 9). The runtime replaces `me` and
/// `signed` by constants before the query reaches a driver.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// A constant. `List`, `Set` and `Map` constants are not allowed.
    Const(Value),
    /// The value of a column field, `Null` if it was never written.
    Field(Path),
    /// The id of the queried row (no hops) or of the row reached by the hops, as `Value::Ref`.
    RowId(Vec<Hop>),
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    /// True when every operand is true; true for no operands.
    And(Vec<Expr>),
    /// True when some operand is true; false for no operands.
    Or(Vec<Expr>),
    Not(Box<Expr>),
    /// `elem in setField`: true when some live tag of the set has an element equal to `elem`.
    InSet {
        elem: Box<Expr>,
        set: Path,
    },
    /// `key in mapField`: true when the map has a not removed entry for `key`.
    InMap {
        key: Box<Expr>,
        map: Path,
    },
    /// `mapField[key]`: the value of the not removed entry for `key`, otherwise `Null`.
    MapGet {
        map: Path,
        key: Box<Expr>,
    },
    /// `e ?? c`: the value of `e` unless it is `Null`, otherwise `c`.
    Coalesce(Box<Expr>, Value),
}

impl Query {
    /// All live rows of `model` in id order.
    pub fn all(model: ModelId) -> Self {
        Self {
            model,
            filter: None,
            order: Vec::new(),
            limit: None,
            after: None,
        }
    }

    /// Checks the rules a driver must not have to guess about: at most [`MAX_FILTER_NODES`]
    /// filter nodes, at most [`MAX_HOPS`] hops per path, no collection constants, and one
    /// cursor value per sort key.
    pub fn check(&self) -> Result<(), DbError> {
        if let Some(cursor) = &self.after
            && cursor.values.len() != self.order.len()
        {
            return Err(DbError::Invalid("cursor values do not match the sort keys"));
        }
        let Some(filter) = &self.filter else {
            return Ok(());
        };
        // Iterative walk: a hostile filter may be nested far deeper than the node limit, and
        // the walk stops as soon as the limit is passed.
        let mut stack = vec![filter];
        let mut nodes = 0usize;
        while let Some(e) = stack.pop() {
            nodes += 1;
            if nodes > MAX_FILTER_NODES {
                return Err(DbError::Invalid("filter has too many nodes"));
            }
            match e {
                Expr::Const(v) => {
                    if matches!(v, Value::List(_) | Value::Set(_) | Value::Map(_)) {
                        return Err(DbError::Invalid("collection constant in filter"));
                    }
                }
                Expr::Field(p) => check_hops(&p.hops)?,
                Expr::RowId(hops) => check_hops(hops)?,
                Expr::Cmp(_, a, b) => {
                    stack.push(a);
                    stack.push(b);
                }
                Expr::And(items) | Expr::Or(items) => stack.extend(items.iter()),
                Expr::Not(a) => stack.push(a),
                Expr::InSet { elem: x, set: p }
                | Expr::InMap { key: x, map: p }
                | Expr::MapGet { map: p, key: x } => {
                    check_hops(&p.hops)?;
                    stack.push(x);
                }
                Expr::Coalesce(a, c) => {
                    if matches!(c, Value::List(_) | Value::Set(_) | Value::Map(_)) {
                        return Err(DbError::Invalid("collection constant in filter"));
                    }
                    stack.push(a);
                }
            }
        }
        Ok(())
    }
}

fn check_hops(hops: &[Hop]) -> Result<(), DbError> {
    if hops.len() > MAX_HOPS {
        return Err(DbError::Invalid("path has too many hops"));
    }
    Ok(())
}
