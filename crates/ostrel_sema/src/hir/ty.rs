//! Types of the HIR, one variant per row of the type table in `docs/types.md`
//! (ARCHITECTURE 5.1), plus the types that only exist inside the checker.

use super::DefId;

/// The type of a value, a field, a parameter or an expression.
///
/// A field type is one of the variants marked "table"; the others appear only
/// on expressions and signatures. `Int serial` is not a type of its own: it is
/// [`Ty::Int`] on a field whose mode is [`super::FieldMode::Serial`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Ty {
    /// Table: `Bool`.
    Bool,
    /// Table: `Int`, plus or minus (2^53 minus 1) (G9).
    Int,
    /// Table: `Float`, always finite.
    Float,
    /// Table: `Text`. Whether a field merges as a sequence is the field's
    /// [`super::Merge`], not part of the type.
    Text,
    /// Table: `Time`, ms since the Unix epoch.
    Time,
    /// Table: `Bytes`.
    Bytes,
    /// Table: `Rank`, a fractional index key.
    Rank,
    /// Table: a variant of the enum declared by this definition.
    Enum(DefId),
    /// Table: a reference to a row of the model declared by this definition.
    Ref(DefId),
    /// Table: `T?`. Never nests: `T??` is reported, not built.
    Optional(Box<Ty>),
    /// Table: `Set[T]`.
    Set(Box<Ty>),
    /// Table: `Map[K, V]`.
    Map(Box<Ty>, Box<Ty>),
    /// Table: `List[T]`.
    List(Box<Ty>),
    /// The result of a client call to a server function (G11).
    Remote(Box<Ty>),
    /// The rows of a query over the model declared by this definition, in
    /// query order (SYNTAX 4.5).
    Rows(DefId),
    /// The inclusive range `a..b` over values of the given type.
    Range(Box<Ty>),
    /// No value: a function without a result type, a statement.
    Unit,
    /// Unknown because of an earlier diagnostic. Never present when the
    /// diagnostics of `resolve` and `check` contain no error, and never the
    /// cause of a further diagnostic.
    Error,
}

impl Ty {
    /// True for the types a model field may have (the rows of the type table).
    pub fn is_field_type(&self) -> bool {
        match self {
            Ty::Bool
            | Ty::Int
            | Ty::Float
            | Ty::Text
            | Ty::Time
            | Ty::Bytes
            | Ty::Rank
            | Ty::Enum(_)
            | Ty::Ref(_) => true,
            Ty::Optional(inner) | Ty::Set(inner) | Ty::List(inner) => inner.is_field_type(),
            Ty::Map(key, value) => key.is_field_type() && value.is_field_type(),
            Ty::Remote(_) | Ty::Rows(_) | Ty::Range(_) | Ty::Unit | Ty::Error => false,
        }
    }
}
