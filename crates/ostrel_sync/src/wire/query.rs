//! Queries on the wire: the query of a `subscribe` (ARCHITECTURE 5.4 "Query", D132).
//!
//! A query is `{"model", "filter", "order", "limit", "after"}`:
//!
//! * `model`: the name of the queried model;
//! * `filter`: an expression or `null`;
//! * `order`: `[[field, "asc" or "desc"], ...]`, fields of the queried model;
//! * `limit`: a count or `null`;
//! * `after`: `{"values": [...], "id": row id}`, one value per sort key, or `null`.
//!
//! An expression is an object with exactly one key:
//!
//! | Key | Content |
//! |---|---|
//! | `const` | a value |
//! | `field` | a path `{"hops": [[field, model], ...], "field": name}` |
//! | `row_id` | the hops of a path |
//! | `cmp` | `[op, a, b]`, `op` one of `eq`, `ne`, `lt`, `le`, `gt`, `ge` |
//! | `and`, `or` | `[e, ...]` |
//! | `not` | `e` |
//! | `in_set` | `[elem, path]` |
//! | `in_map` | `[key, path]` |
//! | `map_get` | `[path, key]` |
//! | `coalesce` | `[e, {"const": value}]` |
//!
//! A hop names a reference field of the current model and the model it points to. A constant
//! has no type of its own on the wire; it takes the type of its place: the other side of a
//! `cmp`, the element type of the `Set` of an `in_set`, the key type of the `Map` of an
//! `in_map` or `map_get`, the type of the first operand of a `coalesce`, and `Bool` where a
//! condition stands. A constant may always be `null`. Two constants in one `cmp` have no type
//! and make the query invalid, and so does a constant whose place is a collection (filters hold
//! no collection constants, [`Query::check`]).
//!
//! The codec counts expression nodes as [`Query::check`] does and stops past
//! [`LIMITS`]`.filter_nodes`, and it refuses a path with more than [`MAX_HOPS`] hops. Readable
//! fields and the `see` rule of each hop are checked by the server after the codec (D101).
//!
//! Every fault inside the query is a [`QueryFault`]; the frame reader turns it into
//! [`WireError::InvalidQuery`] with the subscription id, so the connection stays open (D106).

use std::fmt;

use ostrel_core::canon::{encode, encode_str};

use super::frame::{Fields, array, hex, row_id, string, uint32};
use super::json::Json;
use super::value::{WireTy, read_value};
use super::{WireError, WireSchema, clip};
use crate::protocol::{FieldId, LIMITS, ModelId, Query, Value, ValueFault};
use ostrel_db::api::{CmpOp, Cursor, Dir, Expr, Hop, MAX_HOPS, Path};

/// Why the query of a `subscribe` is invalid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryFault {
    /// The query or one of its parts does not have the form of the module documentation; the
    /// form error found there.
    Form(Box<WireError>),
    /// The schema has no model of this name (cut to [`super::MAX_ECHO_BYTES`]).
    UnknownModel(String),
    /// The model has no field of this name (cut to [`super::MAX_ECHO_BYTES`]).
    UnknownField(String),
    /// This field stands where its type does not fit: a hop through a field that is not a
    /// reference, an `in_set` on a field that is not a `Set`, or an `in_map` or `map_get` on
    /// one that is not a `Map` (cut to [`super::MAX_ECHO_BYTES`]).
    WrongField(String),
    /// A constant does not fit the type of its place.
    Value(ValueFault),
    /// A constant whose place has no type: both sides of a `cmp`, or the first operand of a
    /// `coalesce`.
    UntypedConstant,
    /// A constant whose place is a `Set`, `Map` or `List`.
    CollectionConstant,
    /// More than [`LIMITS`]`.filter_nodes` expression nodes.
    TooManyNodes,
    /// A path with more than [`MAX_HOPS`] hops.
    TooManyHops,
    /// `after` does not have one value per sort key.
    CursorLength,
}

impl From<WireError> for QueryFault {
    fn from(error: WireError) -> Self {
        QueryFault::Form(Box::new(error))
    }
}

