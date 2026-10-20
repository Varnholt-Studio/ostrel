//! Typed values on the wire (ARCHITECTURE 5.1, D132).
//!
//! The JSON form of a value does not name its type: a string may be `Text`, an enum variant, a
//! reference, `Bytes` or a `Rank` key, and a number may be an `Int` or a `Float`. A value is
//! therefore read against the [`WireTy`] of its place, which the schema supplies
//! ([`super::WireSchema`]). Values are written with the canonical encoder of `ostrel_core`,
//! which needs no type because a [`Value`] carries its variant.
//!
//! A value that does not fit its type is a [`ValueFault`]. It never closes the connection: the
//! op that carries it becomes `Invalid` (D132).

use std::cmp::Ordering;
use std::fmt;

use ostrel_core::ids::RowId;
use ostrel_core::value::{Value, compare_key};

use super::json::{Json, NotSafeInt};
use crate::protocol::LIMITS;

/// The type of a value on the wire: one variant per row of the type table (ARCHITECTURE 5.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireTy {
    Bool,
    Int,
    Float,
    Text,
    /// Milliseconds since the Unix epoch, an integer in the `Int` range.
    Time,
    /// base64url without padding.
    Bytes,
    /// The variant names in declaration order.
    Enum(Vec<String>),
    /// A row id; the target model plays no part in the wire form.
    Ref,
    /// `T?`: a value of `T` or `null`.
    Optional(Box<WireTy>),
    Set(Box<WireTy>),
    Map(Box<WireTy>, Box<WireTy>),
    List(Box<WireTy>),
    Rank,
    /// `Int serial`: an `Int` on the wire, assigned by the server.
    Serial,
}

/// Why a value does not fit its type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueFault {
    /// The JSON kind does not match the type, for example a string for an `Int`.
    WrongType,
    /// An `Int`, `Serial` or `Time` outside the safe range (D24).
    IntRange,
    /// Collections nested deeper than [`LIMITS`]`.nesting_depth`.
    TooDeep,
    /// A `Text` of more than [`LIMITS`]`.text_bytes` UTF-8 bytes (D109).
    TextTooLong,
    /// A name that is not a variant of the enum.
    UnknownVariant,
    /// A reference that is not a 32 digit lowercase row id.
    InvalidRef,
    /// `Bytes` that are not unpadded base64url.
    InvalidBytes,
    /// A `Rank` key that is not 1 to [`LIMITS`]`.rank_digits` base 62 digits without a
    /// trailing `0` (D88).
    InvalidRank,
    /// A `Set` that holds an element twice, or a `Map` that holds a key twice.
    Repeated,
}

impl fmt::Display for ValueFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let why = match self {
            ValueFault::WrongType => "value has the wrong JSON form for its type",
            ValueFault::IntRange => "integer outside the safe range",
            ValueFault::TooDeep => "value nested too deeply",
            ValueFault::TextTooLong => "text over the text limit",
            ValueFault::UnknownVariant => "unknown enum variant",
            ValueFault::InvalidRef => "reference is not a row id",
            ValueFault::InvalidBytes => "bytes are not unpadded base64url",
            ValueFault::InvalidRank => "invalid rank key",
            ValueFault::Repeated => "set element or map key repeated",
        };
        f.write_str(why)
    }
}

/// Reads one value of type `ty`.
///
/// Nesting depth counts collections: a `Set`, `List` or `Map` holding only scalars has depth
/// 1, and at most [`LIMITS`]`.nesting_depth` levels are read.
pub fn read_value(ty: &WireTy, json: Json) -> Result<Value, ValueFault> {
    read(ty, json, 0)
}

