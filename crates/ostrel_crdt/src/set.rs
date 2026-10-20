//! `Set[T]`: add wins observed remove set with add tags (ARCHITECTURE 6.2, D49, D62).
//! Counterpart of `runtime/js/crdt/set/add_wins_set.mjs`.
//!
//! * An add of `e` by op `t` creates the tag `t` (the op's own [`OpId`]).
//! * A remove of `e` names the tags of `e` its replica observed. A tag is deleted only when
//!   both its replica and its seq match a named tag; unknown tags are ignored.
//! * `e` is present while at least one of its tags is live, so a concurrent add, whose tag
//!   the remove did not name, survives (add wins).
//! * At most one live tag per (element, replica): an add from replica R replaces R's older
//!   tag of `e`; an add whose seq is not above R's live tag is a replay and is ignored.
//! * No tombstones: the server log delivers a remove only after the adds it names.

use std::collections::BTreeMap;

use ostrel_core::canon::{encode, encode_array, encode_str};
use ostrel_core::ids::{OpId, ReplicaId};
use ostrel_core::value::Value;

use crate::CrdtError;
use crate::keyed::{check_key, search};

/// Most tags one remove op may name (ARCHITECTURE 5.9).
pub const MAX_REMOVE_TAGS: usize = 64;

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    element: Value,
    /// Live tag per replica: the seq of the replica's newest add of this element.
    tags: BTreeMap<ReplicaId, u32>,
}

/// An add wins set. Elements are kept in the wire order of D61.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AddWinsSet {
    entries: Vec<Entry>,
}

impl AddWinsSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies an add of `element` by the op `id`, whose id becomes the tag. Returns whether
    /// the set changed (`false` for a replay of the same or an older add of that replica).
    pub fn add(&mut self, id: OpId, element: Value) -> Result<bool, CrdtError> {
        check_key(&element)?;
        match search(&self.entries, &element, |e| &e.element) {
            Ok(index) => {
                let Some(entry) = self.entries.get_mut(index) else {
                    return Ok(false);
                };
                match entry.tags.get(&id.replica) {
                    Some(&live) if live >= id.seq => Ok(false),
                    _ => {
                        entry.tags.insert(id.replica, id.seq);
                        Ok(true)
                    }
                }
            }
            Err(index) => {
                let tags = BTreeMap::from([(id.replica, id.seq)]);
                self.entries.insert(index, Entry { element, tags });
                Ok(true)
            }
        }
    }

    /// Applies a remove of `element` that names `tags`, between 1 and [`MAX_REMOVE_TAGS`].
    /// Deletes each named tag that is live with exactly that seq and drops the element when
    /// no tag is left. Returns whether the set changed.
    pub fn remove(&mut self, element: &Value, tags: &[OpId]) -> Result<bool, CrdtError> {
        check_key(element)?;
        if tags.is_empty() {
            return Err(CrdtError::NoTags);
        }
        if tags.len() > MAX_REMOVE_TAGS {
            return Err(CrdtError::TooManyTags(tags.len()));
        }
        let Ok(index) = search(&self.entries, element, |e| &e.element) else {
            return Ok(false);
        };
        let Some(entry) = self.entries.get_mut(index) else {
            return Ok(false);
        };
        let mut changed = false;
        for tag in tags {
            if entry.tags.get(&tag.replica) == Some(&tag.seq) {
                entry.tags.remove(&tag.replica);
                changed = true;
            }
        }
        if entry.tags.is_empty() {
            self.entries.remove(index);
        }
        Ok(changed)
    }

    /// Whether `element` is present.
    pub fn contains(&self, element: &Value) -> bool {
        search(&self.entries, element, |e| &e.element).is_ok()
    }

    /// Number of present elements.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no element is present.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Present elements in D61 order.
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.entries.iter().map(|e| &e.element)
    }

    /// Live tags of `element` in id order (equal to hex order); empty when absent.
    pub fn tags_of(&self, element: &Value) -> Vec<OpId> {
        search(&self.entries, element, |e| &e.element)
            .ok()
            .and_then(|index| self.entries.get(index))
            .map(entry_tags)
            .unwrap_or_default()
    }

    /// Tag lists for a local remove of `element`: its live tags in id order, split into
    /// lists of at most [`MAX_REMOVE_TAGS`]; empty when the element is absent.
    pub fn remove_tag_batches(&self, element: &Value) -> Vec<Vec<OpId>> {
        self.tags_of(element)
            .chunks(MAX_REMOVE_TAGS)
            .map(<[OpId]>::to_vec)
            .collect()
    }

    /// Canonical encoding of the present elements, `[e, ...]` in D61 order.
    pub fn encode_value(&self) -> String {
        let items: Vec<String> = self.values().map(encode).collect();
        encode_array(&items)
    }

    /// Canonical encoding of the full state `[[e, [tag, ...]], ...]`, elements in D61 order
    /// and tags as 24 digit hex in id order.
    pub fn encode_state(&self) -> String {
        let items: Vec<String> = self
            .entries
            .iter()
            .map(|entry| {
                let tags: Vec<String> = entry_tags(entry)
                    .into_iter()
                    .map(|t| encode_str(&t.to_hex()))
                    .collect();
                encode_array(&[encode(&entry.element), encode_array(&tags)])
            })
            .collect();
        encode_array(&items)
    }
}

