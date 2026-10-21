//! Ops on the wire (ARCHITECTURE 5.4 "Wire form", D132).
//!
//! An op is the object `{"id", "hlc", "model", "row", "body"}`. Models and fields travel by
//! name, never as [`ModelId`] or [`FieldId`]: ids are positions in the compiled schema, and
//! after a schema change an old offline op would silently write another field. The body is an
//! object with exactly one key:
//!
//! | Key | Content |
//! |---|---|
//! | `make` | `{<field>: value, ...}` |
//! | `drop` | `{}` |
//! | `set` | `{"field", "value"}` |
//! | `add` | `{"field", "elem"}` |
//! | `remove` | `{"field", "elem", "tags"}` |
//! | `put` | `{"field", "key", "value"}` |
//! | `remove_key` | `{"field", "key"}` |
//!
//! Two kinds of fault, as D132 sets them:
//!
//! * the form is wrong (not an object, a missing or extra key, a malformed `id`, `hlc` or
//!   `row`, a body that is not an object or names a kind that is not one of the seven):
//!   [`WireError`], fatal for the connection;
//! * the form is right but the op does not fit the schema (unknown model or field, a value of
//!   the wrong type, a body that does not suit the field), names two or more of the seven
//!   kinds (D147), has a `remove` whose tags are absent, `null`, empty, over the limit or not
//!   op ids (D135), or is over the op size limit: [`DecodedOp::Invalid`]. The server rejects
//!   it in step 6 of ARCHITECTURE 5.5, after the order check, so a replay still gets its
//!   original outcome.
//!
//! Whether a `make` names every declared field (D116) is checked by the server in step 6, not
//! here.

use ostrel_core::canon::{encode, encode_object, encode_str};
use ostrel_core::ids::OpId;

use super::frame::{Fields, array, hex, hlc, op_id, row_id, string};
use super::json::Json;
use super::value::{WireTy, read_value};
use super::{WireError, WireSchema, clip};
use crate::protocol::{DecodedOp, FieldId, LIMITS, ModelId, Op, OpBody, OpFault};

/// Reads one element of an `ops` array.
pub fn decode_op(json: Json, schema: &dyn WireSchema) -> Result<DecodedOp, WireError> {
    let Json::Obj(entries) = json else {
        return Err(WireError::WrongType("ops"));
    };
    let mut f = Fields { entries };
    let id = f.req("id", op_id)?;
    let stamp = f.req("hlc", hlc)?;
    let row = f.req("row", row_id)?;
    let model = f.req("model", string)?;
    let body = f.req("body", raw_body)?;
    f.finish()?;

    let invalid = |fault| DecodedOp::Invalid {
        id,
        hlc: stamp,
        row,
        fault,
    };
    let BodyForm::One(body) = body else {
        return Ok(invalid(OpFault::SeveralKinds));
    };
    let Some(model_id) = schema.model_id(&model) else {
        return Ok(invalid(OpFault::UnknownModel(clip(&model))));
    };
    let op = match body.typed(model_id, schema) {
        Ok(body) => Op {
            id,
            hlc: stamp,
            model: model_id,
            row,
            body,
        },
        Err(fault) => return Ok(invalid(fault)),
    };
    // The op was read from a frame of at most 1 MiB, so encoding it again is cheap and
    // measures it exactly as the client measured it before sending.
    let bytes = op_bytes(&op, schema)?;
    if bytes > LIMITS.op_bytes as usize {
        return Ok(invalid(OpFault::TooLarge { bytes }));
    }
    Ok(DecodedOp::Valid(op))
}

