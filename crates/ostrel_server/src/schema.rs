//! The names and wire types of the compiled schema, as the sync codec reads them (D132).
//!
//! [`ServerSchema`] implements [`ostrel_sync::wire::WireSchema`] for the server. It is built
//! once per program from the models in declaration order: the position of a model is its
//! [`ModelId`] and the position of a declared field inside its model is its [`FieldId`], the
//! same positions the database and the IR use. Ids never travel; the codec maps names to ids
//! when it reads a frame and ids back to names when it writes one (ARCHITECTURE 5.4).
//!
//! Every lookup is a hash map lookup on a name received from a client, so a frame full of
//! unknown names costs no more than one with known names.
//!
//! Only declared fields are part of the schema. The implicit fields (`id`, `made`, `changed`,
//! and `author` in a program with `auth`) are never written by an op (the `make` body names
//! every declared field once, D116), so a name of an implicit field in an op body is an
//! unknown field. Because rows carry the implicit fields under the same names, a declared
//! field may not use one of them ([`SchemaError::ReservedField`]). A program without `auth`
//! has no implicit `author`, so there a declared field `author` is an ordinary field
//! (SPEC 12.6, G8).
//!
//! Server functions are not part of the schema yet: they join the trait together with the
//! `Call` and `Return` messages (D135).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;

use ostrel_sync::protocol::{FieldId, ModelId};
use ostrel_sync::wire::{WireSchema, WireTy};

/// Names of the implicit fields every row carries next to the declared fields.
pub const IMPLICIT_FIELDS: [&str; 3] = ["id", "made", "changed"];

/// Name of the implicit field that rows carry only in a program with `auth` (G8, D142).
pub const AUTHOR_FIELD: &str = "author";

/// Whether the program declares `auth`, which decides if `author` is an implicit field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    /// The `app` has no `auth` line: rows have no implicit `author`.
    Off,
    /// The `app` has an `auth` line: rows carry the implicit `author`.
    On,
}

impl Auth {
    /// Whether `name` is an implicit field of every row of a program with this setting.
    pub fn is_implicit(self, name: &str) -> bool {
        IMPLICIT_FIELDS.contains(&name) || (self == Auth::On && name == AUTHOR_FIELD)
    }
}

/// One model as the compiler declares it: its name and its declared fields in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
}

/// One declared field: its name and the wire type of its values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDecl {
    pub name: String,
    pub ty: WireTy,
}

/// Why a list of models is not a schema. Every kind is a defect of the compiler output,
/// never caused by a client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaError {
    /// A model has an empty name.
    EmptyModelName,
    /// A field of this model has an empty name.
    EmptyFieldName { model: String },
    /// Two models have this name.
    DuplicateModel(String),
    /// This model declares this field twice.
    DuplicateField { model: String, field: String },
    /// This model declares a field with the name of an implicit field.
    ReservedField { model: String, field: String },
    /// More models, or more fields in one model, than an id can number.
    TooMany,
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchemaError::EmptyModelName => write!(f, "a model has an empty name"),
            SchemaError::EmptyFieldName { model } => {
                write!(f, "model `{model}` has a field with an empty name")
            }
            SchemaError::DuplicateModel(name) => write!(f, "model `{name}` is declared twice"),
            SchemaError::DuplicateField { model, field } => {
                write!(f, "model `{model}` declares field `{field}` twice")
            }
            SchemaError::ReservedField { model, field } => {
                write!(f, "model `{model}` declares the implicit field `{field}`")
            }
            SchemaError::TooMany => write!(f, "more models or fields than an id can number"),
        }
    }
}

impl std::error::Error for SchemaError {}

/// The schema of one compiled program for the sync codec.
#[derive(Clone, Debug)]
pub struct ServerSchema {
    models: Vec<ModelEntry>,
    model_ids: HashMap<String, ModelId>,
}

#[derive(Clone, Debug)]
struct ModelEntry {
    name: String,
    fields: Vec<FieldDecl>,
    field_ids: HashMap<String, FieldId>,
}

impl ServerSchema {
    /// Builds the schema from the models in declaration order. `auth` tells whether the
    /// program declares `auth`, which makes `author` an implicit field.
    pub fn new(models: Vec<ModelDecl>, auth: Auth) -> Result<Self, SchemaError> {
        let mut entries = Vec::with_capacity(models.len());
        let mut model_ids = HashMap::with_capacity(models.len());
        for (at, model) in models.into_iter().enumerate() {
            let id = ModelId::try_from(at).map_err(|_| SchemaError::TooMany)?;
            if model.name.is_empty() {
                return Err(SchemaError::EmptyModelName);
            }
            let entry = ModelEntry::new(model, auth)?;
            match model_ids.entry(entry.name.clone()) {
                Entry::Occupied(_) => return Err(SchemaError::DuplicateModel(entry.name)),
                Entry::Vacant(slot) => {
                    slot.insert(id);
                }
            }
            entries.push(entry);
        }
        Ok(Self {
            models: entries,
            model_ids,
        })
    }

