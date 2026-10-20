//! Bounded JSON reader for incoming frames.
//!
//! A frame is parsed with `serde_json` into a small tree ([`Json`]) before any message type is
//! built. The reader enforces the rules every frame must meet, whatever message it carries:
//!
//! * no object repeats a key, because two parsers may keep different copies of it (D77);
//! * nesting is at most [`MAX_DEPTH`] arrays and objects deep, so hostile input cannot exhaust
//!   the stack.
//!
//! Numbers are not range checked here. The canonical encoding writes some floats as integer
//! tokens (`2^53` as `9007199254740992`, `1e20` as `100000000000000000000`, ARCHITECTURE 5.3),
//! so whether a token is a valid number depends on the type of its place. Each number keeps
//! its token form ([`Number`]), and the typed readers decide: [`Number::safe_int`] for integer
//! fields and `Int` values (D24), [`Number::value`] for `Float` values (D132).
//!
//! Strings are valid UTF-8 without unpaired surrogates; `serde_json` already rejects both.

use std::cell::Cell;
use std::collections::HashSet;
use std::fmt;

use ostrel_core::value::{INT_MAX, INT_MIN};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

use super::WireError;

/// Deepest nesting of arrays and objects in one frame. The deepest legitimate frame holds a
/// value of nesting depth 32 (ARCHITECTURE 5.9) a few levels below the frame object, or a filter
/// of at most 64 nodes; 100 leaves room for both and stays below the fixed limit of 128 inside
/// `serde_json`, so the typed error of this reader is the one that fires.
pub const MAX_DEPTH: usize = 100;

/// One parsed JSON node.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(Number),
    Str(String),
    Arr(Vec<Json>),
    /// Keys in the order of the frame, each at most once.
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Name of the node kind, for error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "boolean",
            Json::Num(_) => "number",
            Json::Str(_) => "string",
            Json::Arr(_) => "array",
            Json::Obj(_) => "object",
        }
    }
}

/// One number token of a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Number {
    value: f64,
    integer: bool,
}

/// Why a [`Number`] is not a safe integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotSafeInt {
    /// The number is outside `INT_MIN..=INT_MAX`, however it is written.
    OutOfRange,
    /// The number is in range but written with a fraction or an exponent (`7.0`, `7e0`).
    NotAnInteger,
}

impl Number {
    /// The number the token denotes, rounded to the nearest `f64` as by
    /// `str::parse::<f64>`, so every finite float written by the canonical encoder reads back
    /// to the same float. Always finite: a token beyond the `f64` range is malformed JSON.
    pub fn value(self) -> f64 {
        self.value
    }

    /// True when the token has neither fraction nor exponent.
    pub fn is_integer_token(self) -> bool {
        self.integer
    }

    /// The token as an integer of the safe range (D24). A number outside the range is
    /// [`NotSafeInt::OutOfRange`] whatever its form, so `2^64` and `1e20` are refused alike.
    pub fn safe_int(self) -> Result<i64, NotSafeInt> {
        // Every integer in the safe range is exact in f64, and no integer token outside the
        // range rounds into it, so comparing the rounded value is exact.
        if !(INT_MIN as f64..=INT_MAX as f64).contains(&self.value) {
            return Err(NotSafeInt::OutOfRange);
        }
        if !self.integer {
            return Err(NotSafeInt::NotAnInteger);
        }
        // In range and integral, so the conversion is exact; `-0` becomes `0`.
        Ok(self.value as i64)
    }
}

/// Parses one complete JSON text. Trailing content after the value is an error.
pub fn parse(bytes: &[u8]) -> Result<Json, WireError> {
    let numbers = number_tokens(bytes);
    let state = State {
        bytes,
        numbers: &numbers,
        next_number: Cell::new(0),
        fault: Cell::new(None),
    };
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let parsed = Node {
        depth: 0,
        state: &state,
    }
    .deserialize(&mut de)
    .and_then(|json| de.end().map(|()| json));
    match parsed {
        Ok(json) => Ok(json),
        // A rule of this reader fired: report it, not the generic serde error that carried it.
        Err(e) => Err(state
            .fault
            .take()
            .unwrap_or_else(|| WireError::Malformed(e.to_string()))),
    }
}