/// Writes one op as the canonical object that stands in an `ops` array.
///
/// Fails with [`WireError::NotInSchema`] when the schema does not know a model or field id of
/// the op: that is a defect of the caller, never of the peer.
pub fn encode_op(op: &Op, schema: &dyn WireSchema) -> Result<String, WireError> {
    let model = schema
        .model_name(op.model)
        .ok_or(WireError::NotInSchema("model"))?;
    let field = |id: FieldId| {
        schema
            .field_name(op.model, id)
            .map(encode_str)
            .ok_or(WireError::NotInSchema("field"))
    };
    let (kind, body) = match &op.body {
        OpBody::Make { fields } => {
            let names = fields
                .iter()
                .map(|(id, value)| {
                    let name = schema
                        .field_name(op.model, *id)
                        .ok_or(WireError::NotInSchema("field"))?;
                    Ok((name, encode(value)))
                })
                .collect::<Result<Vec<_>, WireError>>()?;
            ("make", object(&names)?)
        }
        OpBody::Drop => ("drop", "{}".to_owned()),
        OpBody::Set { field: id, value } => (
            "set",
            object(&[("field", field(*id)?), ("value", encode(value))])?,
        ),
        OpBody::SetAdd { field: id, elem } => (
            "add",
            object(&[("field", field(*id)?), ("elem", encode(elem))])?,
        ),
        OpBody::SetRemove {
            field: id,
            elem,
            tags,
        } => (
            "remove",
            object(&[
                ("field", field(*id)?),
                ("elem", encode(elem)),
                ("tags", array(tags.iter().map(|t| hex(&t.to_hex())))),
            ])?,
        ),
        OpBody::MapPut {
            field: id,
            key,
            value,
        } => (
            "put",
            object(&[
                ("field", field(*id)?),
                ("key", encode(key)),
                ("value", encode(value)),
            ])?,
        ),
        OpBody::MapRemove { field: id, key } => (
            "remove_key",
            object(&[("field", field(*id)?), ("key", encode(key))])?,
        ),
    };
    object(&[
        ("id", hex(&op.id.to_hex())),
        ("hlc", hex(&op.hlc.to_hex())),
        ("model", encode_str(model)),
        ("row", hex(&op.row.to_hex())),
        ("body", object(&[(kind, body)])?),
    ])
}

/// Size of an op for the limit of D109: the length of its canonical encoding as it stands in
/// the `ops` array, without the frame's `v` and `t` and without array separators (D132).
pub fn op_bytes(op: &Op, schema: &dyn WireSchema) -> Result<usize, WireError> {
    encode_op(op, schema).map(|text| text.len())
}

fn object(fields: &[(&str, String)]) -> Result<String, WireError> {
    encode_object(fields).map_err(|e| WireError::DuplicateKey(clip(&e.0)))
}

/// A body whose form is checked and whose names and values are not resolved yet.
enum RawBody {
    Make(Vec<(String, Json)>),
    Drop,
    Set {
        field: String,
        value: Json,
    },
    Add {
        field: String,
        elem: Json,
    },
    Remove {
        field: String,
        elem: Json,
        /// `None` when the key `tags` is absent. That makes only this op invalid, not the
        /// frame: the tags are content to check against the limit, like a value (D135).
        tags: Option<Json>,
    },
    Put {
        field: String,
        key: Json,
        value: Json,
    },
    RemoveKey {
        field: String,
        key: Json,
    },
}

/// A body whose form is right, or one that names several kinds of the seven (D147).
enum BodyForm {
    One(RawBody),
    SeveralKinds,
}

/// Checks the form of `body`: an object of one or more known kinds, each holding an object
/// with exactly the keys of that kind. An unknown kind or a malformed kind is fatal, also next
/// to a well formed one; two or more well formed kinds make only the op invalid (D147).
fn raw_body(key: &'static str, json: Json) -> Result<BodyForm, WireError> {
    let Json::Obj(entries) = json else {
        return Err(WireError::WrongType(key));
    };
    let several = entries.len() > 1;
    let mut bodies = entries
        .into_iter()
        .map(|(kind, inner)| raw_kind(key, &kind, inner))
        .collect::<Result<Vec<_>, _>>()?;
    match bodies.pop() {
        Some(body) if !several => Ok(BodyForm::One(body)),
        Some(_) => Ok(BodyForm::SeveralKinds),
        None => Err(WireError::WrongType(key)),
    }
}