    /// The number of models.
    pub fn model_count(&self) -> usize {
        self.models.len()
    }

    fn model(&self, model: ModelId) -> Option<&ModelEntry> {
        self.models.get(usize::try_from(model).ok()?)
    }

    fn field(&self, model: ModelId, field: FieldId) -> Option<&FieldDecl> {
        self.model(model)?.fields.get(usize::try_from(field).ok()?)
    }
}

impl ModelEntry {
    fn new(model: ModelDecl, auth: Auth) -> Result<Self, SchemaError> {
        let mut field_ids = HashMap::with_capacity(model.fields.len());
        for (at, field) in model.fields.iter().enumerate() {
            let id = FieldId::try_from(at).map_err(|_| SchemaError::TooMany)?;
            if field.name.is_empty() {
                return Err(SchemaError::EmptyFieldName { model: model.name });
            }
            if auth.is_implicit(&field.name) {
                return Err(SchemaError::ReservedField {
                    model: model.name,
                    field: field.name.clone(),
                });
            }
            if field_ids.insert(field.name.clone(), id).is_some() {
                return Err(SchemaError::DuplicateField {
                    model: model.name,
                    field: field.name.clone(),
                });
            }
        }
        Ok(Self {
            name: model.name,
            fields: model.fields,
            field_ids,
        })
    }
}

impl WireSchema for ServerSchema {
    fn model_id(&self, name: &str) -> Option<ModelId> {
        self.model_ids.get(name).copied()
    }

    fn model_name(&self, model: ModelId) -> Option<&str> {
        Some(self.model(model)?.name.as_str())
    }

    fn field_id(&self, model: ModelId, name: &str) -> Option<FieldId> {
        self.model(model)?.field_ids.get(name).copied()
    }

    fn field_name(&self, model: ModelId, field: FieldId) -> Option<&str> {
        Some(self.field(model, field)?.name.as_str())
    }