/// Byte ranges of the number tokens of `bytes`, in document order.
///
/// `serde_json` reports an integer token beyond `u64` as an `f64` and rounds long tokens on a
/// fast path, so the reader takes the form and value of each number from its token instead.
/// Only well formed JSON has to be scanned correctly: anything else fails in `serde_json`.
fn number_tokens(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut tokens = Vec::new();
    let mut at = 0;
    while let Some(&b) = bytes.get(at) {
        match b {
            b'"' => {
                // Skip the string; an escaped character never ends it.
                at += 1;
                while let Some(&c) = bytes.get(at) {
                    at += if c == b'\\' { 2 } else { 1 };
                    if c == b'"' {
                        break;
                    }
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = at;
                while bytes
                    .get(at)
                    .is_some_and(|c| matches!(c, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'))
                {
                    at += 1;
                }
                tokens.push((start, at));
            }
            _ => at += 1,
        }
    }
    tokens
}

/// What every node of one parse shares.
struct State<'a> {
    bytes: &'a [u8],
    numbers: &'a [(usize, usize)],
    next_number: Cell<usize>,
    fault: Cell<Option<WireError>>,
}

/// Visitor for one node at a known depth. A rule violation is stored in the shared `fault` as
/// a typed error, and a placeholder serde error stops the parse.
struct Node<'s, 'a> {
    depth: usize,
    state: &'s State<'a>,
}

impl Node<'_, '_> {
    fn reject<E: de::Error>(&self, error: WireError) -> E {
        self.state.fault.set(Some(error));
        E::custom("frame rejected by the wire reader")
    }

    fn child(&self) -> Self {
        Node {
            depth: self.depth + 1,
            state: self.state,
        }
    }

    fn enter<E: de::Error>(&self) -> Result<(), E> {
        if self.depth >= MAX_DEPTH {
            return Err(self.reject(WireError::TooDeep));
        }
        Ok(())
    }

    /// Builds the next number from its token. `serde_json` visits numbers in document order,
    /// the same order as [`number_tokens`].
    fn number<E: de::Error>(&self) -> Result<Json, E> {
        let index = self.state.next_number.get();
        self.state.next_number.set(index + 1);
        let token = self
            .state
            .numbers
            .get(index)
            .and_then(|&(start, end)| self.state.bytes.get(start..end))
            .and_then(|token| std::str::from_utf8(token).ok());
        let Some(token) = token else {
            return Err(self.reject(WireError::Malformed("number token not found".into())));
        };
        match token.parse::<f64>() {
            Ok(value) if value.is_finite() => Ok(Json::Num(Number {
                value,
                integer: !token.contains(['.', 'e', 'E']),
            })),
            // serde_json refuses such tokens first; this guards the contract of `Number`.
            _ => Err(self.reject(WireError::Malformed("number outside the f64 range".into()))),
        }
    }
}

impl<'de> DeserializeSeed<'de> for Node<'_, '_> {
    type Value = Json;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Json, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Node<'_, '_> {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Json, E> {
        self.number()
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Json, E> {
        self.number()
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Json, E> {
        self.number()
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Json, E> {
        Ok(Json::Str(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Json, E> {
        Ok(Json::Str(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        self.enter()?;
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(self.child())? {
            items.push(item);
        }
        Ok(Json::Arr(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        self.enter()?;
        // A set, not a scan of `entries`: a hostile object with many keys must stay linear.
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(self.reject(WireError::DuplicateKey(super::clip(&key))));
            }
            let value = map.next_value_seed(self.child())?;
            entries.push((key, value));
        }
        Ok(Json::Obj(entries))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_tokens_skip_strings_and_escapes() {
        let text = br#"{"a1":"x\"2","b":-3.5e+2,"c":[0,"\\",7]}"#;
        let tokens: Vec<&[u8]> = number_tokens(text)
            .into_iter()
            .filter_map(|(s, e)| text.get(s..e))
            .collect();
        assert_eq!(tokens, [&b"-3.5e+2"[..], b"0", b"7"]);
    }
}
