//! Bounded JSON reader for incoming frames.
//!
//! A frame is parsed with `serde_json` into a small tree ([`Json`]) before any message type is
//! built. The reader enforces the rules every frame must meet, whatever message it carries:
//!
//! * no object repeats a key, because two parsers may keep different copies of it (D77);
//! * nesting is at most [`MAX_DEPTH`] arrays and objects deep, so hostile input cannot exhaust
//!   the stack;
//! * every integer is in the safe range `INT_MIN..=INT_MAX` (D24); a float is finite.
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
    /// An integer token in `INT_MIN..=INT_MAX`.
    Int(i64),
    /// A number token with a fraction or exponent, finite.
    Float(f64),
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
            Json::Int(_) | Json::Float(_) => "number",
            Json::Str(_) => "string",
            Json::Arr(_) => "array",
            Json::Obj(_) => "object",
        }
    }
}

/// Parses one complete JSON text. Trailing content after the value is an error.
pub fn parse(bytes: &[u8]) -> Result<Json, WireError> {
    let fault = Cell::new(None);
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let parsed = Node {
        depth: 0,
        fault: &fault,
    }
    .deserialize(&mut de)
    .and_then(|json| de.end().map(|()| json));
    match parsed {
        Ok(json) => Ok(json),
        // A rule of this reader fired: report it, not the generic serde error that carried it.
        Err(e) => Err(fault
            .take()
            .unwrap_or_else(|| WireError::Malformed(e.to_string()))),
    }
}

/// Visitor for one node at a known depth. A rule violation is stored in `fault` as a typed
/// error, and a placeholder serde error stops the parse.
struct Node<'f> {
    depth: usize,
    fault: &'f Cell<Option<WireError>>,
}

impl Node<'_> {
    fn reject<E: de::Error>(&self, error: WireError) -> E {
        self.fault.set(Some(error));
        E::custom("frame rejected by the wire reader")
    }

    fn child(&self) -> Self {
        Node {
            depth: self.depth + 1,
            fault: self.fault,
        }
    }

    fn enter<E: de::Error>(&self) -> Result<(), E> {
        if self.depth >= MAX_DEPTH {
            return Err(self.reject(WireError::TooDeep));
        }
        Ok(())
    }
}

impl<'de> DeserializeSeed<'de> for Node<'_> {
    type Value = Json;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Json, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Node<'_> {
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

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Json, E> {
        if (INT_MIN..=INT_MAX).contains(&v) {
            Ok(Json::Int(v))
        } else {
            Err(self.reject(WireError::NumberOutOfRange))
        }
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Json, E> {
        match i64::try_from(v) {
            Ok(v) => self.visit_i64(v),
            Err(_) => Err(self.reject(WireError::NumberOutOfRange)),
        }
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Json, E> {
        // serde_json reports overflow itself; this guards the contract of `Json::Float`.
        if v.is_finite() {
            Ok(Json::Float(v))
        } else {
            Err(self.reject(WireError::NumberOutOfRange))
        }
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