/// Checks one kind of a body: its object has exactly the keys of that kind.
fn raw_kind(key: &'static str, kind: &str, inner: Json) -> Result<RawBody, WireError> {
    let Json::Obj(entries) = inner else {
        return Err(WireError::WrongType(key));
    };
    if kind == "make" {
        return Ok(RawBody::Make(entries));
    }
    let mut f = Fields { entries };
    let body = match kind {
        "drop" => RawBody::Drop,
        "set" => RawBody::Set {
            field: f.req("field", string)?,
            value: f.req("value", any)?,
        },
        "add" => RawBody::Add {
            field: f.req("field", string)?,
            elem: f.req("elem", any)?,
        },
        "remove" => RawBody::Remove {
            field: f.req("field", string)?,
            elem: f.req("elem", any)?,
            tags: if f.has("tags") {
                Some(f.req("tags", any)?)
            } else {
                None
            },
        },
        "put" => RawBody::Put {
            field: f.req("field", string)?,
            key: f.req("key", any)?,
            value: f.req("value", any)?,
        },
        "remove_key" => RawBody::RemoveKey {
            field: f.req("field", string)?,
            key: f.req("key", any)?,
        },
        _ => return Err(WireError::WrongType(key)),
    };
    f.finish()?;
    Ok(body)
}

fn any(_: &'static str, json: Json) -> Result<Json, WireError> {
    Ok(json)
}

impl RawBody {
    /// Resolves names and reads values against the schema.
    fn typed(self, model: ModelId, schema: &dyn WireSchema) -> Result<OpBody, OpFault> {
        let field = |name: &str| -> Result<(FieldId, &WireTy), OpFault> {
            schema
                .field_id(model, name)
                .and_then(|id| schema.field_ty(model, id).map(|ty| (id, ty)))
                .ok_or_else(|| OpFault::UnknownField(clip(name)))
        };
        let value = |id: FieldId, ty: &WireTy, json: Json| {
            read_value(ty, json).map_err(|fault| OpFault::Value { field: id, fault })
        };
        Ok(match self {
            RawBody::Make(entries) => {
                let mut fields = Vec::with_capacity(entries.len());
                for (name, json) in entries {
                    let (id, ty) = field(&name)?;
                    fields.push((id, value(id, ty, json)?));
                }
                // Declaration order, whatever the key order of the frame.
                fields.sort_by_key(|(id, _)| *id);
                OpBody::Make { fields }
            }
            RawBody::Drop => OpBody::Drop,
            RawBody::Set {
                field: name,
                value: v,
            } => {
                let (id, ty) = field(&name)?;
                if matches!(ty, WireTy::Set(_) | WireTy::Map(..)) {
                    return Err(OpFault::WrongBody { field: id });
                }
                OpBody::Set {
                    field: id,
                    value: value(id, ty, v)?,
                }
            }
            RawBody::Add { field: name, elem } => {
                let (id, ty) = field(&name)?;
                let WireTy::Set(elem_ty) = ty else {
                    return Err(OpFault::WrongBody { field: id });
                };
                OpBody::SetAdd {
                    field: id,
                    elem: value(id, elem_ty, elem)?,
                }
            }
            RawBody::Remove {
                field: name,
                elem,
                tags,
            } => {
                let (id, ty) = field(&name)?;
                let WireTy::Set(elem_ty) = ty else {
                    return Err(OpFault::WrongBody { field: id });
                };
                OpBody::SetRemove {
                    field: id,
                    elem: value(id, elem_ty, elem)?,
                    tags: remove_tags(tags).ok_or(OpFault::Tags { field: id })?,
                }
            }
            RawBody::Put {
                field: name,
                key,
                value: v,
            } => {
                let (id, ty) = field(&name)?;
                let WireTy::Map(key_ty, value_ty) = ty else {
                    return Err(OpFault::WrongBody { field: id });
                };
                OpBody::MapPut {
                    field: id,
                    key: value(id, key_ty, key)?,
                    value: value(id, value_ty, v)?,
                }
            }
            RawBody::RemoveKey { field: name, key } => {
                let (id, ty) = field(&name)?;
                let WireTy::Map(key_ty, _) = ty else {
                    return Err(OpFault::WrongBody { field: id });
                };
                OpBody::MapRemove {
                    field: id,
                    key: value(id, key_ty, key)?,
                }
            }
        })
    }
}

/// The tags of a `remove`: 1 to `tags_per_remove` op ids (ARCHITECTURE 5.9, 6.2). `None`
/// for anything else, an absent key included.
fn remove_tags(json: Option<Json>) -> Option<Vec<OpId>> {
    let Some(Json::Arr(items)) = json else {
        return None;
    };
    if items.is_empty() || items.len() > LIMITS.tags_per_remove as usize {
        return None;
    }
    items
        .into_iter()
        .map(|item| match item {
            Json::Str(s) => OpId::from_hex(&s).ok(),
            _ => None,
        })
        .collect()
}