fn read(ty: &WireTy, json: Json, depth: u32) -> Result<Value, ValueFault> {
    match (ty, json) {
        (WireTy::Optional(_), Json::Null) => Ok(Value::Null),
        (WireTy::Optional(inner), json) => read(inner, json, depth),
        (WireTy::Bool, Json::Bool(b)) => Ok(Value::Bool(b)),
        (WireTy::Int | WireTy::Serial, Json::Num(n)) => {
            Value::int(int(n.safe_int())?).map_err(|_| ValueFault::IntRange)
        }
        (WireTy::Time, Json::Num(n)) => {
            Value::time(int(n.safe_int())?).map_err(|_| ValueFault::IntRange)
        }
        // Any number token: the canonical encoder writes integral floats without a fraction.
        (WireTy::Float, Json::Num(n)) => Value::float(n.value()).map_err(|_| ValueFault::WrongType),
        (WireTy::Text, Json::Str(s)) => {
            if s.len() > LIMITS.text_bytes as usize {
                return Err(ValueFault::TextTooLong);
            }
            Ok(Value::Text(s))
        }
        (WireTy::Bytes, Json::Str(s)) => base64url_decode(&s)
            .map(Value::Bytes)
            .ok_or(ValueFault::InvalidBytes),
        (WireTy::Enum(variants), Json::Str(s)) => {
            if variants.contains(&s) {
                Ok(Value::Enum(s))
            } else {
                Err(ValueFault::UnknownVariant)
            }
        }
        (WireTy::Ref, Json::Str(s)) => RowId::from_hex(&s)
            .map(Value::Ref)
            .map_err(|_| ValueFault::InvalidRef),
        (WireTy::Rank, Json::Str(s)) => {
            if is_valid_rank(&s) {
                Ok(Value::Rank(s))
            } else {
                Err(ValueFault::InvalidRank)
            }
        }
        (WireTy::Set(elem), Json::Arr(items)) => {
            let depth = enter(depth)?;
            let elems = items
                .into_iter()
                .map(|item| read(elem, item, depth))
                .collect::<Result<Vec<_>, _>>()?;
            if has_repeats(elems.iter()) {
                return Err(ValueFault::Repeated);
            }
            Ok(Value::Set(elems))
        }
        (WireTy::List(elem), Json::Arr(items)) => {
            let depth = enter(depth)?;
            items
                .into_iter()
                .map(|item| read(elem, item, depth))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List)
        }
        (WireTy::Map(key, value), Json::Arr(items)) => {
            let depth = enter(depth)?;
            let entries = items
                .into_iter()
                .map(|item| match item {
                    Json::Arr(pair) => match <[Json; 2]>::try_from(pair) {
                        Ok([k, v]) => Ok((read(key, k, depth)?, read(value, v, depth)?)),
                        Err(_) => Err(ValueFault::WrongType),
                    },
                    _ => Err(ValueFault::WrongType),
                })
                .collect::<Result<Vec<_>, _>>()?;
            if has_repeats(entries.iter().map(|(k, _)| k)) {
                return Err(ValueFault::Repeated);
            }
            Ok(Value::Map(entries))
        }
        _ => Err(ValueFault::WrongType),
    }
}

fn int(n: Result<i64, NotSafeInt>) -> Result<i64, ValueFault> {
    n.map_err(|why| match why {
        NotSafeInt::OutOfRange => ValueFault::IntRange,
        NotSafeInt::NotAnInteger => ValueFault::WrongType,
    })
}

/// One collection level deeper.
fn enter(depth: u32) -> Result<u32, ValueFault> {
    if depth >= LIMITS.nesting_depth {
        return Err(ValueFault::TooDeep);
    }
    Ok(depth + 1)
}

/// True when two of `values` are equal under [`compare_key`], the order of the canonical
/// encoding: such a set or map would have two encodings for one content.
fn has_repeats<'v>(values: impl Iterator<Item = &'v Value>) -> bool {
    let mut sorted: Vec<&Value> = values.collect();
    sorted.sort_by(|a, b| compare_key(a, b));
    sorted
        .windows(2)
        .any(|w| matches!(w, [a, b] if compare_key(a, b) == Ordering::Equal))
}

/// The rule of `isValidRank` in `runtime/js/crdt/rank/rank.mjs`: 1 to `rank_digits` digits of
/// `0-9A-Za-z`, not ending in `0`.
fn is_valid_rank(key: &str) -> bool {
    key.len() <= LIMITS.rank_digits as usize
        && key.bytes().all(|b| b.is_ascii_alphanumeric())
        && key.bytes().last().is_some_and(|b| b != b'0')
}

/// Strict base64url without padding (RFC 4648 section 5), the inverse of
/// `ostrel_core::value::base64url`: unused low bits of the last digit must be zero, so every
/// byte string has exactly one accepted text.
fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    fn digit(c: u8) -> Option<u32> {
        let d = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        Some(u32::from(d))
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3 + 2);
    for chunk in text.as_bytes().chunks(4) {
        let mut n = 0u32;
        for &c in chunk {
            n = (n << 6) | digit(c)?;
        }
        // A chunk of k digits holds k - 1 bytes; the remaining low bits must be zero.
        let bytes = chunk.len().checked_sub(1).filter(|&b| b > 0)?;
        let spare = chunk.len() * 6 - bytes * 8;
        if n & ((1 << spare) - 1) != 0 {
            return None;
        }
        n >>= spare;
        for i in (0..bytes).rev() {
            out.push((n >> (8 * i)) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use ostrel_core::value::base64url;

    use super::*;

    #[test]
    fn base64url_reads_back_what_core_writes() {
        for len in 0..40usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 251) as u8).collect();
            assert_eq!(base64url_decode(&base64url(&bytes)), Some(bytes));
        }
    }

    #[test]
    fn base64url_refuses_padding_stray_bits_and_bad_lengths() {
        for text in ["AA==", "AB", "A", "AAAAA", "+/", "AA AA"] {
            assert_eq!(base64url_decode(text), None, "{text}");
        }
        assert_eq!(base64url_decode("-_8"), Some(vec![0xfb, 0xff]));
    }

    #[test]
    fn rank_keys_follow_the_js_rule() {
        assert!(is_valid_rank("U"));
        assert!(is_valid_rank(&"z".repeat(1024)));
        for key in ["", "a0", "a-b", "é", &"z".repeat(1025)] {
            assert!(!is_valid_rank(key), "{key}");
        }
    }
}
