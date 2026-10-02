//! Writes to materialised rows and their collection side tables (ARCHITECTURE 6.1, 6.2).
//!
//! The driver stores, it does not merge: `ostrel_crdt` and `ostrel_sync` have merged every
//! change before it reaches the driver (docs/db-mapping.md 1). The only decisions a driver
//! takes are the version check and the tag rules of [`SetChange`], which make replay of the
//! same change harmless.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use super::{DbError, FieldId, Hlc, ModelId, OpId, RowId, Value, compare_key};

/// A change to materialised rows, applied inside a transaction.
#[derive(Clone, Debug, PartialEq)]
pub enum Write {
    /// Creates the row with version 1. Fails with [`DbError::Conflict`] if `row` exists or
    /// existed in any model.
    Insert {
        model: ModelId,
        row: RowId,
        /// Column fields. A field not listed was never written (see `Row::fields`).
        fields: Vec<(FieldId, Value)>,
        /// Initial entries of collection fields. Only [`SetChange::Add`] is allowed here.
        collections: Vec<(FieldId, CollectionChange)>,
    },
    /// Sets the listed fields, applies the collection changes and increases the version by
    /// one; fields not listed keep their value. Fails with [`DbError::NotFound`] or
    /// [`DbError::VersionMismatch`].
    Update {
        model: ModelId,
        row: RowId,
        expect_version: u64,
        fields: Vec<(FieldId, Value)>,
        collections: Vec<(FieldId, CollectionChange)>,
    },
    /// Deletes the row with its collection entries and leaves a tombstone, so the id is never
    /// reused. Fails with [`DbError::NotFound`] or [`DbError::VersionMismatch`].
    Delete {
        model: ModelId,
        row: RowId,
        expect_version: u64,
    },
}

/// Changed entries of one collection field.
#[derive(Clone, Debug, PartialEq)]
pub enum CollectionChange {
    /// Tag changes of a `Set` field (D49), applied in order.
    Set(Vec<SetChange>),
    /// Entries of a `Map` field, each replacing the stored entry for its key. At most one
    /// entry per key.
    Map(Vec<MapEntry>),
}

/// One tag change of a `Set` field (D49, ARCHITECTURE 6.2). A set keeps at most one live tag
/// per (element, replica of the tag).
#[derive(Clone, Debug, PartialEq)]
pub enum SetChange {
    /// `tag` becomes the live tag of (`elem`, `tag.replica`), replacing an older one of the
    /// same replica. Ignored when that live tag already has a `seq` of at least `tag.seq`.
    Add { elem: Value, tag: OpId },
    /// Removes the live tag of (`elem`, `tag.replica`) if its `seq` is exactly `tag.seq`;
    /// otherwise ignored (idempotent replay, a newer re-add of the same replica survives).
    Remove { elem: Value, tag: OpId },
}

/// One entry of a `Map` field (per key last writer wins, ARCHITECTURE 6.2).
#[derive(Clone, Debug, PartialEq)]
pub struct MapEntry {
    pub key: Value,
    /// `None` for a removed key; the entry stays as a tombstone with its stamp.
    pub value: Option<Value>,
    /// Stamp of the op that wrote this entry.
    pub hlc: Hlc,
}

/// Checks a write batch against the rules of the contract. A driver calls it before it
/// touches the database; on error nothing of the batch is applied.
///
/// Rules: a field id appears at most once per write, across `fields` and `collections`;
/// column values are not `Set` or `Map` (those go through `collections`); an insert carries
/// no [`SetChange::Remove`]; a map change has at most one entry per key.
pub fn check_writes(writes: &[Write]) -> Result<(), DbError> {
    writes.iter().try_for_each(check_write)
}

fn check_write(w: &Write) -> Result<(), DbError> {
    let (fields, collections, insert) = match w {
        Write::Insert {
            fields,
            collections,
            ..
        } => (fields, collections, true),
        Write::Update {
            fields,
            collections,
            ..
        } => (fields, collections, false),
        Write::Delete { .. } => return Ok(()),
    };
    let mut seen = BTreeSet::new();
    let ids = fields.iter().map(|(f, _)| *f);
    for f in ids.chain(collections.iter().map(|(f, _)| *f)) {
        if !seen.insert(f) {
            return Err(DbError::Invalid("field written twice in one write"));
        }
    }
    if fields
        .iter()
        .any(|(_, v)| matches!(v, Value::Set(_) | Value::Map(_)))
    {
        return Err(DbError::Invalid("set or map value in a column field"));
    }
    for (_, change) in collections {
        match change {
            CollectionChange::Set(changes) => {
                if insert
                    && changes
                        .iter()
                        .any(|c| matches!(c, SetChange::Remove { .. }))
                {
                    return Err(DbError::Invalid("set remove in an insert"));
                }
            }
            CollectionChange::Map(entries) => {
                let mut keys: Vec<&Value> = entries.iter().map(|e| &e.key).collect();
                keys.sort_by(|a, b| compare_key(a, b));
                if keys
                    .windows(2)
                    .any(|p| matches!(p, [a, b] if compare_key(a, b) == Ordering::Equal))
                {
                    return Err(DbError::Invalid("map key written twice in one change"));
                }
            }
        }
    }
    Ok(())
}
