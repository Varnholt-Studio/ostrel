//! Types of the full IR: the rows of the type table (ARCHITECTURE 5.1) plus the
//! types that exist only on locals.

use super::{EnumId, ModelId};

/// Type of a local, a parameter, a function result or a model field.
///
/// Unlike the v0.1 slice type ([`crate::Ty`]) it is not `Copy`, because
/// collections nest. The checker guarantees that every local has exactly one
/// type and that no type is unknown; there is no error type in the IR.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Ty {
    /// No value: result of a function without `-> T`.
    Unit,
    /// `Bool`.
    Bool,
    /// `Int` in [`crate::INT_MIN`, `crate::INT_MAX`] (G9).
    Int,
    /// `Float`, always finite (5.1).
    Float,
    /// `Text`, immutable UTF 8.
    Text,
    /// `Time`, ms since the Unix epoch.
    Time,
    /// `Bytes`.
    Bytes,
    /// `Rank`, a fractional index key.
    Rank,
    /// A variant of the enum.
    Enum(EnumId),
    /// A reference to a row of the model. Model values are always references:
    /// `==` compares ids (SYNTAX 4.10).
    Ref(ModelId),
    /// `T?`. Never nests.
    Optional(Box<Ty>),
    /// `Set[T]`.
    Set(Box<Ty>),
    /// `Map[K, V]`.
    Map(Box<Ty>, Box<Ty>),
    /// `List[T]`.
    List(Box<Ty>),
    /// The pending result of a client call to a server function (G11). Only
    /// on the client; [`super::InstKind::Settle`] turns it into a `T`.
    Remote(Box<Ty>),
    /// A snapshot of query rows of the model, in query order.
    Rows(ModelId),
    /// The inclusive range `lo..hi` over values of the type.
    Range(Box<Ty>),
    /// The state of a `for` loop over a value of the inner type (see
    /// [`super::InstKind::IterStart`]). Never a parameter, field or result.
    Iter(Box<Ty>),
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
            Ty::Optional(inner) => !matches!(**inner, Ty::Optional(_)) && inner.is_field_type(),
            Ty::Set(inner) | Ty::List(inner) => inner.is_field_type(),
            Ty::Map(key, value) => key.is_field_type() && value.is_field_type(),
            Ty::Unit | Ty::Remote(_) | Ty::Rows(_) | Ty::Range(_) | Ty::Iter(_) => false,
        }
    }

    /// True for the types that may cross the `Call` and `Return` boundary
    /// between client and server: field types and `Unit` (ARCHITECTURE 5.4).
    pub fn is_wire_type(&self) -> bool {
        *self == Ty::Unit || self.is_field_type()
    }
}
