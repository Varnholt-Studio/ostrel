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
//! The implicit fields `made` and `changed` (`Time`), and `author` (a reference to the std
//! `User`) in a program with `auth`, are resolved too, because `snapshot` rows and queries name
//! them (`sort made`, `where author == me`). They have the fixed reserved ids
//! [`FIELD_MADE`], [`FIELD_CHANGED`] and [`FIELD_AUTHOR`] of `ostrel_db::api` (D186), so a
//! declared field added later never relabels a stored stamp. Every declared position stays
//! below [`ostrel_db::api::FIRST_IMPLICIT_FIELD`] ([`SchemaError::TooMany`]). The implicit
//! `id` has no field id; it is the row id. No op writes an implicit field: the op decoder
//! refuses their names as unknown fields (D116). Because rows carry the implicit fields under
//! the same names, a declared field may not use one of them ([`SchemaError::ReservedField`]).
//! A program without `auth` has no implicit `author`, so there a declared field `author` is an
//! ordinary field (SPEC 12.6, G8).
//!
//! Server functions are numbered the same way: the position of a function in the list given
//! to [`ServerSchema::new`] is its [`FnId`]. A function carries the wire types of its
//! parameters and of its result, which type the values of `call` and `return` (D135).
//!
//! A reference names its target model ([`WireTy::Ref`], D165). Every target, also one inside
//! an optional or a collection, in a field or in a function signature, must be a model of the
//! same schema ([`SchemaError::UnknownRefTarget`]), so a query hop is never checked against a
//! model that does not exist.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;

use ostrel_db::api::{FIELD_AUTHOR, FIELD_CHANGED, FIELD_MADE, is_implicit_field};
use ostrel_sync::protocol::{FieldId, FnId, ModelId};
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
    /// The `app` has an `auth` line: rows carry the implicit `author`, a reference to `user`,
    /// the std `User` model (G8, D142).
    On { user: ModelId },
}

impl Auth {
    /// Whether `name` is an implicit field of every row of a program with this setting.
    pub fn is_implicit(self, name: &str) -> bool {
        IMPLICIT_FIELDS.contains(&name) || (matches!(self, Auth::On { .. }) && name == AUTHOR_FIELD)
    }

    /// The implicit fields that have a field id, with their reserved ids and wire types
    /// (D186). `id` has none.
    fn stamped_fields(self) -> Vec<Implicit> {
        let mut fields = vec![
            Implicit {
                id: FIELD_MADE,
                name: "made",
                ty: WireTy::Time,
            },
            Implicit {
                id: FIELD_CHANGED,
                name: "changed",
                ty: WireTy::Time,
            },
        ];
        if let Auth::On { user } = self {
            fields.push(Implicit {
                id: FIELD_AUTHOR,
                name: AUTHOR_FIELD,
                ty: WireTy::Ref(user),
            });
        }
        fields
    }
}

/// An implicit field with a reserved id (D186).
#[derive(Clone, Debug)]
struct Implicit {
    id: FieldId,
    name: &'static str,
    ty: WireTy,
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

/// One server function as the compiler declares it: its name and the wire types of its
/// parameters (in declaration order) and of its result. A model parameter is a
/// [`WireTy::Ref`] to that model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<WireTy>,
    pub result: WireTy,
}

/// Why a list of models and functions is not a schema. Every kind is a defect of the compiler output,
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
    /// A function has an empty name.
    EmptyFunctionName,
    /// Two functions have this name.
    DuplicateFunction(String),
    /// A reference in this place names a model the schema does not have. The place is
    /// `Model.field`, or `fn name` for a parameter or the result of a function.
    UnknownRefTarget { place: String, target: ModelId },
    /// More models or functions than an id can number, or a declared field whose position
    /// would reach the ids reserved for implicit fields (D186).
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
            SchemaError::EmptyFunctionName => write!(f, "a function has an empty name"),
            SchemaError::DuplicateFunction(name) => {
                write!(f, "function `{name}` is declared twice")
            }
            SchemaError::UnknownRefTarget { place, target } => {
                write!(f, "a reference in {place} names the unknown model {target}")
            }
            SchemaError::TooMany => {
                write!(f, "more models, fields or functions than an id can number")
            }
        }
    }
}

