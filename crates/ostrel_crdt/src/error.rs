use std::fmt;

use ostrel_core::ids::Hlc;

/// An op a CRDT refuses. The replica is unchanged; every case is the protocol reason
/// `Invalid`, the variants exist for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CrdtError {
    /// Two different writes carry the same `Hlc`. An `Hlc` names exactly one op of one
    /// replica, so this cannot come from a correct replica, and keeping either write would
    /// let replicas diverge silently.
    ConflictingWrite(Hlc),
    /// A `Set` element or `Map` key that is not a wire scalar (boolean, number or string
    /// class value): `null`, `Set`, `Map` and `List` are refused, as in the JS runtime.
    InvalidKey,
    /// A set remove that names no tag.
    NoTags,
    /// A set remove that names more than [`MAX_REMOVE_TAGS`](crate::MAX_REMOVE_TAGS) tags;
    /// carries the number named.
    TooManyTags(usize),
}

impl fmt::Display for CrdtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConflictingWrite(hlc) => {
                write!(
                    f,
                    "two different writes carry the same hlc {}",
                    hlc.to_hex()
                )
            }
            Self::InvalidKey => write!(
                f,
                "a set element or map key must be a boolean, number or string"
            ),
            Self::NoTags => write!(f, "a set remove names at least one tag"),
            Self::TooManyTags(n) => write!(
                f,
                "a set remove names at most {} tags, got {n}",
                crate::MAX_REMOVE_TAGS
            ),
        }
    }
}

impl std::error::Error for CrdtError {}
