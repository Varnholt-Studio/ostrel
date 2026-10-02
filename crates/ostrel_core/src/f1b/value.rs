//! The persisted and wire value (ARCHITECTURE 5.1, 5.3, 7.4).
//!
//! The VM keeps its own reference counted values and converts at the `Host` boundary. A
//! [`Value`] can only hold what the wire can carry: integers in the safe JavaScript range and
//! finite floats are enforced by [`SafeInt`] and [`FiniteFloat`], whose constructors are the
//! only way in. Rust strings are always valid UTF-8, so an unpaired surrogate cannot occur.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;

use super::ids::RowId;

/// Largest `Int`: 2^53 - 1, the largest safe JavaScript integer (D24).
pub const INT_MAX: i64 = (1 << 53) - 1;
/// Smallest `Int`: -(2^53 - 1).
pub const INT_MIN: i64 = -INT_MAX;

/// A value that cannot be stored or sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidValue {
    /// An integer outside `INT_MIN..=INT_MAX` (runtime kind `IntOverflow`, 7.4).
    IntRange,
    /// NaN or an infinity (5.1).
    NotFinite,
}

impl fmt::Display for InvalidValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IntRange => write!(f, "integer outside the safe range of +-(2^53 - 1)"),
            Self::NotFinite => write!(f, "NaN and infinities are not values"),
        }
    }
}

impl std::error::Error for InvalidValue {}

/// An integer in `INT_MIN..=INT_MAX`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SafeInt(i64);

impl SafeInt {
    /// Checks the range of ARCHITECTURE 7.4.
    pub fn new(v: i64) -> Result<Self, InvalidValue> {
        if (INT_MIN..=INT_MAX).contains(&v) {
            Ok(Self(v))
        } else {
            Err(InvalidValue::IntRange)
        }
    }

    /// The integer.
    pub fn get(self) -> i64 {
        self.0
    }
}

/// A finite `f64`. `-0.0` is kept as written and compares equal to `0.0`.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct FiniteFloat(f64);

impl FiniteFloat {
    /// Rejects NaN and both infinities.
    pub fn new(v: f64) -> Result<Self, InvalidValue> {
        if v.is_finite() {
            Ok(Self(v))
        } else {
            Err(InvalidValue::NotFinite)
        }
    }

    /// The float.
    pub fn get(self) -> f64 {
        self.0
    }
}

/// A persisted and wire value, one variant per row of the type table in ARCHITECTURE 5.1.
///
/// Wire forms: `Time` is a JSON number, `Bytes` a base64url string without padding, `Enum` the
/// variant name, `Ref` the 32 digit row id hex, `Set` and `List` arrays, `Map` an array of
/// `[key, value]` pairs. `Set` elements and `Map` entries are written in [`compare_key`]
/// order whatever their order in the vector (D61).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `null`, the value of an empty `T?`.
    Null,
    /// `Bool`.
    Bool(bool),
    /// `Int`, safe JavaScript range.
    Int(SafeInt),
    /// `Float`, finite.
    Float(FiniteFloat),
    /// `Text`.
    Text(String),
    /// `Time`, milliseconds since the Unix epoch, in the `Int` range.
    Time(SafeInt),
    /// `Bytes`.
    Bytes(Vec<u8>),
    /// Enum variant name.
    Enum(String),
    /// Reference to a row.
    Ref(RowId),
    /// `Set[T]` elements.
    Set(Vec<Value>),
    /// `Map[K, V]` entries.
    Map(Vec<(Value, Value)>),
    /// `List[T]`, in program order.
    List(Vec<Value>),
    /// `Rank`, a fractional index key.
    Rank(String),
}

impl Value {
    /// `Int` value, range checked.
    pub fn int(v: i64) -> Result<Self, InvalidValue> {
        SafeInt::new(v).map(Self::Int)
    }

    /// `Float` value, finiteness checked.
    pub fn float(v: f64) -> Result<Self, InvalidValue> {
        FiniteFloat::new(v).map(Self::Float)
    }

    /// `Time` value, range checked like `Int`.
    pub fn time(ms: i64) -> Result<Self, InvalidValue> {
        SafeInt::new(ms).map(Self::Time)
    }
}