fn entry_tags(entry: &Entry) -> Vec<OpId> {
    entry
        .tags
        .iter()
        .map(|(&replica, &seq)| OpId { replica, seq })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(replica: u64, seq: u32) -> OpId {
        OpId {
            replica: ReplicaId(replica),
            seq,
        }
    }

    fn text(s: &str) -> Value {
        Value::Text(s.to_owned())
    }

    #[test]
    fn concurrent_add_survives_remove() {
        let mut s = AddWinsSet::new();
        s.add(op(1, 1), text("x")).unwrap();
        s.add(op(3, 1), text("x")).unwrap();
        assert_eq!(s.remove(&text("x"), &[op(1, 1)]), Ok(true));
        assert!(s.contains(&text("x")));
        assert_eq!(s.tags_of(&text("x")), vec![op(3, 1)]);
    }

    #[test]
    fn readd_replaces_tag_and_replay_is_ignored() {
        let mut s = AddWinsSet::new();
        assert_eq!(s.add(op(1, 1), text("x")), Ok(true));
        assert_eq!(s.add(op(1, 2), text("x")), Ok(true));
        assert_eq!(s.add(op(1, 1), text("x")), Ok(false));
        assert_eq!(s.add(op(1, 2), text("x")), Ok(false));
        assert_eq!(s.remove(&text("x"), &[op(1, 1)]), Ok(false));
        assert_eq!(s.tags_of(&text("x")), vec![op(1, 2)]);
        assert_eq!(s.remove(&text("x"), &[op(1, 2)]), Ok(true));
        assert!(s.is_empty());
    }

    #[test]
    fn remove_checks_tag_count_before_any_change() {
        let mut s = AddWinsSet::new();
        s.add(op(1, 1), text("x")).unwrap();
        let before = s.clone();
        assert_eq!(s.remove(&text("x"), &[]), Err(CrdtError::NoTags));
        let many: Vec<OpId> = (1..=65).map(|i| op(1, i)).collect();
        assert_eq!(s.remove(&text("x"), &many), Err(CrdtError::TooManyTags(65)));
        assert_eq!(s, before);
        // Exactly 64 is allowed and removes the live tag among them.
        assert_eq!(s.remove(&text("x"), &many[..64]), Ok(true));
        assert!(s.is_empty());
    }

    #[test]
    fn non_scalar_elements_are_refused() {
        let mut s = AddWinsSet::new();
        for bad in [Value::Null, Value::List(vec![]), Value::Set(vec![])] {
            assert_eq!(s.add(op(1, 1), bad.clone()), Err(CrdtError::InvalidKey));
            assert_eq!(s.remove(&bad, &[op(1, 1)]), Err(CrdtError::InvalidKey));
        }
        assert!(s.is_empty());
    }

    #[test]
    fn numbers_are_one_element_by_value() {
        let mut s = AddWinsSet::new();
        s.add(op(1, 1), Value::int(0).unwrap()).unwrap();
        s.add(op(2, 1), Value::float(-0.0).unwrap()).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(
            s.encode_state(),
            r#"[[0,["000000000000000100000001","000000000000000200000001"]]]"#
        );
    }

    #[test]
    fn remove_batches_split_at_the_limit() {
        let mut s = AddWinsSet::new();
        for replica in 1..=130 {
            s.add(op(replica, 1), text("x")).unwrap();
        }
        let batches = s.remove_tag_batches(&text("x"));
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            [64, 64, 2]
        );
        for batch in &batches {
            s.remove(&text("x"), batch).unwrap();
        }
        assert!(s.is_empty());
        assert!(s.remove_tag_batches(&text("x")).is_empty());
    }
}
