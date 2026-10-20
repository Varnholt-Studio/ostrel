//! Queries: the filter of the pushdown subset (ARCHITECTURE 9) and the plan of
//! one query instruction.
//!
//! The shapes follow the query IR of the database interface (`ostrel_db::api`
//! `Query` and `Expr`) one to one, with two differences: a constant may be a
//! local of the running function, `me` or `signed` ([`Operand`]), which the
//! runtime replaces by values before the query reaches a driver; and models
//! and fields are IR ids. The server backend maps a plan to `ostrel_db` and to
//! the wire form of `Subscribe` (ARCHITECTURE 5.4) without further analysis.

use super::{FieldId, Local, ModelId};

/// Largest number of [`Filter`] nodes in one filter, the same bound as the
/// database interface (ARCHITECTURE 5.9, "Filter size").
pub const MAX_FILTER_NODES: usize = 64;

/// Largest number of reference hops in one [`Path`] (ARCHITECTURE 9).
pub const MAX_HOPS: usize = 2;

/// A value the runtime knows before the query runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operand {
    /// A constant of the program.
    Const(super::Const),
    /// The value of a local of the function that runs the query.
    Local(Local),
    /// `me`: the user row of the principal, `none` without one.
    Me,
    /// `signed`: whether the request has a signed principal.
    Signed,
}

/// One reference hop: the reference field of the current row and its model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hop {
    /// The reference field.
    pub field: FieldId,
    /// The model the field points to.
    pub model: ModelId,
}

/// A field of the queried row (`hops` empty) or of a row reached through at
/// most [`MAX_HOPS`] references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    /// The hops from the queried row.
    pub hops: Vec<Hop>,
    /// The field read at the end of the hops.
    pub field: FieldId,
}

/// Comparison operators of a filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A filter expression of the pushdown subset (ARCHITECTURE 9). Its semantics
/// is the two valued one of D125 and of `ostrel_db::api::query`: any value
/// other than `true` counts as false, a hop that reaches no live row makes the
/// whole filter false for that row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    /// A value fixed before the query runs.
    Value(Operand),
    /// A field of the row or of a row behind hops.
    Field(Path),
    /// The id of the row (no hops) or of the row behind the hops.
    RowId(Vec<Hop>),
    /// A comparison.
    Cmp(CmpOp, Box<Filter>, Box<Filter>),
    /// True when every operand is true; true for no operands.
    And(Vec<Filter>),
    /// True when some operand is true; false for no operands.
    Or(Vec<Filter>),
    /// Negation of the two valued result.
    Not(Box<Filter>),
    /// `elem in setField`.
    InSet {
        /// The searched element.
        elem: Box<Filter>,
        /// The `Set` field.
        set: Path,
    },
    /// `key in mapField`.
    InMap {
        /// The searched key.
        key: Box<Filter>,
        /// The `Map` field.
        map: Path,
    },
    /// `mapField[key]`, `none` without an entry.
    MapGet {
        /// The `Map` field.
        map: Path,
        /// The key.
        key: Box<Filter>,
    },
    /// `e ?? c`.
    Coalesce(Box<Filter>, Operand),
}

impl Filter {
    /// Number of nodes, counted like the database interface counts them: one
    /// per variant, a [`Path`] and an [`Operand`] inside a node count with it.
    pub fn nodes(&self) -> usize {
        match self {
            Filter::Value(_) | Filter::Field(_) | Filter::RowId(_) => 1,
            Filter::Cmp(_, a, b) => 1 + a.nodes() + b.nodes(),
            Filter::And(items) | Filter::Or(items) => {
                1 + items.iter().map(Filter::nodes).sum::<usize>()
            }
            Filter::Not(inner) | Filter::Coalesce(inner, _) => 1 + inner.nodes(),
            Filter::InSet { elem, .. } => 1 + elem.nodes(),
            Filter::InMap { key, .. } | Filter::MapGet { key, .. } => 1 + key.nodes(),
        }
    }
}

/// Which `see` filter a query runs under (ARCHITECTURE 5.7, 9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum See {
    /// Server, as the calling principal: the `see` rule of the model
    /// ([`super::Model::see`]), which the lowering copies here. The backend
    /// joins it to the `where` filter with `and` in the database query, so
    /// unreadable rows never leave the database; it adds nothing and leaves
    /// nothing out.
    Rule(Filter),
    /// Server, as the calling principal, for a model without a `see` rule:
    /// no row is readable (default deny, D8). The query returns no rows and
    /// reads nothing.
    DenyAll,
    /// Server, inside `as system`: no `see` filter (G13, D34).
    System,
    /// Client: the local replica, which holds only rows the server sent
    /// through the `see` filter of the session (ARCHITECTURE 5.4).
    Replica,
}

/// One query: `Model [where e] [sort f [desc]] [limit n | last n]` plus its
/// `see` filter (SYNTAX 4.5, G14).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryPlan {
    /// The queried model.
    pub model: ModelId,
    /// The `where` filter.
    pub filter: Option<Filter>,
    /// The `see` filter.
    pub see: See,
    /// The sort field and `desc`. The tie break by id in the same direction is
    /// implied; without a sort the order is by id ascending (SPEC 12.10).
    pub sort: Option<(super::FieldRef, bool)>,
    /// `limit n` or `last n` with `n` an `Int` local. A negative value at run
    /// time is the runtime error `NegativeCount` at [`QueryPlan::take_span`],
    /// checked before any data is read (D123).
    pub take: Option<(Take, Local)>,
    /// Position of the expression `n`, reported by `NegativeCount`.
    pub take_span: Option<ostrel_core::Span>,
}

/// `limit n` or `last n`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Take {
    /// The first `n` rows in sort order.
    First,
    /// The last `n` rows in sort order, still in sort order (B2-14).
    Last,
}