impl std::error::Error for SchemaError {}

/// The schema of one compiled program for the sync codec.
#[derive(Clone, Debug)]
pub struct ServerSchema {
    models: Vec<ModelEntry>,
    model_ids: HashMap<String, ModelId>,
    functions: Vec<FnDecl>,
    function_ids: HashMap<String, FnId>,
    /// The implicit fields every model of this program has, `author` only with `auth`.
    implicit: Vec<Implicit>,
}

#[derive(Clone, Debug)]
struct ModelEntry {
    name: String,
    fields: Vec<FieldDecl>,
    field_ids: HashMap<String, FieldId>,
}

impl ServerSchema {
    /// Builds the schema from the models and the server functions, each in declaration order.
    /// `auth` tells whether the program declares `auth`, which makes `author` an implicit
    /// field.
    pub fn new(
        models: Vec<ModelDecl>,
        functions: Vec<FnDecl>,
        auth: Auth,
    ) -> Result<Self, SchemaError> {
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
        let mut function_ids = HashMap::with_capacity(functions.len());
        for (at, function) in functions.iter().enumerate() {
            let id = FnId::try_from(at).map_err(|_| SchemaError::TooMany)?;
            if function.name.is_empty() {
                return Err(SchemaError::EmptyFunctionName);
            }
            if function_ids.insert(function.name.clone(), id).is_some() {
                return Err(SchemaError::DuplicateFunction(function.name.clone()));
            }
        }
        let schema = Self {
            models: entries,
            model_ids,
            functions,
            function_ids,
            implicit: auth.stamped_fields(),
        };
        schema.check_ref_targets()?;
        Ok(schema)
    }

    /// The number of server functions.
    pub fn function_count(&self) -> usize {
        self.functions.len()
    }

    fn function(&self, function: FnId) -> Option<&FnDecl> {
        self.functions.get(usize::try_from(function).ok()?)
    }

    /// Every [`WireTy::Ref`] in a field type or a function signature names a model of this
    /// schema.
    fn check_ref_targets(&self) -> Result<(), SchemaError> {
        let count = self.models.len();
        for model in &self.models {
            for field in &model.fields {
                check_ref_target(&field.ty, count).map_err(|target| {
                    SchemaError::UnknownRefTarget {
                        place: format!("{}.{}", model.name, field.name),
                        target,
                    }
                })?;
            }
        }
        for field in &self.implicit {
            check_ref_target(&field.ty, count).map_err(|target| SchemaError::UnknownRefTarget {
                place: format!("implicit {}", field.name),
                target,
            })?;
        }
        for function in &self.functions {
            for ty in function.params.iter().chain([&function.result]) {
                check_ref_target(ty, count).map_err(|target| SchemaError::UnknownRefTarget {
                    place: format!("fn {}", function.name),
                    target,
                })?;
            }
        }
        Ok(())
    }

    /// The number of models.
    pub fn model_count(&self) -> usize {
        self.models.len()
    }

    fn model(&self, model: ModelId) -> Option<&ModelEntry> {
        self.models.get(usize::try_from(model).ok()?)
    }

    /// The name and wire type of a declared or implicit field of an existing model.
    fn field(&self, model: ModelId, field: FieldId) -> Option<(&str, &WireTy)> {
        let entry = self.model(model)?;
        if is_implicit_field(field) {
            let implicit = self.implicit.iter().find(|f| f.id == field)?;
            return Some((implicit.name, &implicit.ty));
        }
        let declared = entry.fields.get(usize::try_from(field).ok()?)?;
        Some((declared.name.as_str(), &declared.ty))
    }
}

