//! `Map[K, V]`: a last writer wins register per key, with tombstones (ARCHITECTURE 5.1, 6.2,
//! G6). Counterpart of `runtime/js/crdt/map/map.mjs`.
//!
//! Every op sets or removes exactly one key (G12). Per key the entry with the highest
//! [`Hlc`] wins; a remove is kept as a tombstone, so an older put that arrives later cannot
//! bring the key back.

use ostrel_core::canon::{encode, encode_array, encode_object, encode_str};
use ostrel_core::ids::Hlc;
use ostrel_core::value::Value;

use crate::CrdtError;
use crate::keyed::{check_key, search};
use crate::lww::same_value;

/// One key of an [`LwwMap`]: the winning write, a put (`value` is `Some`) or a remove
/// (`value` is `None`, a tombstone).
#[derive(Clone, Debug, PartialEq)]
pub struct MapEntry {
    /// The key.
    pub key: Value,
    /// `Hlc` of the winning write.
    pub hlc: Hlc,
    /// The value, `None` for a tombstone.
    pub value: Option<Value>,
}

/// A map with per key last writer wins. Keys are kept in the wire order of D61.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LwwMap {
    entries: Vec<MapEntry>,
}

impl LwwMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a put of `key` to `value`. Returns whether the map changed.
    pub fn put(&mut self, hlc: Hlc, key: Value, value: Value) -> Result<bool, CrdtError> {
        self.write(MapEntry {
            key,
            hlc,
            value: Some(value),
        })
    }

    /// Applies a remove of `key`, which leaves a tombstone. Returns whether the map changed.
    pub fn remove(&mut self, hlc: Hlc, key: Value) -> Result<bool, CrdtError> {
        self.write(MapEntry {
            key,
            hlc,
            value: None,
        })
    }

    fn write(&mut self, incoming: MapEntry) -> Result<bool, CrdtError> {
        check_key(&incoming.key)?;
        match search(&self.entries, &incoming.key, |e| &e.key) {
            Err(index) => {
                self.entries.insert(index, incoming);
                Ok(true)
            }
            Ok(index) => {
                let Some(current) = self.entries.get_mut(index) else {
                    return Ok(false);
                };
                if current.hlc < incoming.hlc {
                    *current = incoming;
                    return Ok(true);
                }
                if current.hlc == incoming.hlc {
                    let same = match (&current.value, &incoming.value) {
                        (None, None) => true,
                        (Some(a), Some(b)) => same_value(a, b),
                        _ => false,
                    };
                    if !same {
                        return Err(CrdtError::ConflictingWrite(incoming.hlc));
                    }
                }
                Ok(false)
            }
        }
    }

    /// The value of `key`, `None` when absent or removed.
    pub fn get(&self, key: &Value) -> Option<&Value> {
        search(&self.entries, key, |e| &e.key)
            .ok()
            .and_then(|index| self.entries.get(index))
            .and_then(|e| e.value.as_ref())
    }

    /// Live `(key, value)` pairs in D61 key order.
    pub fn iter(&self) -> impl Iterator<Item = (&Value, &Value)> {
        self.entries
            .iter()
            .filter_map(|e| e.value.as_ref().map(|v| (&e.key, v)))
    }

    /// Every entry including tombstones, in D61 key order.
    pub fn entries(&self) -> &[MapEntry] {
        &self.entries
    }

    /// Canonical encoding of the live pairs `[[k, v], ...]`.
    pub fn encode_value(&self) -> String {
        let items: Vec<String> = self
            .iter()
            .map(|(k, v)| encode_array(&[encode(k), encode(v)]))
            .collect();
        encode_array(&items)
    }

    /// Canonical encoding of the full state: `[[k, {"hlc": h, "value": v}], ...]`, and
    /// `[k, {"hlc": h, "removed": true}]` for a tombstone.
    pub fn encode_state(&self) -> String {
        let items: Vec<String> = self
            .entries
            .iter()
            .map(|e| {
                let hlc = ("hlc", encode_str(&e.hlc.to_hex()));
                let body = match &e.value {
                    Some(v) => [hlc, ("value", encode(v))],
                    None => [hlc, ("removed", "true".to_owned())],
                };
                // The keys are fixed and distinct, so `encode_object` cannot fail.
                encode_array(&[encode(&e.key), encode_object(&body).unwrap_or_default()])
            })
            .collect();
        encode_array(&items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ostrel_core::ids::ReplicaId;

    fn hlc(wall: u64, replica: u64) -> Hlc {
        Hlc::new(wall, 0, ReplicaId(replica)).unwrap()
    }

    fn text(s: &str) -> Value {
        Value::Text(s.to_owned())
    }

    #[test]
    fn tombstone_blocks_older_put() {
        let mut m = LwwMap::new();
        assert_eq!(m.remove(hlc(2, 2), text("ana")), Ok(true));
        assert_eq!(m.put(hlc(1, 1), text("ana"), text("admin")), Ok(false));
        assert_eq!(m.get(&text("ana")), None);
        assert_eq!(m.encode_value(), "[]");
        assert_eq!(
            m.encode_state(),
            r#"[["ana",{"hlc":"00000000000200000000000000000002","removed":true}]]"#
        );
        assert_eq!(m.put(hlc(3, 1), text("ana"), text("viewer")), Ok(true));
        assert_eq!(m.get(&text("ana")), Some(&text("viewer")));
    }

    #[test]
    fn same_hlc_must_carry_the_same_write() {
        let mut m = LwwMap::new();
        m.put(hlc(1, 1), text("k"), text("v")).unwrap();
        assert_eq!(m.put(hlc(1, 1), text("k"), text("v")), Ok(false));
        let before = m.clone();
        assert_eq!(
            m.put(hlc(1, 1), text("k"), text("w")),
            Err(CrdtError::ConflictingWrite(hlc(1, 1)))
        );
        assert_eq!(
            m.remove(hlc(1, 1), text("k")),
            Err(CrdtError::ConflictingWrite(hlc(1, 1)))
        );
        assert_eq!(m, before);
    }

    #[test]
    fn keys_follow_d61_and_non_scalars_are_refused() {
        let mut m = LwwMap::new();
        for (i, k) in [10, 9, 100].into_iter().enumerate() {
            m.put(hlc(1, i as u64), Value::int(k).unwrap(), Value::Bool(true))
                .unwrap();
        }
        assert_eq!(m.encode_value(), "[[9,true],[10,true],[100,true]]");
        assert_eq!(
            m.put(hlc(5, 1), Value::Null, Value::Null),
            Err(CrdtError::InvalidKey)
        );
        assert_eq!(
            m.remove(hlc(5, 1), Value::List(vec![])),
            Err(CrdtError::InvalidKey)
        );
        assert_eq!(m.entries().len(), 3);
    }
}