impl fmt::Display for QueryFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryFault::Form(error) => write!(f, "{error}"),
            QueryFault::UnknownModel(name) => write!(f, "unknown model {name:?}"),
            QueryFault::UnknownField(name) => write!(f, "unknown field {name:?}"),
            QueryFault::WrongField(name) => {
                write!(f, "field {name:?} does not fit its place")
            }
            QueryFault::Value(fault) => write!(f, "constant: {fault}"),
            QueryFault::UntypedConstant => f.write_str("constant without a typed place"),
            QueryFault::CollectionConstant => f.write_str("collection constant in filter"),
            QueryFault::TooManyNodes => f.write_str("filter has too many nodes"),
            QueryFault::TooManyHops => f.write_str("path has too many hops"),
            QueryFault::CursorLength => f.write_str("cursor values do not match the sort keys"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Reading

/// Reads the query of a `subscribe`.
pub(super) fn decode_query(json: Json, schema: &dyn WireSchema) -> Result<Query, QueryFault> {
    let Json::Obj(entries) = json else {
        return Err(WireError::WrongType("query").into());
    };
    let mut f = Fields { entries };
    let model = f.req("model", string)?;
    let filter = f.opt("filter", any)?;
    let order = f.req("order", any)?;
    let limit = f.opt("limit", uint32)?;
    let after = f.opt("after", any)?;
    f.finish()?;

    let mut reader = Reader { schema, nodes: 0 };
    let model = reader.model(&model)?;
    let (order, order_types) = reader.order(order, model)?;
    let filter = filter
        .map(|json| reader.operand(json, model, Some(&WireTy::Bool)))
        .transpose()?
        .map(|(expr, _)| expr);
    let after = after
        .map(|json| reader.cursor(json, &order_types))
        .transpose()?;
    Ok(Query {
        model,
        filter,
        order,
        limit,
        after,
    })
}

/// An expression with the wire type of its value, `None` for a constant.
type Typed = (Expr, Option<WireTy>);

/// The sort keys of a query with the wire type of each sort field.
type Order = (Vec<(FieldId, Dir)>, Vec<WireTy>);

struct Reader<'s> {
    schema: &'s dyn WireSchema,
    /// Expression nodes read so far.
    nodes: usize,
}