/// The field id of the declared field at position `at`: the position itself, which must stay
/// below the ids reserved for implicit fields (D186).
fn declared_field_id(at: usize) -> Result<FieldId, SchemaError> {
    FieldId::try_from(at)
        .ok()
        .filter(|id| !is_implicit_field(*id))
        .ok_or(SchemaError::TooMany)
}

/// The first reference target in `ty` that is not below `models`, the number of models.
fn check_ref_target(ty: &WireTy, models: usize) -> Result<(), ModelId> {
    match ty {
        WireTy::Ref(target) => match usize::try_from(*target) {
            Ok(at) if at < models => Ok(()),
            _ => Err(*target),
        },
        WireTy::Optional(inner) | WireTy::Set(inner) | WireTy::List(inner) => {
            check_ref_target(inner, models)
        }
        WireTy::Map(key, value) => {
            check_ref_target(key, models)?;
            check_ref_target(value, models)
        }
        WireTy::Bool
        | WireTy::Int
        | WireTy::Float
        | WireTy::Text
        | WireTy::Time
        | WireTy::Bytes
        | WireTy::Enum(_)
        | WireTy::Rank
        | WireTy::Serial => Ok(()),
    }
}

impl ModelEntry {
    fn new(model: ModelDecl, auth: Auth) -> Result<Self, SchemaError> {
        let mut field_ids = HashMap::with_capacity(model.fields.len());
        for (at, field) in model.fields.iter().enumerate() {
            let id = declared_field_id(at)?;
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

    /// Every readable name: a declared field, or `made`, `changed` and (with `auth`) `author`
    /// under their reserved ids (D186). The op decoder refuses the implicit ones.
    fn field_id(&self, model: ModelId, name: &str) -> Option<FieldId> {
        let entry = self.model(model)?;
        if let Some(id) = entry.field_ids.get(name) {
            return Some(*id);
        }
        self.implicit.iter().find(|f| f.name == name).map(|f| f.id)
    }

    fn field_name(&self, model: ModelId, field: FieldId) -> Option<&str> {
        Some(self.field(model, field)?.0)
    }

    fn field_ty(&self, model: ModelId, field: FieldId) -> Option<&WireTy> {
        Some(self.field(model, field)?.1)
    }

    fn function_id(&self, name: &str) -> Option<FnId> {
        self.function_ids.get(name).copied()
    }

    fn function_name(&self, function: FnId) -> Option<&str> {
        Some(self.function(function)?.name.as_str())
    }

    fn function_params(&self, function: FnId) -> Option<&[WireTy]> {
        Some(self.function(function)?.params.as_slice())
    }

    fn function_result(&self, function: FnId) -> Option<&WireTy> {
        Some(&self.function(function)?.result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ostrel_db::api::FIRST_IMPLICIT_FIELD;

    use ostrel_core::ids::{Hlc, OpId, ReplicaId, RowId};
    use ostrel_core::value::Value;
    use ostrel_sync::protocol::{CallError, CallId, ClientMsg, Op, OpBody, ServerMsg, SubId};
    use ostrel_sync::wire::{
        QueryFault, Requests, WireError, decode_client, decode_op, decode_server, encode_client,
        encode_op, encode_server, json,
    };

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
                    field("room", WireTy::Ref(1)),
                    field("tags", WireTy::Set(Box::new(WireTy::Text))),
                ],
            ),
            model("Room", vec![field("title", WireTy::Text)]),
            model("Empty", vec![]),
        ]
    }

    fn function(name: &str, params: Vec<WireTy>, result: WireTy) -> FnDecl {
        FnDecl {
            name: name.into(),
            params,
            result,
        }
    }