/// Order of `Set` elements and `Map` keys by wire value (D61, ARCHITECTURE 6.2).
///
/// Booleans before numbers before strings; `false` before `true`; numbers numerically across
/// `Int`, `Float` and `Time`, with `-0` equal to `0`; strings by code point (D50), which is
/// byte order of UTF-8 and covers `Text`, `Rank`, enum names, reference hex and `Bytes` as
/// base64url.
///
/// ASSUMPTION (not fixed by D61, needed for totality only): `null` sorts first and arrays
/// (`Set`, `Map`, `List`) sort last, compared element by element with this function, a
/// shorter prefix first. Within one `Set[T]` all elements have one type, so these cases do
/// not occur in valid programs.
pub fn compare_key(a: &Value, b: &Value) -> Ordering {
    let by_class = class(a).cmp(&class(b));
    if by_class != Ordering::Equal {
        return by_class;
    }
    match (class(a), a, b) {
        (_, &Value::Bool(x), &Value::Bool(y)) => x.cmp(&y),
        (Class::Number, _, _) => compare_numbers(a, b),
        (Class::String, _, _) => compare_wire_strings(a, b),
        (Class::Array, _, _) => compare_arrays(a, b),
        _ => Ordering::Equal,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Null,
    Bool,
    Number,
    String,
    Array,
}

fn class(v: &Value) -> Class {
    match v {
        Value::Null => Class::Null,
        Value::Bool(_) => Class::Bool,
        Value::Int(_) | Value::Float(_) | Value::Time(_) => Class::Number,
        Value::Text(_) | Value::Bytes(_) | Value::Enum(_) | Value::Ref(_) | Value::Rank(_) => {
            Class::String
        }
        Value::Set(_) | Value::Map(_) | Value::List(_) => Class::Array,
    }
}

/// Every `Int` and `Time` is exact in `f64` (at most 53 bits), and floats are finite, so the
/// partial order of `f64` is total here and treats `-0` as `0`.
fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Int(i) | Value::Time(i) => i.get() as f64,
        Value::Float(f) => f.get(),
        _ => 0.0,
    }
}

fn compare_numbers(a: &Value, b: &Value) -> Ordering {
    as_f64(a).partial_cmp(&as_f64(b)).unwrap_or(Ordering::Equal)
}

fn compare_wire_strings(a: &Value, b: &Value) -> Ordering {
    if let (Value::Ref(x), Value::Ref(y)) = (a, b) {
        // Fixed width hex: string order equals id order (5.3).
        return x.cmp(y);
    }
    wire_string(a).as_bytes().cmp(wire_string(b).as_bytes())
}

/// The JSON string a string class value travels as (unescaped).
pub(crate) fn wire_string(v: &Value) -> Cow<'_, str> {
    match v {
        Value::Text(s) | Value::Enum(s) | Value::Rank(s) => Cow::Borrowed(s),
        Value::Ref(r) => Cow::Owned(r.to_hex()),
        Value::Bytes(b) => Cow::Owned(base64url(b)),
        _ => Cow::Borrowed(""),
    }
}

fn compare_arrays(a: &Value, b: &Value) -> Ordering {
    let left = array_items(a);
    let right = array_items(b);
    for (x, y) in left.iter().zip(right.iter()) {
        let o = compare_key(x, y);
        if o != Ordering::Equal {
            return o;
        }
    }
    left.len().cmp(&right.len())
}

/// Items of an array class value in wire order: a `Map` entry is the pair `[k, v]`.
fn array_items(v: &Value) -> Vec<Value> {
    match v {
        Value::Set(items) => sorted(items).into_iter().cloned().collect(),
        Value::List(items) => items.clone(),
        Value::Map(entries) => sorted_entries(entries)
            .into_iter()
            .map(|(k, v)| Value::List(vec![k.clone(), v.clone()]))
            .collect(),
        _ => Vec::new(),
    }
}

/// Set elements in [`compare_key`] order (stable, duplicates kept).
pub(crate) fn sorted(items: &[Value]) -> Vec<&Value> {
    let mut out: Vec<&Value> = items.iter().collect();
    out.sort_by(|a, b| compare_key(a, b));
    out
}

/// Map entries in [`compare_key`] order of their keys (stable, duplicates kept).
pub(crate) fn sorted_entries(entries: &[(Value, Value)]) -> Vec<(&Value, &Value)> {
    let mut out: Vec<(&Value, &Value)> = entries.iter().map(|(k, v)| (k, v)).collect();
    out.sort_by(|a, b| compare_key(a.0, b.0));
    out
}

