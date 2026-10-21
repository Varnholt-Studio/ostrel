//! Rows on the wire: the rows of a `snapshot` (ARCHITECTURE 5.4 "Row", D132).
//!
//! A row is the object
//!
//! ```text
//! {"id": row id,
//!  "fields": {<name>: value, ...},
//!  "sets": {<name>: [[elem, [tag, ...]], ...], ...},
//!  "maps": {<name>: [[key, {"hlc", "value"}] or [key, {"hlc", "removed": true}], ...], ...}}
//! ```
//!
//! `fields` holds the column fields, the implicit `made`, `changed` and `author` included.
//! `sets` lists each element once with the tags of all its live adds, and `maps` every entry
//! ever written, removed ones with `"removed": true`; both in the order of
//! [`CollectionState`]. A row belongs to the model of its subscription, which the wire does not
//! name ([`super::Requests`]).
//!
//! The row `version` is internal to the database and does not travel: a row read from the wire
//! has version 0.
//!
//! Every fault in a row is fatal. Rows come only from the server, and client and server share
//! one compiled schema, so a name or value that does not fit is a mismatch, not a single bad
//! request.

use std::cmp::Ordering;

use ostrel_core::canon::{encode, encode_object};
use ostrel_core::value::compare_key;
use ostrel_db::api::{CollectionState, MapEntry, SetTag};

use super::frame::{Fields, array, boolean, hex, hlc, op_id, row_id};
use super::json::Json;
use super::value::{WireTy, read_value};
use super::{WireError, WireSchema, clip};
use crate::protocol::{FieldId, LIMITS, ModelId, Row, Value};

/// Size of a row for the row limit of D121: the length of its canonical encoding as it stands
/// in the `rows` array of a `snapshot`.
pub fn row_bytes(row: &Row, model: ModelId, schema: &dyn WireSchema) -> Result<usize, WireError> {
    encode_row(row, model, schema).map(|text| text.len())
}

// ---------------------------------------------------------------------------------------------
// Writing

/// Writes one row of `model`. A row over the row limit is [`WireError::RowTooLarge`]: the
/// server never stores one (D121), so this is a defect of the caller.
pub(super) fn encode_row(
    row: &Row,
    model: ModelId,
    schema: &dyn WireSchema,
) -> Result<String, WireError> {
    let name = |field: FieldId| {
        schema
            .field_name(model, field)
            .ok_or(WireError::NotInSchema("field"))
    };
    let fields = row
        .fields
        .iter()
        .map(|(field, value)| Ok((name(*field)?, encode(value))))
        .collect::<Result<Vec<_>, WireError>>()?;
    let mut sets = Vec::new();
    let mut maps = Vec::new();
    for (field, state) in &row.collections {
        match state {
            CollectionState::Set(tags) => sets.push((name(*field)?, set_groups(tags))),
            CollectionState::Map(entries) => maps.push((name(*field)?, map_entries(entries)?)),
        }
    }
    let text = object(&[
        ("id", hex(&row.id.to_hex())),
        ("fields", object(&fields)?),
        ("sets", object(&sets)?),
        ("maps", object(&maps)?),
    ])?;
    if text.len() > LIMITS.row_bytes as usize {
        return Err(WireError::RowTooLarge {
            row: row.id,
            bytes: text.len(),
        });
    }
    Ok(text)
}

/// `[[elem, [tag, ...]], ...]`: the live tags grouped by element, in their stored order.
fn set_groups(tags: &[SetTag]) -> String {
    let mut groups: Vec<(&Value, Vec<String>)> = Vec::new();
    for SetTag { elem, tag } in tags {
        match groups.last_mut() {
            Some((last, tags)) if compare_key(last, elem) == Ordering::Equal => {
                tags.push(hex(&tag.to_hex()));
            }
            _ => groups.push((elem, vec![hex(&tag.to_hex())])),
        }
    }
    array(
        groups
            .into_iter()
            .map(|(elem, tags)| array([encode(elem), array(tags)])),
    )
}

/// `[[key, {"hlc", "value"}] or [key, {"hlc", "removed": true}], ...]`.
fn map_entries(entries: &[MapEntry]) -> Result<String, WireError> {
    let items = entries
        .iter()
        .map(|entry| {
            let stamp = ("hlc", hex(&entry.hlc.to_hex()));
            let state = match &entry.value {
                Some(value) => object(&[stamp, ("value", encode(value))])?,
                None => object(&[stamp, ("removed", "true".to_owned())])?,
            };
            Ok(array([encode(&entry.key), state]))
        })
        .collect::<Result<Vec<_>, WireError>>()?;
    Ok(array(items))
}

fn object(fields: &[(&str, String)]) -> Result<String, WireError> {
    encode_object(fields).map_err(|e| WireError::DuplicateKey(clip(&e.0)))
}

// ---------------------------------------------------------------------------------------------
// Reading

/// Reads one row of `model`.
pub(super) fn decode_row(
    json: Json,
    model: ModelId,
    schema: &dyn WireSchema,
) -> Result<Row, WireError> {
    let Json::Obj(entries) = json else {
        return Err(WireError::WrongType("rows"));
    };
    let mut f = Fields { entries };
    let id = f.req("id", row_id)?;
    let reader = Reader { schema, model };
    let fields = f.req("fields", |key, json| reader.each(key, json, Reader::column))?;
    let sets = f.req("sets", |key, json| reader.each(key, json, Reader::set))?;
    let maps = f.req("maps", |key, json| reader.each(key, json, Reader::map))?;
    f.finish()?;

    // A field stands in at most one of the three objects: `column`, `set` and `map` each
    // accept only their kind of field type, and no object repeats a key.
    let mut fields = fields;
    let mut collections: Vec<(FieldId, CollectionState)> = sets.into_iter().chain(maps).collect();
    fields.sort_by_key(|(field, _)| *field);
    collections.sort_by_key(|(field, _)| *field);
    Ok(Row {
        id,
        version: 0,
        fields,
        collections,
    })
}