    fn field_ty(&self, model: ModelId, field: FieldId) -> Option<&WireTy> {
        Some(&self.field(model, field)?.ty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ostrel_core::ids::{Hlc, OpId, ReplicaId, RowId};
    use ostrel_core::value::Value;
    use ostrel_sync::protocol::{Op, OpBody};
    use ostrel_sync::wire::{decode_op, encode_op, json};

    fn field(name: &str, ty: WireTy) -> FieldDecl {
        FieldDecl {
            name: name.into(),
            ty,
        }
    }

    fn model(name: &str, fields: Vec<FieldDecl>) -> ModelDecl {
        ModelDecl {
            name: name.into(),
            fields,
        }
    }

    /// `Message` (0) with three fields, `Room` (1) with one field, `Empty` (2) with none.
    fn chat() -> Vec<ModelDecl> {
        vec![
            model(
                "Message",
                vec![
                    field("text", WireTy::Text),
                    field("room", WireTy::Ref),
                    field("tags", WireTy::Set(Box::new(WireTy::Text))),
                ],
            ),
            model("Room", vec![field("title", WireTy::Text)]),
            model("Empty", vec![]),
        ]
    }

    fn schema() -> Result<ServerSchema, SchemaError> {
        ServerSchema::new(chat(), Auth::Off)
    }

    #[test]
    fn ids_are_declaration_positions() -> Result<(), SchemaError> {
        let s = schema()?;
        assert_eq!(s.model_count(), 3);
        assert_eq!(s.model_id("Message"), Some(0));
        assert_eq!(s.model_id("Room"), Some(1));
        assert_eq!(s.model_id("Empty"), Some(2));
        assert_eq!(s.field_id(0, "text"), Some(0));
        assert_eq!(s.field_id(0, "room"), Some(1));
        assert_eq!(s.field_id(0, "tags"), Some(2));
        assert_eq!(s.field_id(1, "title"), Some(0));
        Ok(())
    }

    #[test]
    fn names_and_types_map_back_from_ids() -> Result<(), SchemaError> {
        let s = schema()?;
        assert_eq!(s.model_name(0), Some("Message"));
        assert_eq!(s.model_name(2), Some("Empty"));
        assert_eq!(s.field_name(0, 1), Some("room"));
        assert_eq!(s.field_name(1, 0), Some("title"));
        assert_eq!(s.field_ty(0, 1), Some(&WireTy::Ref));
        assert_eq!(s.field_ty(0, 2), Some(&WireTy::Set(Box::new(WireTy::Text))));
        Ok(())
    }

    #[test]
    fn unknown_names_and_ids_are_none() -> Result<(), SchemaError> {
        let s = schema()?;
        assert_eq!(s.model_id("message"), None, "names are case sensitive");
        assert_eq!(s.model_id(""), None);
        assert_eq!(s.model_name(3), None);
        assert_eq!(s.model_name(ModelId::MAX), None);
        // A field name is looked up in its own model only.
        assert_eq!(s.field_id(1, "text"), None);
        assert_eq!(s.field_id(3, "text"), None);
        assert_eq!(s.field_id(2, "text"), None);
        assert_eq!(s.field_name(1, 1), None);
        assert_eq!(s.field_name(5, 0), None);
        assert_eq!(s.field_ty(0, 3), None);
        assert_eq!(s.field_ty(0, FieldId::MAX), None);
        Ok(())
    }

    #[test]
    fn implicit_fields_are_not_declared_fields() -> Result<(), SchemaError> {
        for auth in [Auth::Off, Auth::On] {
            let s = ServerSchema::new(chat(), auth)?;
            for name in IMPLICIT_FIELDS.into_iter().chain([AUTHOR_FIELD]) {
                assert_eq!(s.field_id(0, name), None, "{name} {auth:?}");
            }
        }
        Ok(())
    }

    /// Without `auth` a declared field `author` is an ordinary field (SPEC 12.6, G8): the
    /// schema loads, the codec maps it by name, and its declared type holds.
    #[test]
    fn declared_author_without_auth_is_an_ordinary_field() -> Result<(), String> {
        let s = ServerSchema::new(
            vec![model(
                "Note",
                vec![field("body", WireTy::Text), field("author", WireTy::Text)],
            )],
            Auth::Off,
        )
        .map_err(|e| e.to_string())?;
        assert_eq!(s.field_id(0, "author"), Some(1));
        assert_eq!(s.field_name(0, 1), Some("author"));
        assert_eq!(s.field_ty(0, 1), Some(&WireTy::Text));
        let sent = op(
            0,
            OpBody::Set {
                field: 1,
                value: Value::Text("Ada".into()),
            },
        )?;
        let text = encode_op(&sent, &s).map_err(|e| format!("{e:?}"))?;
        assert!(text.contains(r#""field":"author""#), "{text}");
        let decoded = decode_op(
            json::parse(text.as_bytes()).map_err(|e| format!("{e:?}"))?,
            &s,
        )
        .map_err(|e| format!("{e:?}"))?;
        assert_eq!(format!("{decoded:?}"), format!("Valid({sent:?})"));
        Ok(())
    }

    /// With `auth` the server stamps `author`, so a declaration of it is a compiler defect.
    #[test]
    fn declared_author_with_auth_is_reserved() {
        let reserved = vec![model("Note", vec![field("author", WireTy::Text)])];
        assert_eq!(
            ServerSchema::new(reserved, Auth::On).err(),
            Some(SchemaError::ReservedField {
                model: "Note".into(),
                field: "author".into()
            })
        );
    }

    #[test]
    fn only_author_depends_on_auth() {
        for name in IMPLICIT_FIELDS {
            assert!(Auth::Off.is_implicit(name), "{name}");
            assert!(Auth::On.is_implicit(name), "{name}");
        }
        assert!(!Auth::Off.is_implicit(AUTHOR_FIELD));
        assert!(Auth::On.is_implicit(AUTHOR_FIELD));
        assert!(!Auth::On.is_implicit("title"));
    }

    #[test]
    fn empty_schema_knows_nothing() -> Result<(), SchemaError> {
        let s = ServerSchema::new(Vec::new(), Auth::Off)?;
        assert_eq!(s.model_count(), 0);
        assert_eq!(s.model_id("Message"), None);
        assert_eq!(s.model_name(0), None);
        Ok(())
    }

    #[test]
    fn rejects_invalid_declarations() {
        let mut twice = chat();
        twice.push(model("Room", vec![]));
        assert_eq!(
            ServerSchema::new(twice, Auth::Off).err(),
            Some(SchemaError::DuplicateModel("Room".into()))
        );

        let field_twice = vec![model(
            "Room",
            vec![field("title", WireTy::Text), field("title", WireTy::Int)],
        )];
        assert_eq!(
            ServerSchema::new(field_twice, Auth::Off).err(),
            Some(SchemaError::DuplicateField {
                model: "Room".into(),
                field: "title".into()
            })
        );

        for auth in [Auth::Off, Auth::On] {
            for name in IMPLICIT_FIELDS {
                let reserved = vec![model("Room", vec![field(name, WireTy::Text)])];
                assert_eq!(
                    ServerSchema::new(reserved, auth).err(),
                    Some(SchemaError::ReservedField {
                        model: "Room".into(),
                        field: name.into()
                    }),
                    "{name} {auth:?}"
                );
            }
        }

        assert_eq!(
            ServerSchema::new(vec![model("", vec![])], Auth::Off).err(),
            Some(SchemaError::EmptyModelName)
        );
        assert_eq!(
            ServerSchema::new(
                vec![model("Room", vec![field("", WireTy::Text)])],
                Auth::Off
            )
            .err(),
            Some(SchemaError::EmptyFieldName {
                model: "Room".into()
            })
        );
    }

    #[test]
    fn same_field_name_in_two_models_is_allowed() -> Result<(), SchemaError> {
        let s = ServerSchema::new(
            vec![
                model("A", vec![field("title", WireTy::Text)]),
                model(
                    "B",
                    vec![field("x", WireTy::Int), field("title", WireTy::Int)],
                ),
            ],
            Auth::Off,
        )?;
        assert_eq!(s.field_id(0, "title"), Some(0));
        assert_eq!(s.field_id(1, "title"), Some(1));
        assert_eq!(s.field_ty(1, 1), Some(&WireTy::Int));
        Ok(())
    }

    fn op(model: ModelId, body: OpBody) -> Result<Op, String> {
        let replica = ReplicaId(0x0123_4567_89ab_cdef);
        let hlc = Hlc::new(0x0001_0203_0405, 7, replica).map_err(|e| format!("{e:?}"))?;
        Ok(Op {
            id: OpId { replica, seq: 1 },
            hlc,
            model,
            row: RowId::new(hlc),
            body,
        })
    }

    /// The codec writes names from this schema and reads them back to the same ids.
    #[test]
    fn codec_round_trip_through_names() -> Result<(), String> {
        let s = schema().map_err(|e| e.to_string())?;
        let sent = op(
            1,
            OpBody::Set {
                field: 0,
                value: Value::Text("Lobby".into()),
            },
        )?;
        let text = encode_op(&sent, &s).map_err(|e| format!("{e:?}"))?;
        assert!(text.contains(r#""model":"Room""#), "{text}");
        assert!(text.contains(r#""field":"title""#), "{text}");
        let decoded = decode_op(
            json::parse(text.as_bytes()).map_err(|e| format!("{e:?}"))?,
            &s,
        )
        .map_err(|e| format!("{e:?}"))?;
        assert_eq!(format!("{decoded:?}"), format!("Valid({sent:?})"));
        Ok(())
    }

    /// A name the schema does not know makes only the op invalid (D132): an old offline op
    /// names a field that a later schema dropped.
    #[test]
    fn codec_unknown_field_makes_op_invalid() -> Result<(), String> {
        let s = schema().map_err(|e| e.to_string())?;
        let sent = op(
            1,
            OpBody::Set {
                field: 0,
                value: Value::Text("Lobby".into()),
            },
        )?;
        let text = encode_op(&sent, &s)
            .map_err(|e| format!("{e:?}"))?
            .replace(r#""field":"title""#, r#""field":"topic""#);
        let decoded = decode_op(
            json::parse(text.as_bytes()).map_err(|e| format!("{e:?}"))?,
            &s,
        )
        .map_err(|e| format!("{e:?}"))?;
        let shown = format!("{decoded:?}");
        assert!(shown.starts_with("Invalid"), "{shown}");
        assert!(shown.contains(r#"UnknownField("topic")"#), "{shown}");
        Ok(())
    }

    /// Writing an id the schema does not have is a defect of the caller, never a frame.
    #[test]
    fn codec_refuses_to_write_unknown_ids() -> Result<(), String> {
        let s = schema().map_err(|e| e.to_string())?;
        let sent = op(3, OpBody::Drop)?;
        assert!(encode_op(&sent, &s).is_err());
        let sent = op(
            1,
            OpBody::Set {
                field: 1,
                value: Value::Text("x".into()),
            },
        )?;
        assert!(encode_op(&sent, &s).is_err());
        Ok(())
    }
}
