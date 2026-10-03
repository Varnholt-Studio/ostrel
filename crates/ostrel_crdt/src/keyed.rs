//! Shared helpers for collections ordered by the wire order of D61.

use ostrel_core::value::{Value, compare_key};

use crate::CrdtError;

/// Refuses values that are not wire scalars as set elements or map keys.
pub(crate) fn check_key(key: &Value) -> Result<(), CrdtError> {
    match key {
        Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Time(_)
        | Value::Text(_)
        | Value::Bytes(_)
        | Value::Enum(_)
        | Value::Ref(_)
        | Value::Rank(_) => Ok(()),
        Value::Null | Value::Set(_) | Value::Map(_) | Value::List(_) => Err(CrdtError::InvalidKey),
    }
}

/// Position of `key` in `items`, which are sorted by [`compare_key`] of `key_of`: `Ok` with
/// the index of the equal key, or `Err` with the insert position.
pub(crate) fn search<T>(
    items: &[T],
    key: &Value,
    key_of: impl Fn(&T) -> &Value,
) -> Result<usize, usize> {
    items.binary_search_by(|item| compare_key(key_of(item), key))
}