struct Reader<'s> {
    schema: &'s dyn WireSchema,
    model: ModelId,
}

impl Reader<'_> {
    /// Reads each entry of the object under `key` with `read`, which gets the field's id and
    /// wire type.
    fn each<T>(
        &self,
        key: &'static str,
        json: Json,
        read: fn(&Self, &'static str, FieldId, &WireTy, Json) -> Result<T, WireError>,
    ) -> Result<Vec<(FieldId, T)>, WireError> {
        let Json::Obj(entries) = json else {
            return Err(WireError::WrongType(key));
        };
        entries
            .into_iter()
            .map(|(name, json)| {
                let (field, ty) = self
                    .schema
                    .field_id(self.model, &name)
                    .and_then(|id| Some((id, self.schema.field_ty(self.model, id)?)))
                    .ok_or_else(|| WireError::UnknownField(clip(&name)))?;
                Ok((field, read(self, key, field, ty, json)?))
            })
            .collect()
    }

    /// A column value; a `Set` or `Map` field belongs under `sets` or `maps`.
    fn column(
        &self,
        key: &'static str,
        _: FieldId,
        ty: &WireTy,
        json: Json,
    ) -> Result<Value, WireError> {
        if matches!(ty, WireTy::Set(_) | WireTy::Map(..)) {
            return Err(WireError::WrongType(key));
        }
        value(key, ty, json)
    }

    /// `[[elem, [tag, ...]], ...]`, every element with at least one tag.
    fn set(
        &self,
        key: &'static str,
        _: FieldId,
        ty: &WireTy,
        json: Json,
    ) -> Result<CollectionState, WireError> {
        let WireTy::Set(elem_ty) = ty else {
            return Err(WireError::WrongType(key));
        };
        let mut tags = Vec::new();
        for group in items(key, json)? {
            let [elem, group_tags] = pair(key, group)?;
            let elem = value(key, elem_ty, elem)?;
            let group_tags = items(key, group_tags)?;
            if group_tags.is_empty() {
                return Err(WireError::WrongType(key));
            }
            for tag in group_tags {
                tags.push(SetTag {
                    elem: elem.clone(),
                    tag: op_id(key, tag)?,
                });
            }
        }
        Ok(CollectionState::Set(tags))
    }

    /// `[[key, {"hlc", "value"}] or [key, {"hlc", "removed": true}], ...]`.
    fn map(
        &self,
        key: &'static str,
        _: FieldId,
        ty: &WireTy,
        json: Json,
    ) -> Result<CollectionState, WireError> {
        let WireTy::Map(key_ty, value_ty) = ty else {
            return Err(WireError::WrongType(key));
        };
        let mut entries = Vec::new();
        for item in items(key, json)? {
            let [map_key, state] = pair(key, item)?;
            let Json::Obj(state) = state else {
                return Err(WireError::WrongType(key));
            };
            let mut f = Fields { entries: state };
            let stamp = f.req("hlc", hlc)?;
            let entry_value = if f.has("removed") {
                if !f.req("removed", boolean)? {
                    return Err(WireError::WrongType("removed"));
                }
                None
            } else {
                Some(f.req("value", |k, json| value(k, value_ty, json))?)
            };
            f.finish()?;
            entries.push(MapEntry {
                key: value(key, key_ty, map_key)?,
                value: entry_value,
                hlc: stamp,
            });
        }
        Ok(CollectionState::Map(entries))
    }
}

fn value(key: &'static str, ty: &WireTy, json: Json) -> Result<Value, WireError> {
    read_value(ty, json).map_err(|fault| WireError::Value { key, fault })
}

fn items(key: &'static str, json: Json) -> Result<Vec<Json>, WireError> {
    match json {
        Json::Arr(items) => Ok(items),
        _ => Err(WireError::WrongType(key)),
    }
}

fn pair(key: &'static str, json: Json) -> Result<[Json; 2], WireError> {
    <[Json; 2]>::try_from(items(key, json)?).map_err(|_| WireError::WrongType(key))
}

#[cfg(test)]
mod tests {
    use ostrel_core::ids::{OpId, ReplicaId};

    use super::*;

    #[test]
    fn set_groups_join_the_tags_of_one_element() {
        let tag = |seq| OpId {
            replica: ReplicaId(1),
            seq,
        };
        let text = |s: &str| Value::Text(s.into());
        let tags = [
            SetTag {
                elem: text("a"),
                tag: tag(1),
            },
            SetTag {
                elem: text("a"),
                tag: tag(2),
            },
            SetTag {
                elem: text("b"),
                tag: tag(3),
            },
        ];
        let hexed = |seq| tag(seq).to_hex();
        assert_eq!(
            set_groups(&tags),
            format!(
                r#"[["a",["{}","{}"]],["b",["{}"]]]"#,
                hexed(1),
                hexed(2),
                hexed(3)
            )
        );
        assert_eq!(set_groups(&[]), "[]");
    }
}
