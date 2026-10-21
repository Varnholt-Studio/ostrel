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
//!
//! The schema also resolves the implicit fields `made`, `changed` and `author`, because rows and
//! queries name them (D186). Only the server stamps them, so a body that names one of them is
//! read as if the name were unknown ([`OpFault::UnknownField`]), and [`encode_op`] refuses an op
//! that writes one.

use ostrel_core::canon::{encode, encode_object, encode_str};
use ostrel_core::ids::OpId;
use ostrel_db::api::is_implicit_field;

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
/// the op, or when the op writes an implicit field (D186): that is a defect of the caller,
/// never of the peer.
pub fn encode_op(op: &Op, schema: &dyn WireSchema) -> Result<String, WireError> {
    let model = schema
        .model_name(op.model)
        .ok_or(WireError::NotInSchema("model"))?;
    let name = |id: FieldId| {
        if is_implicit_field(id) {
            return Err(WireError::NotInSchema("field"));
        }
        schema
            .field_name(op.model, id)
            .ok_or(WireError::NotInSchema("field"))
    };
    let field = |id: FieldId| name(id).map(encode_str);
    let (kind, body) = match &op.body {
        OpBody::Make { fields } => {
            let names = fields
                .iter()
                .map(|(id, value)| Ok((name(*id)?, encode(value))))
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
    /// Resolves names and reads values against the schema. A name of an implicit field is
    /// refused like an unknown name, so no op writes a stamp or the author (D186, D116).
    fn typed(self, model: ModelId, schema: &dyn WireSchema) -> Result<OpBody, OpFault> {
        let field = |name: &str| -> Result<(FieldId, &WireTy), OpFault> {
            schema
                .field_id(model, name)
                .filter(|id| !is_implicit_field(*id))
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

#[cfg(test)]
mod tests {
    use super::*;

    use ostrel_core::ids::{Hlc, ReplicaId, RowId};
    use ostrel_core::value::Value;
    use ostrel_db::api::{FIELD_AUTHOR, FIELD_CHANGED, FIELD_MADE, FIRST_IMPLICIT_FIELD};

    use super::super::json;
    use crate::protocol::FnId;

    /// Model 0 `Note` with the declared `title` (0) and `tags` (1), and the implicit fields
    /// resolved as D186 asks a schema to resolve them.
    struct Schema;

    const TAGS: WireTy = WireTy::Text;

    fn declared(field: FieldId) -> Option<(&'static str, WireTy)> {
        match field {
            0 => Some(("title", WireTy::Text)),
            1 => Some(("tags", WireTy::Set(Box::new(TAGS)))),
            FIELD_MADE => Some(("made", WireTy::Time)),
            FIELD_CHANGED => Some(("changed", WireTy::Time)),
            FIELD_AUTHOR => Some(("author", WireTy::Ref(0))),
            _ => None,
        }
    }

    const FIELDS: [FieldId; 5] = [0, 1, FIELD_MADE, FIELD_CHANGED, FIELD_AUTHOR];

    // Built once, so that `field_ty` can hand out a reference.
    fn types() -> &'static [WireTy] {
        use std::sync::OnceLock;
        static TYPES: OnceLock<Vec<WireTy>> = OnceLock::new();
        TYPES.get_or_init(|| {
            FIELDS
                .iter()
                .filter_map(|f| declared(*f).map(|(_, ty)| ty))
                .collect()
        })
    }

    impl WireSchema for Schema {
        fn model_id(&self, name: &str) -> Option<ModelId> {
            (name == "Note").then_some(0)
        }

        fn model_name(&self, model: ModelId) -> Option<&str> {
            (model == 0).then_some("Note")
        }

        fn field_id(&self, model: ModelId, name: &str) -> Option<FieldId> {
            let found = FIELDS
                .into_iter()
                .find(|f| declared(*f).is_some_and(|(n, _)| n == name));
            (model == 0).then_some(found)?
        }

        fn field_name(&self, model: ModelId, field: FieldId) -> Option<&str> {
            (model == 0).then(|| declared(field).map(|(n, _)| n))?
        }

        fn field_ty(&self, model: ModelId, field: FieldId) -> Option<&WireTy> {
            let at = FIELDS.iter().position(|f| *f == field)?;
            (model == 0).then(|| types().get(at))?
        }

        fn function_id(&self, _: &str) -> Option<FnId> {
            None
        }

        fn function_name(&self, _: FnId) -> Option<&str> {
            None
        }

        fn function_params(&self, _: FnId) -> Option<&[WireTy]> {
            None
        }

        fn function_result(&self, _: FnId) -> Option<&WireTy> {
            None
        }
    }

    const HEAD: &str = r#""hlc":"00010203040500070123456789abcdef","id":"0123456789abcdef00000001","model":"Note","row":"00010203040500070123456789abcdef""#;

    fn decode(body: &str) -> Result<DecodedOp, String> {
        let text = format!(r#"{{"body":{body},{HEAD}}}"#);
        let json = json::parse(text.as_bytes()).map_err(|e| format!("{e:?}"))?;
        decode_op(json, &Schema).map_err(|e| format!("{e:?}"))
    }

    fn unknown_field(decoded: &DecodedOp) -> Option<&str> {
        match decoded {
            DecodedOp::Invalid {
                fault: OpFault::UnknownField(name),
                ..
            } => Some(name.as_str()),
            _ => None,
        }
    }

    #[test]
    fn the_test_schema_resolves_implicit_names() {
        assert_eq!(Schema.field_id(0, "made"), Some(FIELD_MADE));
        assert_eq!(Schema.field_ty(0, FIELD_AUTHOR), Some(&WireTy::Ref(0)));
        assert_eq!(Schema.field_name(0, FIELD_CHANGED), Some("changed"));
    }

    /// A body that writes `made`, `changed` or `author` is `UnknownField`, whatever its kind
    /// (D186), exactly as a name the schema does not know.
    #[test]
    fn an_op_that_writes_an_implicit_field_is_unknown_field() -> Result<(), String> {
        for name in ["made", "changed", "author"] {
            let value = if name == "author" {
                r#""00010203040500070123456789abcdef""#
            } else {
                "5"
            };
            let bodies = [
                format!(r#"{{"set":{{"field":"{name}","value":{value}}}}}"#),
                format!(r#"{{"make":{{"{name}":{value},"title":"x"}}}}"#),
                format!(r#"{{"add":{{"field":"{name}","elem":{value}}}}}"#),
                format!(
                    r#"{{"remove":{{"elem":{value},"field":"{name}","tags":["0123456789abcdef00000001"]}}}}"#
                ),
                format!(r#"{{"put":{{"field":"{name}","key":{value},"value":{value}}}}}"#),
                format!(r#"{{"remove_key":{{"field":"{name}","key":{value}}}}}"#),
            ];
            for body in bodies {
                let decoded = decode(&body)?;
                assert_eq!(unknown_field(&decoded), Some(name), "{body}: {decoded:?}");
            }
        }
        Ok(())
    }

    /// The guard does not touch declared fields.
    #[test]
    fn declared_fields_still_decode() -> Result<(), String> {
        let decoded = decode(r#"{"set":{"field":"title","value":"x"}}"#)?;
        assert!(matches!(decoded, DecodedOp::Valid(_)), "{decoded:?}");
        let decoded = decode(r#"{"add":{"elem":"a","field":"tags"}}"#)?;
        assert!(matches!(decoded, DecodedOp::Valid(_)), "{decoded:?}");
        Ok(())
    }

    /// Writing an op that sets an implicit field is a defect of the caller, never a frame.
    #[test]
    fn encode_refuses_an_op_that_writes_an_implicit_field() -> Result<(), String> {
        let replica = ReplicaId(0x0123_4567_89ab_cdef);
        let hlc = Hlc::new(0x0001_0203_0405, 7, replica).map_err(|e| format!("{e:?}"))?;
        let op = |body| Op {
            id: OpId { replica, seq: 1 },
            hlc,
            model: 0,
            row: RowId::new(hlc),
            body,
        };
        let time = Value::time(5).map_err(|e| format!("{e:?}"))?;
        for field in [
            FIELD_MADE,
            FIELD_CHANGED,
            FIELD_AUTHOR,
            FIRST_IMPLICIT_FIELD + 7,
        ] {
            let set = op(OpBody::Set {
                field,
                value: time.clone(),
            });
            assert_eq!(
                encode_op(&set, &Schema),
                Err(WireError::NotInSchema("field")),
                "{field:#x}"
            );
            let make = op(OpBody::Make {
                fields: vec![(0, Value::Text("x".into())), (field, time.clone())],
            });
            assert_eq!(
                encode_op(&make, &Schema),
                Err(WireError::NotInSchema("field")),
                "{field:#x}"
            );
        }
        let title = op(OpBody::Set {
            field: 0,
            value: Value::Text("x".into()),
        });
        assert!(encode_op(&title, &Schema).is_ok());
        Ok(())
    }
}