/// base64url (RFC 4648 section 5) without padding.
///
/// ASSUMPTION: the `Bytes` wire form of ARCHITECTURE 5.1 is unpadded, as in PASETO.
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let digit = |n: u32| char::from(ALPHABET.get((n & 63) as usize).copied().unwrap_or(b'A'));
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (b(0) << 16) | (b(1) << 8) | b(2);
        out.push(digit(n >> 18));
        out.push(digit(n >> 12));
        if chunk.len() > 1 {
            out.push(digit(n >> 6));
        }
        if chunk.len() > 2 {
            out.push(digit(n));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::f1b::ids::{Hlc, ReplicaId};

    fn int(v: i64) -> Value {
        Value::int(v).unwrap()
    }

    fn text(s: &str) -> Value {
        Value::Text(s.to_owned())
    }

    #[test]
    fn constructors_enforce_wire_ranges() {
        assert!(Value::int(INT_MAX).is_ok());
        assert!(Value::int(INT_MIN).is_ok());
        assert_eq!(Value::int(INT_MAX + 1), Err(InvalidValue::IntRange));
        assert_eq!(Value::int(INT_MIN - 1), Err(InvalidValue::IntRange));
        assert_eq!(Value::int(i64::MIN), Err(InvalidValue::IntRange));
        assert_eq!(Value::time(i64::MAX), Err(InvalidValue::IntRange));
        assert_eq!(Value::float(f64::NAN), Err(InvalidValue::NotFinite));
        assert_eq!(Value::float(f64::INFINITY), Err(InvalidValue::NotFinite));
        assert_eq!(
            Value::float(f64::NEG_INFINITY),
            Err(InvalidValue::NotFinite)
        );
        assert!(Value::float(-0.0).is_ok());
    }

    #[test]
    fn key_order_follows_d61() {
        // The D61 cases: 9 before 10, booleans before numbers before strings.
        let mut keys = [
            text("b"),
            int(10),
            Value::Bool(true),
            int(9),
            Value::float(-0.5).unwrap(),
            Value::Bool(false),
            text("a"),
            Value::Null,
        ];
        keys.sort_by(compare_key);
        assert_eq!(
            keys,
            [
                Value::Null,
                Value::Bool(false),
                Value::Bool(true),
                Value::float(-0.5).unwrap(),
                int(9),
                int(10),
                text("a"),
                text("b"),
            ]
        );
        let neg_zero = Value::float(-0.0).unwrap();
        assert_eq!(compare_key(&neg_zero, &int(0)), Ordering::Equal);
        assert_eq!(
            compare_key(&int(1), &Value::float(1.5).unwrap()),
            Ordering::Less
        );
    }

    #[test]
    fn strings_compare_by_code_point_not_escape_or_utf16() {
        // U+FF01 before U+1F600 (UTF-16 units would put the astral character first).
        assert_eq!(
            compare_key(&text("\u{ff01}"), &text("\u{1f600}")),
            Ordering::Less
        );
        // Raw order, not canonical bytes: U+0001 is escaped as \u0001 but sorts before "!".
        assert_eq!(compare_key(&text("\u{1}"), &text("!")), Ordering::Less);
        assert_eq!(compare_key(&text("\""), &text("\\")), Ordering::Less);
        assert_eq!(compare_key(&text("a"), &text("ab")), Ordering::Less);
        // Enum names and ranks are strings too.
        assert_eq!(
            compare_key(&Value::Enum("b".into()), &Value::Rank("a".into())),
            Ordering::Greater
        );
    }

    #[test]
    fn refs_and_bytes_compare_as_their_wire_strings() {
        let row = |w| Value::Ref(RowId::new(Hlc::new(w, 0, ReplicaId(1)).unwrap()));
        assert_eq!(compare_key(&row(1), &row(2)), Ordering::Less);
        // Wire string order, not byte order: "-" (0x2d) < "A" (0x41) < "_" (0x5f), and
        // [0xfb] is "-w", [0x00] is "AA", [0xff] is "_w".
        let bytes = |b: &[u8]| Value::Bytes(b.to_vec());
        assert_eq!(
            compare_key(&bytes(&[0xfb]), &bytes(&[0x00])),
            Ordering::Less
        );
        assert_eq!(
            compare_key(&bytes(&[0x00]), &bytes(&[0xff])),
            Ordering::Less
        );
        assert_eq!(compare_key(&bytes(&[0x00]), &text("AA")), Ordering::Equal);
    }

    #[test]
    fn base64url_matches_rfc_4648_without_padding() {
        let cases: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "Zg"),
            (b"fo", "Zm8"),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg"),
            (b"fooba", "Zm9vYmE"),
            (&[0xfb, 0xff, 0xbf], "-_-_"),
        ];
        for (input, want) in cases {
            assert_eq!(base64url(input), want);
        }
    }
}