impl Reader<'_> {
    fn model(&self, name: &str) -> Result<ModelId, QueryFault> {
        self.schema
            .model_id(name)
            .ok_or_else(|| QueryFault::UnknownModel(clip(name)))
    }

    fn field(&self, model: ModelId, name: &str) -> Result<(FieldId, WireTy), QueryFault> {
        self.schema
            .field_id(model, name)
            .and_then(|id| Some((id, self.schema.field_ty(model, id)?.clone())))
            .ok_or_else(|| QueryFault::UnknownField(clip(name)))
    }

    /// One more expression node, as [`Query::check`] counts them.
    fn count(&mut self) -> Result<(), QueryFault> {
        self.nodes += 1;
        if self.nodes > LIMITS.filter_nodes as usize {
            return Err(QueryFault::TooManyNodes);
        }
        Ok(())
    }

    /// `[[field, "asc" or "desc"], ...]`, with the type of each sort field for the cursor.
    fn order(&self, json: Json, model: ModelId) -> Result<Order, QueryFault> {
        let mut order = Vec::new();
        let mut types = Vec::new();
        for key in items(json, "order")? {
            let [field, dir] = pair(key, "order")?;
            let (field, ty) = self.field(model, &string("order", field)?)?;
            let dir = match string("order", dir)?.as_str() {
                "asc" => Dir::Asc,
                "desc" => Dir::Desc,
                _ => return Err(WireError::WrongType("order").into()),
            };
            order.push((field, dir));
            types.push(ty);
        }
        Ok((order, types))
    }

    /// `{"values": [...], "id": row id}`, each value typed by its sort field.
    fn cursor(&self, json: Json, types: &[WireTy]) -> Result<Cursor, QueryFault> {
        let mut f = fields(json, "after")?;
        let values = f.req("values", any)?;
        let id = f.req("id", row_id)?;
        f.finish()?;
        let values = items(values, "values")?;
        if values.len() != types.len() {
            return Err(QueryFault::CursorLength);
        }
        let values = values
            .into_iter()
            .zip(types)
            .map(|(json, ty)| read_value(&optional(ty), json).map_err(QueryFault::Value))
            .collect::<Result<_, _>>()?;
        Ok(Cursor { values, id })
    }

    /// An expression or a constant in a place of type `ty`.
    fn operand(
        &mut self,
        json: Json,
        model: ModelId,
        ty: Option<&WireTy>,
    ) -> Result<Typed, QueryFault> {
        if !is_const(&json) {
            return self.expr(json, model);
        }
        self.count()?;
        Ok((Expr::Const(constant(json, ty)?), None))
    }

    /// An expression that is not a constant. A constant reaching this place has no type.
    fn expr(&mut self, json: Json, model: ModelId) -> Result<Typed, QueryFault> {
        self.count()?;
        let (kind, body) = single(json, "filter")?;
        let bool_ty = Some(WireTy::Bool);
        Ok(match kind.as_str() {
            "const" => return Err(QueryFault::UntypedConstant),
            "field" => {
                let (path, ty) = self.path(body, model)?;
                (Expr::Field(path), Some(ty))
            }
            "row_id" => {
                let (hops, _) = self.hops(body, model)?;
                (Expr::RowId(hops), Some(WireTy::Ref))
            }
            "cmp" => {
                let [op, a, b] = triple(body, "cmp")?;
                let op = cmp_op(op)?;
                // The constant side, if any, takes the type of the other side.
                let (a, b) = if is_const(&a) {
                    let (b, ty) = self.operand(b, model, None)?;
                    let (a, _) = self.operand(a, model, ty.as_ref())?;
                    (a, b)
                } else {
                    let (a, ty) = self.expr(a, model)?;
                    let (b, _) = self.operand(b, model, ty.as_ref())?;
                    (a, b)
                };
                (Expr::Cmp(op, Box::new(a), Box::new(b)), bool_ty)
            }
            "and" | "or" => {
                let operands = items(body, "filter")?
                    .into_iter()
                    .map(|e| Ok(self.operand(e, model, Some(&WireTy::Bool))?.0))
                    .collect::<Result<Vec<_>, QueryFault>>()?;
                let expr = if kind == "and" {
                    Expr::And(operands)
                } else {
                    Expr::Or(operands)
                };
                (expr, bool_ty)
            }
            "not" => {
                let (e, _) = self.operand(body, model, Some(&WireTy::Bool))?;
                (Expr::Not(Box::new(e)), bool_ty)
            }
            "in_set" => {
                let [elem, set] = pair(body, "in_set")?;
                let (set, ty) = self.path(set, model)?;
                let WireTy::Set(elem_ty) = bare(&ty) else {
                    return Err(self.wrong_field(&set, model));
                };
                let (elem, _) = self.operand(elem, model, Some(elem_ty))?;
                let elem = Box::new(elem);
                (Expr::InSet { elem, set }, bool_ty)
            }
            "in_map" => {
                let [key, map] = pair(body, "in_map")?;
                let (map, ty) = self.path(map, model)?;
                let WireTy::Map(key_ty, _) = bare(&ty) else {
                    return Err(self.wrong_field(&map, model));
                };
                let (key, _) = self.operand(key, model, Some(key_ty))?;
                let key = Box::new(key);
                (Expr::InMap { key, map }, bool_ty)
            }
            "map_get" => {
                let [map, key] = pair(body, "map_get")?;
                let (map, ty) = self.path(map, model)?;
                let WireTy::Map(key_ty, value_ty) = bare(&ty) else {
                    return Err(self.wrong_field(&map, model));
                };
                let (key, _) = self.operand(key, model, Some(key_ty))?;
                let key = Box::new(key);
                (Expr::MapGet { map, key }, Some((**value_ty).clone()))
            }
            "coalesce" => {
                let [e, fallback] = pair(body, "coalesce")?;
                let (e, ty) = self.expr(e, model)?;
                // The fallback is a value of `Query`, not an expression node.
                let fallback = constant(fallback, ty.as_ref())?;
                (Expr::Coalesce(Box::new(e), fallback), ty)
            }
            _ => return Err(WireError::WrongType("filter").into()),
        })
    }

    /// `{"hops": [[field, model], ...], "field": name}` with the type of the field it reaches.
    fn path(&self, json: Json, model: ModelId) -> Result<(Path, WireTy), QueryFault> {
        let mut f = fields(json, "path")?;
        let hops = f.req("hops", any)?;
        let field = f.req("field", string)?;
        f.finish()?;
        let (hops, end) = self.hops(hops, model)?;
        let (field, ty) = self.field(end, &field)?;
        Ok((Path { hops, field }, ty))
    }

    /// `[[field, model], ...]` with the model the last hop reaches.
    fn hops(&self, json: Json, model: ModelId) -> Result<(Vec<Hop>, ModelId), QueryFault> {
        let items = items(json, "hops")?;
        if items.len() > MAX_HOPS {
            return Err(QueryFault::TooManyHops);
        }
        let mut current = model;
        let mut hops = Vec::with_capacity(items.len());
        for item in items {
            let [field, target] = pair(item, "hops")?;
            let name = string("hops", field)?;
            let (field, ty) = self.field(current, &name)?;
            if *bare(&ty) != WireTy::Ref {
                return Err(QueryFault::WrongField(clip(&name)));
            }
            current = self.model(&string("hops", target)?)?;
            hops.push(Hop {
                field,
                model: current,
            });
        }
        Ok((hops, current))
    }

    /// [`QueryFault::WrongField`] for the field a path reaches.
    fn wrong_field(&self, path: &Path, model: ModelId) -> QueryFault {
        let end = path.hops.last().map_or(model, |hop| hop.model);
        let name = self.schema.field_name(end, path.field).unwrap_or_default();
        QueryFault::WrongField(clip(name))
    }
}