    /// `rename` (0): a `Room` and a text to `Bool`; `count` (1): nothing to `Int`; `rooms` (2):
    /// an optional `Message` to a list of `Room`.
    fn chat_functions() -> Vec<FnDecl> {
        vec![
            function("rename", vec![WireTy::Ref(1), WireTy::Text], WireTy::Bool),
            function("count", vec![], WireTy::Int),
            function(
                "rooms",
                vec![WireTy::Optional(Box::new(WireTy::Ref(0)))],
                WireTy::List(Box::new(WireTy::Ref(1))),
            ),
        ]
    }

    fn schema() -> Result<ServerSchema, SchemaError> {
        ServerSchema::new(chat(), chat_functions(), Auth::Off)
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
        assert_eq!(s.field_ty(0, 1), Some(&WireTy::Ref(1)));
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

    /// `Room` (1) stands in for the std `User` of a program with `auth`.
    const WITH_AUTH: Auth = Auth::On { user: 1 };

    /// `made` and `changed` resolve to their reserved ids in every model, `author` only with
    /// `auth`, and `id` never: it is the row id (D186).
    #[test]
    fn implicit_fields_resolve_to_reserved_ids() -> Result<(), SchemaError> {
        for auth in [Auth::Off, WITH_AUTH] {
            let s = ServerSchema::new(chat(), Vec::new(), auth)?;
            for model in 0..3 {
                assert_eq!(s.field_id(model, "made"), Some(FIELD_MADE), "{auth:?}");
                assert_eq!(
                    s.field_id(model, "changed"),
                    Some(FIELD_CHANGED),
                    "{auth:?}"
                );
                assert_eq!(s.field_id(model, "id"), None, "{auth:?}");
                assert_eq!(s.field_name(model, FIELD_MADE), Some("made"));
                assert_eq!(s.field_name(model, FIELD_CHANGED), Some("changed"));
                assert_eq!(s.field_ty(model, FIELD_MADE), Some(&WireTy::Time));
                assert_eq!(s.field_ty(model, FIELD_CHANGED), Some(&WireTy::Time));
            }
            // Implicit fields belong to existing models only.
            assert_eq!(s.field_id(3, "made"), None);
            assert_eq!(s.field_name(3, FIELD_MADE), None);
            assert_eq!(s.field_ty(3, FIELD_CHANGED), None);
            // The rest of the reserved range is no field.
            for unused in [FIELD_AUTHOR + 1, FieldId::MAX] {
                assert_eq!(s.field_name(0, unused), None);
                assert_eq!(s.field_ty(0, unused), None);
            }
        }
        let off = ServerSchema::new(chat(), Vec::new(), Auth::Off)?;
        assert_eq!(off.field_id(0, "author"), None);
        assert_eq!(off.field_name(0, FIELD_AUTHOR), None);
        assert_eq!(off.field_ty(0, FIELD_AUTHOR), None);
        let on = ServerSchema::new(chat(), Vec::new(), WITH_AUTH)?;
        assert_eq!(on.field_id(0, "author"), Some(FIELD_AUTHOR));
        assert_eq!(on.field_name(0, FIELD_AUTHOR), Some("author"));
        assert_eq!(on.field_ty(0, FIELD_AUTHOR), Some(&WireTy::Ref(1)));
        // Declared positions are unchanged by the implicit fields.
        assert_eq!(on.field_id(0, "text"), Some(0));
        assert_eq!(on.field_id(0, "tags"), Some(2));
        Ok(())
    }

    /// The reserved ids are the ones D186 fixes in `ostrel_db::api`.
    #[test]
    fn reserved_ids_are_fixed() {
        assert_eq!(FIRST_IMPLICIT_FIELD, 0xFFFF_FF00);
        assert_eq!(FIELD_MADE, 0xFFFF_FF00);
        assert_eq!(FIELD_CHANGED, 0xFFFF_FF01);
        assert_eq!(FIELD_AUTHOR, 0xFFFF_FF02);
        assert!(!is_implicit_field(0));
        assert!(!is_implicit_field(FIRST_IMPLICIT_FIELD - 1));
        for id in [FIELD_MADE, FIELD_CHANGED, FIELD_AUTHOR, FieldId::MAX] {
            assert!(is_implicit_field(id), "{id:#x}");
        }
    }

    /// A declared position never reaches the reserved range.
    #[test]
    fn declared_positions_stay_below_the_reserved_ids() {
        assert_eq!(declared_field_id(0), Ok(0));
        let last = FIRST_IMPLICIT_FIELD - 1;
        assert_eq!(declared_field_id(last as usize), Ok(last));
        assert_eq!(
            declared_field_id(FIRST_IMPLICIT_FIELD as usize),
            Err(SchemaError::TooMany)
        );
        assert_eq!(declared_field_id(usize::MAX), Err(SchemaError::TooMany));
    }

    /// The target of the implicit `author` must be a model of the schema.
    #[test]
    fn author_target_must_be_a_model() {
        assert_eq!(
            ServerSchema::new(chat(), Vec::new(), Auth::On { user: 3 }).err(),
            Some(SchemaError::UnknownRefTarget {
                place: "implicit author".into(),
                target: 3
            })
        );
    }

    /// An op that writes `made` (or another implicit field) is `UnknownField`, the same fault
    /// as an unknown name (D186, D116), in every body kind that names a field.
    #[test]
    fn an_op_that_writes_made_is_unknown_field() -> Result<(), String> {
        let s = ServerSchema::new(chat(), Vec::new(), WITH_AUTH).map_err(|e| e.to_string())?;
        let sent = op(
            1,
            OpBody::Set {
                field: 0,
                value: Value::Text("Lobby".into()),
            },
        )?;
        let text = encode_op(&sent, &s).map_err(|e| format!("{e:?}"))?;
        let row = room_row()?.to_hex();
        for name in ["made", "changed", "author"] {
            let value = if name == "author" {
                format!("\"{row}\"")
            } else {
                "5".into()
            };
            let set = text.replace(
                r#"{"set":{"field":"title","value":"Lobby"}}"#,
                &format!(r#"{{"set":{{"field":"{name}","value":{value}}}}}"#),
            );
            let make = text.replace(
                r#"{"set":{"field":"title","value":"Lobby"}}"#,
                &format!(r#"{{"make":{{"{name}":{value},"title":"Lobby"}}}}"#),
            );
            for frame in [set, make] {
                assert_ne!(frame, text);
                let decoded = decode_op(
                    json::parse(frame.as_bytes()).map_err(|e| format!("{e:?}"))?,
                    &s,
                )
                .map_err(|e| format!("{e:?}"))?;
                let shown = format!("{decoded:?}");
                assert!(shown.starts_with("Invalid"), "{frame}: {shown}");
                assert!(
                    shown.contains(&format!(r#"UnknownField("{name}")"#)),
                    "{frame}: {shown}"
                );
            }
        }
        // Writing such an op is a defect of the caller.
        let stamp = op(
            1,
            OpBody::Set {
                field: FIELD_MADE,
                value: Value::time(5).map_err(|e| format!("{e:?}"))?,
            },
        )?;
        assert!(encode_op(&stamp, &s).is_err());
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
            Vec::new(),
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
            ServerSchema::new(reserved, Vec::new(), Auth::On { user: 0 }).err(),
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
            assert!(WITH_AUTH.is_implicit(name), "{name}");
        }
        assert!(!Auth::Off.is_implicit(AUTHOR_FIELD));
        assert!(WITH_AUTH.is_implicit(AUTHOR_FIELD));
        assert!(!WITH_AUTH.is_implicit("title"));
    }

    #[test]
    fn empty_schema_knows_nothing() -> Result<(), SchemaError> {
        let s = ServerSchema::new(Vec::new(), Vec::new(), Auth::Off)?;
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
            ServerSchema::new(twice, Vec::new(), Auth::Off).err(),
            Some(SchemaError::DuplicateModel("Room".into()))
        );

        let field_twice = vec![model(
            "Room",
            vec![field("title", WireTy::Text), field("title", WireTy::Int)],
        )];
        assert_eq!(
            ServerSchema::new(field_twice, Vec::new(), Auth::Off).err(),
            Some(SchemaError::DuplicateField {
                model: "Room".into(),
                field: "title".into()
            })
        );

        for auth in [Auth::Off, WITH_AUTH] {
            for name in IMPLICIT_FIELDS {
                let reserved = vec![model("Room", vec![field(name, WireTy::Text)])];
                assert_eq!(
                    ServerSchema::new(reserved, Vec::new(), auth).err(),
                    Some(SchemaError::ReservedField {
                        model: "Room".into(),
                        field: name.into()
                    }),
                    "{name} {auth:?}"
                );
            }
        }

        assert_eq!(
            ServerSchema::new(vec![model("", vec![])], Vec::new(), Auth::Off).err(),
            Some(SchemaError::EmptyModelName)
        );
        assert_eq!(
            ServerSchema::new(
                vec![model("Room", vec![field("", WireTy::Text)])],
                Vec::new(),
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
            Vec::new(),
            Auth::Off,
        )?;
        assert_eq!(s.field_id(0, "title"), Some(0));
        assert_eq!(s.field_id(1, "title"), Some(1));
        assert_eq!(s.field_ty(1, 1), Some(&WireTy::Int));
        Ok(())
    }

    #[test]
    fn functions_are_numbered_by_declaration_position() -> Result<(), SchemaError> {
        let s = schema()?;
        assert_eq!(s.function_count(), 3);
        assert_eq!(s.function_id("rename"), Some(0));
        assert_eq!(s.function_id("count"), Some(1));
        assert_eq!(s.function_id("rooms"), Some(2));
        assert_eq!(s.function_name(0), Some("rename"));
        assert_eq!(s.function_name(2), Some("rooms"));
        assert_eq!(s.function_id("Rename"), None, "names are case sensitive");
        assert_eq!(s.function_id("Room"), None, "a model is no function");
        assert_eq!(s.function_id(""), None);
        assert_eq!(s.function_name(3), None);
        assert_eq!(s.function_name(FnId::MAX), None);
        Ok(())
    }

    #[test]
    fn function_params_are_the_declared_types_in_order() -> Result<(), SchemaError> {
        let s = schema()?;
        assert_eq!(
            s.function_params(0),
            Some(&[WireTy::Ref(1), WireTy::Text][..])
        );
        assert_eq!(s.function_params(1), Some(&[][..]));
        assert_eq!(
            s.function_params(2),
            Some(&[WireTy::Optional(Box::new(WireTy::Ref(0)))][..])
        );
        assert_eq!(s.function_params(3), None);
        Ok(())
    }

    #[test]
    fn function_result_is_the_declared_type() -> Result<(), SchemaError> {
        let s = schema()?;
        assert_eq!(s.function_result(0), Some(&WireTy::Bool));
        assert_eq!(s.function_result(1), Some(&WireTy::Int));
        assert_eq!(
            s.function_result(2),
            Some(&WireTy::List(Box::new(WireTy::Ref(1))))
        );
        assert_eq!(s.function_result(3), None);
        assert_eq!(s.function_result(FnId::MAX), None);
        Ok(())
    }

    #[test]
    fn a_schema_without_functions_knows_none() -> Result<(), SchemaError> {
        let s = ServerSchema::new(chat(), Vec::new(), Auth::Off)?;
        assert_eq!(s.function_count(), 0);
        assert_eq!(s.function_id("rename"), None);
        assert_eq!(s.function_name(0), None);
        assert_eq!(s.function_params(0), None);
        assert_eq!(s.function_result(0), None);
        Ok(())
    }

    #[test]
    fn rejects_invalid_functions() {
        let mut twice = chat_functions();
        twice.push(function("count", vec![WireTy::Int], WireTy::Int));
        assert_eq!(
            ServerSchema::new(chat(), twice, Auth::Off).err(),
            Some(SchemaError::DuplicateFunction("count".into()))
        );
        assert_eq!(
            ServerSchema::new(chat(), vec![function("", vec![], WireTy::Int)], Auth::Off).err(),
            Some(SchemaError::EmptyFunctionName)
        );
    }

    /// Each reference keeps its own target model (D165).
    #[test]
    fn ref_fields_name_their_target_model() -> Result<(), SchemaError> {
        let s = ServerSchema::new(
            vec![
                model("Person", vec![field("home", WireTy::Ref(1))]),
                model(
                    "Room",
                    vec![
                        field("owner", WireTy::Ref(0)),
                        field("parent", WireTy::Optional(Box::new(WireTy::Ref(1)))),
                    ],
                ),
            ],
            Vec::new(),
            Auth::Off,
        )?;
        assert_eq!(s.field_ty(0, 0), Some(&WireTy::Ref(1)));
        assert_eq!(s.field_ty(1, 0), Some(&WireTy::Ref(0)));
        assert_eq!(
            s.field_ty(1, 1),
            Some(&WireTy::Optional(Box::new(WireTy::Ref(1))))
        );
        Ok(())
    }

    /// A reference to a model the schema does not have is refused wherever it stands.
    #[test]
    fn rejects_unknown_ref_targets() -> Result<(), String> {
        let nested = [
            WireTy::Ref(3),
            WireTy::Optional(Box::new(WireTy::Ref(3))),
            WireTy::Set(Box::new(WireTy::Ref(3))),
            WireTy::List(Box::new(WireTy::Ref(3))),
            WireTy::Map(Box::new(WireTy::Ref(3)), Box::new(WireTy::Int)),
            WireTy::Map(Box::new(WireTy::Text), Box::new(WireTy::Ref(3))),
            WireTy::Ref(ModelId::MAX),
        ];
        for ty in nested {
            let target = if ty == WireTy::Ref(ModelId::MAX) {
                ModelId::MAX
            } else {
                3
            };
            let mut models = chat();
            models
                .get_mut(1)
                .ok_or("chat has a model 1")?
                .fields
                .push(field("bad", ty.clone()));
            assert_eq!(
                ServerSchema::new(models, Vec::new(), Auth::Off).err(),
                Some(SchemaError::UnknownRefTarget {
                    place: "Room.bad".into(),
                    target
                }),
                "{ty:?}"
            );
            for (params, result) in [(vec![ty.clone()], WireTy::Int), (vec![], ty.clone())] {
                let functions = vec![function("bad", params, result)];
                assert_eq!(
                    ServerSchema::new(chat(), functions, Auth::Off).err(),
                    Some(SchemaError::UnknownRefTarget {
                        place: "fn bad".into(),
                        target
                    }),
                    "{ty:?}"
                );
            }
        }
        // The last model is a valid target, one past it is not.
        let mut models = chat();
        models
            .get_mut(0)
            .ok_or("chat has a model 0")?
            .fields
            .push(field("last", WireTy::Ref(2)));
        assert!(ServerSchema::new(models, Vec::new(), Auth::Off).is_ok());
        Ok(())
    }

    fn room_row() -> Result<RowId, String> {
        let hlc = Hlc::new(9, 0, ReplicaId(1)).map_err(|e| format!("{e:?}"))?;
        Ok(RowId::new(hlc))
    }

    /// The codec types the arguments of a `call` by the parameters of this schema.
    #[test]
    fn codec_reads_call_arguments_by_function_params() -> Result<(), String> {
        let s = schema().map_err(|e| e.to_string())?;
        let room = room_row()?;
        let sent = ClientMsg::Call {
            call: 7,
            function: 0,
            args: vec![Value::Ref(room), Value::Text("Lobby".into())],
        };
        let text = encode_client(&sent, &s).map_err(|e| format!("{e:?}"))?;
        assert!(text.contains(r#""function":"rename""#), "{text}");
        let read = decode_client(text.as_bytes(), &s).map_err(|e| format!("{e:?}"))?;
        assert_eq!(read, sent);

        let call = |function: &str, args: String| {
            format!(r#"{{"args":{args},"call":9,"function":"{function}","t":"call","v":0}}"#)
        };
        let refused = |frame: String, error: CallError| {
            assert_eq!(
                decode_client(frame.as_bytes(), &s),
                Err(WireError::InvalidCall { call: 9, error }),
                "{frame}"
            );
        };
        let room = format!("\"{}\"", room.to_hex());
        // Second parameter is `Text`, not `Int`.
        refused(call("rename", format!("[{room},1]")), CallError::Invalid);
        // Two parameters, not one.
        refused(call("rename", format!("[{room}]")), CallError::Invalid);
        refused(call("count", format!("[{room}]")), CallError::Invalid);
        refused(call("gone", "[]".into()), CallError::NotFound);
        Ok(())
    }

    /// One open call, of `function`.
    struct OneCall(FnId);

    impl Requests for OneCall {
        fn subscription_model(&self, _: SubId) -> Option<ModelId> {
            None
        }

        fn call_function(&self, call: CallId) -> Option<FnId> {
            (call == 7).then_some(self.0)
        }
    }

    /// The codec types the value of a `return` by the result of the function of its call.
    #[test]
    fn codec_types_return_values_by_function_result() -> Result<(), String> {
        let s = schema().map_err(|e| e.to_string())?;
        let rooms = Value::List(vec![Value::Ref(room_row()?)]);
        let sent = ServerMsg::Return {
            call: 7,
            result: Ok(rooms),
        };
        let text = encode_server(&sent, &s, &OneCall(2)).map_err(|e| format!("{e:?}"))?;
        let read = decode_server(text.as_bytes(), &s, &OneCall(2)).map_err(|e| format!("{e:?}"))?;
        assert_eq!(read, sent);

        // `rename` returns `Bool`: the server does not write an `Int` for it, and a client
        // that waits for `count` (an `Int`) does not read the `Bool` of `rename`.
        let int = ServerMsg::Return {
            call: 7,
            result: Ok(Value::int(1).map_err(|e| format!("{e:?}"))?),
        };
        assert!(encode_server(&int, &s, &OneCall(0)).is_err());
        let flag = ServerMsg::Return {
            call: 7,
            result: Ok(Value::Bool(true)),
        };
        let text = encode_server(&flag, &s, &OneCall(0)).map_err(|e| format!("{e:?}"))?;
        assert!(
            decode_server(text.as_bytes(), &s, &OneCall(1)).is_err(),
            "{text}"
        );
        Ok(())
    }

    /// A query hop is accepted only towards the target of its reference (D165).
    #[test]
    fn codec_checks_query_hops_against_ref_targets() -> Result<(), String> {
        let s = schema().map_err(|e| e.to_string())?;
        let hop = |model: &str| {
            format!(
                r#"{{"query":{{"after":null,"filter":{{"cmp":["eq",{{"field":{{"field":"title","hops":[["room","{model}"]]}}}},{{"const":"x"}}]}},"limit":null,"model":"Message","order":[]}},"sub":4,"t":"subscribe","v":0}}"#
            )
        };
        let read = decode_client(hop("Room").as_bytes(), &s).map_err(|e| format!("{e:?}"))?;
        assert!(
            matches!(read, ClientMsg::Subscribe { sub: 4, .. }),
            "{read:?}"
        );
        assert_eq!(
            decode_client(hop("Message").as_bytes(), &s),
            Err(WireError::InvalidQuery {
                sub: 4,
                fault: QueryFault::WrongField("room".into())
            })
        );
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