/// `{"const": value}` read in a place of type `ty`; `null` fits every place.
fn constant(json: Json, ty: Option<&WireTy>) -> Result<Value, QueryFault> {
    let (kind, value) = single(json, "const")?;
    if kind != "const" {
        return Err(WireError::WrongType("const").into());
    }
    let ty = ty.ok_or(QueryFault::UntypedConstant)?;
    if matches!(bare(ty), WireTy::Set(_) | WireTy::Map(..) | WireTy::List(_)) {
        return Err(QueryFault::CollectionConstant);
    }
    read_value(&optional(ty), value).map_err(QueryFault::Value)
}

fn is_const(json: &Json) -> bool {
    matches!(json, Json::Obj(entries) if matches!(entries.as_slice(), [(k, _)] if k == "const"))
}

/// The type without its `?`.
fn bare(ty: &WireTy) -> &WireTy {
    match ty {
        WireTy::Optional(inner) => bare(inner),
        other => other,
    }
}

/// `ty` that also admits `null`.
fn optional(ty: &WireTy) -> WireTy {
    WireTy::Optional(Box::new(bare(ty).clone()))
}

fn cmp_op(json: Json) -> Result<CmpOp, QueryFault> {
    let op = match string("cmp", json)?.as_str() {
        "eq" => CmpOp::Eq,
        "ne" => CmpOp::Ne,
        "lt" => CmpOp::Lt,
        "le" => CmpOp::Le,
        "gt" => CmpOp::Gt,
        "ge" => CmpOp::Ge,
        _ => return Err(WireError::WrongType("cmp").into()),
    };
    Ok(op)
}

fn any(_: &'static str, json: Json) -> Result<Json, WireError> {
    Ok(json)
}

fn items(json: Json, key: &'static str) -> Result<Vec<Json>, WireError> {
    match json {
        Json::Arr(items) => Ok(items),
        _ => Err(WireError::WrongType(key)),
    }
}

fn fields(json: Json, key: &'static str) -> Result<Fields, WireError> {
    match json {
        Json::Obj(entries) => Ok(Fields { entries }),
        _ => Err(WireError::WrongType(key)),
    }
}

/// An object with exactly one key.
fn single(json: Json, key: &'static str) -> Result<(String, Json), WireError> {
    let Json::Obj(entries) = json else {
        return Err(WireError::WrongType(key));
    };
    <[(String, Json); 1]>::try_from(entries)
        .map(|[entry]| entry)
        .map_err(|_| WireError::WrongType(key))
}

fn pair(json: Json, key: &'static str) -> Result<[Json; 2], WireError> {
    <[Json; 2]>::try_from(items(json, key)?).map_err(|_| WireError::WrongType(key))
}

fn triple(json: Json, key: &'static str) -> Result<[Json; 3], WireError> {
    <[Json; 3]>::try_from(items(json, key)?).map_err(|_| WireError::WrongType(key))
}

// ---------------------------------------------------------------------------------------------
// Writing

/// Writes a query in the form of the module documentation. Fails with
/// [`WireError::NotInSchema`] for an id the schema does not know, a defect of the caller.
pub(super) fn encode_query(query: &Query, schema: &dyn WireSchema) -> Result<String, WireError> {
    let writer = Writer { schema };
    let model = writer.model_name(query.model)?;
    let order = query
        .order
        .iter()
        .map(|(field, dir)| {
            let dir = match dir {
                Dir::Asc => "asc",
                Dir::Desc => "desc",
            };
            Ok(array([
                encode_str(writer.field_name(query.model, *field)?),
                encode_str(dir),
            ]))
        })
        .collect::<Result<Vec<_>, WireError>>()?;
    let filter = match &query.filter {
        Some(e) => writer.expr(e, query.model)?,
        None => "null".to_owned(),
    };
    let after = match &query.after {
        Some(cursor) => object(&[
            ("id", hex(&cursor.id.to_hex())),
            ("values", array(cursor.values.iter().map(encode))),
        ])?,
        None => "null".to_owned(),
    };
    object(&[
        ("model", encode_str(model)),
        ("filter", filter),
        ("order", array(order)),
        (
            "limit",
            query.limit.map_or("null".to_owned(), |n| n.to_string()),
        ),
        ("after", after),
    ])
}

struct Writer<'s> {
    schema: &'s dyn WireSchema,
}

impl Writer<'_> {
    fn model_name(&self, model: ModelId) -> Result<&str, WireError> {
        self.schema
            .model_name(model)
            .ok_or(WireError::NotInSchema("model"))
    }

    fn field_name(&self, model: ModelId, field: FieldId) -> Result<&str, WireError> {
        self.schema
            .field_name(model, field)
            .ok_or(WireError::NotInSchema("field"))
    }

    fn expr(&self, e: &Expr, model: ModelId) -> Result<String, WireError> {
        let (kind, body) = match e {
            Expr::Const(v) => ("const", encode(v)),
            Expr::Field(path) => ("field", self.path(path, model)?),
            Expr::RowId(hops) => ("row_id", self.hops(hops, model)?),
            Expr::Cmp(op, a, b) => {
                let op = match op {
                    CmpOp::Eq => "eq",
                    CmpOp::Ne => "ne",
                    CmpOp::Lt => "lt",
                    CmpOp::Le => "le",
                    CmpOp::Gt => "gt",
                    CmpOp::Ge => "ge",
                };
                let sides = [encode_str(op), self.expr(a, model)?, self.expr(b, model)?];
                ("cmp", array(sides))
            }
            Expr::And(items) => ("and", self.list(items, model)?),
            Expr::Or(items) => ("or", self.list(items, model)?),
            Expr::Not(a) => ("not", self.expr(a, model)?),
            Expr::InSet { elem, set } => (
                "in_set",
                array([self.expr(elem, model)?, self.path(set, model)?]),
            ),
            Expr::InMap { key, map } => (
                "in_map",
                array([self.expr(key, model)?, self.path(map, model)?]),
            ),
            Expr::MapGet { map, key } => (
                "map_get",
                array([self.path(map, model)?, self.expr(key, model)?]),
            ),
            Expr::Coalesce(a, fallback) => {
                let fallback = object(&[("const", encode(fallback))])?;
                ("coalesce", array([self.expr(a, model)?, fallback]))
            }
        };
        object(&[(kind, body)])
    }

    fn list(&self, items: &[Expr], model: ModelId) -> Result<String, WireError> {
        let items = items
            .iter()
            .map(|e| self.expr(e, model))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(array(items))
    }

    fn path(&self, path: &Path, model: ModelId) -> Result<String, WireError> {
        let end = path.hops.last().map_or(model, |hop| hop.model);
        object(&[
            ("hops", self.hops(&path.hops, model)?),
            ("field", encode_str(self.field_name(end, path.field)?)),
        ])
    }

    fn hops(&self, hops: &[Hop], model: ModelId) -> Result<String, WireError> {
        let mut current = model;
        let mut items = Vec::with_capacity(hops.len());
        for hop in hops {
            let field = encode_str(self.field_name(current, hop.field)?);
            items.push(array([field, encode_str(self.model_name(hop.model)?)]));
            current = hop.model;
        }
        Ok(array(items))
    }
}

fn object(fields: &[(&str, String)]) -> Result<String, WireError> {
    ostrel_core::canon::encode_object(fields).map_err(|e| WireError::DuplicateKey(clip(&e.0)))
}
