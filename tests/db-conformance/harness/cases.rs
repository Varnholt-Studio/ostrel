//! Decodes a case file (format 1, `tests/db-conformance/cases/README.md`) into driver calls.
//!
//! Every key is checked: an unknown or missing key, a value that does not fit its field type,
//! or an insert that does not set every field makes the whole file invalid. Decoding never
//! panics on a malformed file.

use std::collections::BTreeMap;

use ostrel_db::api::{DbError, Dir, FieldId, ModelId, Query, Row, RowId, Value, Write};

use super::json::Json;

/// Largest text a `text_repeat` value may expand to, so a broken case file cannot exhaust
/// memory. The largest value the cases need is 1 MiB.
pub const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

pub struct Suite {
    pub cases: Vec<Case>,
}

pub struct Case {
    pub name: String,
    pub steps: Vec<Step>,
}

pub struct Step {
    pub action: Action,
    pub expect: Expect,
}

pub enum Action {
    Tx(Vec<Write>),
    Query(Query),
}

pub enum Expect {
    Ok,
    Error(DbError),
    Rows(Vec<Row>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Type {
    Text,
    Int,
    Bool,
}

#[derive(Clone, Copy)]
struct Field {
    ty: Type,
    optional: bool,
}

type Model = BTreeMap<FieldId, Field>;

type Result<T> = std::result::Result<T, String>;

pub fn decode(file: &Json) -> Result<Suite> {
    let top = object(file, &["format", "ac", "schema", "cases"], &[], "file")?;
    if number(get(&top, "format"), "format")? != 1 {
        return Err("format: only format 1 is supported".to_string());
    }
    let ac = string(get(&top, "ac"), "ac")?;
    let digits = ac.strip_prefix("AC-").unwrap_or("");
    if digits.len() != 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("ac: expected \"AC-NN\", got {ac:?}"));
    }
    let schema = decode_schema(get(&top, "schema"))?;
    let raw = array(get(&top, "cases"), "cases")?;
    if raw.is_empty() {
        return Err("cases: a file needs at least one case".to_string());
    }
    let mut cases: Vec<Case> = Vec::new();
    for (i, c) in raw.iter().enumerate() {
        let case = decode_case(c, &schema).map_err(|e| format!("cases[{i}]: {e}"))?;
        if cases.iter().any(|other| other.name == case.name) {
            return Err(format!("cases[{i}]: duplicate name {}", case.name));
        }
        cases.push(case);
    }
    Ok(Suite { cases })
}

fn decode_schema(json: &Json) -> Result<Vec<Model>> {
    let mut models = Vec::new();
    for (i, m) in array(json, "schema")?.iter().enumerate() {
        let at = format!("schema[{i}]");
        let m = object(m, &["model", "name", "fields"], &[], &at)?;
        if number(get(&m, "model"), &at)? != i as u64 {
            return Err(format!("{at}: models are numbered from 0 in order"));
        }
        string(get(&m, "name"), &at)?;
        let mut fields = Model::new();
        for (j, f) in array(get(&m, "fields"), &at)?.iter().enumerate() {
            let at = format!("{at}.fields[{j}]");
            let f = object(f, &["field", "name", "type"], &["optional"], &at)?;
            if number(get(&f, "field"), &at)? != j as u64 {
                return Err(format!("{at}: fields are numbered from 0 in order"));
            }
            string(get(&f, "name"), &at)?;
            let ty = match string(get(&f, "type"), &at)? {
                "Text" => Type::Text,
                "Int" => Type::Int,
                "Bool" => Type::Bool,
                other => return Err(format!("{at}: unknown type {other:?}")),
            };
            let optional = match f.get("optional") {
                None => false,
                Some(Json::Bool(b)) => *b,
                Some(_) => return Err(format!("{at}: optional must be a boolean")),
            };
            fields.insert(j as FieldId, Field { ty, optional });
        }
        models.push(fields);
    }
    Ok(models)
}

fn decode_case(json: &Json, schema: &[Model]) -> Result<Case> {
    let c = object(json, &["name", "doc", "steps"], &[], "case")?;
    let name = string(get(&c, "name"), "name")?.to_string();
    if !valid_test_name(&name) {
        return Err(format!("name {name:?} is not of the form ac_NN_<topic>"));
    }
    string(get(&c, "doc"), "doc")?;
    let raw = array(get(&c, "steps"), "steps")?;
    if raw.is_empty() {
        return Err(format!("{name}: a case needs at least one step"));
    }
    let mut steps = Vec::new();
    for (i, s) in raw.iter().enumerate() {
        steps.push(decode_step(s, schema).map_err(|e| format!("{name}.steps[{i}]: {e}"))?);
    }
    Ok(Case { name, steps })
}

/// `ac_NN_<topic>` with a lowercase topic (MEASUREMENT 5.1).
fn valid_test_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("ac_") else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let topic = &rest[digits..];
    digits >= 2
        && topic.len() > 1
        && topic.starts_with('_')
        && topic[1..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn decode_step(json: &Json, schema: &[Model]) -> Result<Step> {
    let Json::Object(members) = json else {
        return Err("a step must be an object".to_string());
    };
    if members.iter().any(|(k, _)| k == "tx") {
        let s = object(json, &["tx", "expect"], &[], "step")?;
        let raw = array(get(&s, "tx"), "tx")?;
        if raw.is_empty() {
            return Err("tx: a transaction needs at least one write".to_string());
        }
        let mut writes = Vec::new();
        for (i, w) in raw.iter().enumerate() {
            writes.push(decode_write(w, schema).map_err(|e| format!("tx[{i}]: {e}"))?);
        }
        let expect = match get(&s, "expect") {
            Json::String(s) if s == "ok" => Expect::Ok,
            e @ Json::Object(_) => {
                let e = object(e, &["error"], &[], "expect")?;
                Expect::Error(match string(get(&e, "error"), "expect.error")? {
                    "Conflict" => DbError::Conflict,
                    "VersionMismatch" => DbError::VersionMismatch,
                    "NotFound" => DbError::NotFound,
                    other => return Err(format!("expect.error: unknown error {other:?}")),
                })
            }
            _ => return Err("expect: \"ok\" or {\"error\": ...}".to_string()),
        };
        Ok(Step {
            action: Action::Tx(writes),
            expect,
        })
    } else {
        let s = object(json, &["query", "expect"], &[], "step")?;
        let q = object(get(&s, "query"), &["model", "order", "limit"], &[], "query")?;
        let model_id = model_id(get(&q, "model"), schema, "query.model")?;
        let model = &schema[model_id as usize];
        let mut order = Vec::new();
        for (i, o) in array(get(&q, "order"), "query.order")?.iter().enumerate() {
            let at = format!("query.order[{i}]");
            let pair = array(o, &at)?;
            let [f, d] = pair else {
                return Err(format!("{at}: expected [field, direction]"));
            };
            let field = number(f, &at)?;
            let field = FieldId::try_from(field)
                .ok()
                .filter(|f| model.contains_key(f))
                .ok_or_else(|| format!("{at}: unknown field {field}"))?;
            let dir = match string(d, &at)? {
                "asc" => Dir::Asc,
                "desc" => Dir::Desc,
                other => return Err(format!("{at}: unknown direction {other:?}")),
            };
            order.push((field, dir));
        }
        let limit = match get(&q, "limit") {
            Json::Null => None,
            n => Some(
                u32::try_from(number(n, "query.limit")?)
                    .map_err(|_| "query.limit: too large".to_string())?,
            ),
        };
        let mut rows = Vec::new();
        for (i, r) in array(get(&s, "expect"), "expect")?.iter().enumerate() {
            let at = format!("expect[{i}]");
            let r = object(r, &["id", "version", "fields"], &[], &at)?;
            rows.push(Row {
                id: row_id(get(&r, "id"), &at)?,
                version: number(get(&r, "version"), &at)?,
                fields: decode_fields(get(&r, "fields"), model, true, &at)?,
            });
        }
        Ok(Step {
            action: Action::Query(Query {
                model: model_id,
                order,
                limit,
            }),
            expect: Expect::Rows(rows),
        })
    }
}

fn decode_write(json: &Json, schema: &[Model]) -> Result<Write> {
    let (kind, body) = match json {
        Json::Object(m) if m.len() == 1 => (m[0].0.as_str(), &m[0].1),
        _ => return Err("a write is an object with one key".to_string()),
    };
    let keys: &[&str] = match kind {
        "insert" => &["model", "row", "fields"],
        "update" => &["model", "row", "expect_version", "fields"],
        "delete" => &["model", "row", "expect_version"],
        other => return Err(format!("unknown write {other:?}")),
    };
    let w = object(body, keys, &[], kind)?;
    let model = model_id(get(&w, "model"), schema, kind)?;
    let fields = &schema[model as usize];
    let row = row_id(get(&w, "row"), kind)?;
    let version = || -> Result<u64> {
        let v = number(get(&w, "expect_version"), kind)?;
        if v == 0 {
            return Err(format!("{kind}: expect_version starts at 1"));
        }
        Ok(v)
    };
    Ok(match kind {
        "insert" => Write::Insert {
            model,
            row,
            fields: decode_fields(get(&w, "fields"), fields, true, kind)?,
        },
        "update" => Write::Update {
            model,
            row,
            expect_version: version()?,
            fields: decode_fields(get(&w, "fields"), fields, false, kind)?,
        },
        _ => Write::Delete {
            model,
            row,
            expect_version: version()?,
        },
    })
}

fn decode_fields(json: &Json, model: &Model, all: bool, at: &str) -> Result<Vec<(FieldId, Value)>> {
    let Json::Object(members) = json else {
        return Err(format!("{at}: fields must be an object"));
    };
    let mut out = Vec::new();
    for (k, v) in members {
        let id = canonical_decimal(k)
            .and_then(|k| k.parse::<FieldId>().ok())
            .filter(|id| model.contains_key(id))
            .ok_or_else(|| format!("{at}: unknown field {k:?}"))?;
        let field = model[&id];
        out.push((
            id,
            decode_value(v, field).map_err(|e| format!("{at}.{k}: {e}"))?,
        ));
    }
    if all && out.len() != model.len() {
        return Err(format!("{at}: every field of the model must be given"));
    }
    out.sort_by_key(|(id, _)| *id);
    Ok(out)
}

fn decode_value(json: &Json, field: Field) -> Result<Value> {
    let (kind, body) = match json {
        Json::Null if field.optional => return Ok(Value::Null),
        Json::Null => return Err("none in a field that is not optional".to_string()),
        Json::Bool(b) if field.ty == Type::Bool => return Ok(Value::Bool(*b)),
        Json::Object(m) if m.len() == 1 => (m[0].0.as_str(), &m[0].1),
        _ => return Err("value does not fit the field type".to_string()),
    };
    match (kind, field.ty) {
        ("int", Type::Int) => {
            let s = string(body, "int")?;
            let digits = s.strip_prefix('-').unwrap_or(s);
            canonical_decimal(digits)
                .filter(|_| s != "-0")
                .and_then(|_| s.parse::<i64>().ok())
                .map(Value::Int)
                .ok_or_else(|| format!("int: {s:?} is not an i64 in decimal"))
        }
        ("text", Type::Text) => Ok(Value::Text(string(body, "text")?.to_string())),
        ("text_repeat", Type::Text) => {
            let pair = array(body, "text_repeat")?;
            let [unit, count] = pair else {
                return Err("text_repeat: expected [unit, count]".to_string());
            };
            let unit = string(unit, "text_repeat")?;
            let count = number(count, "text_repeat")?;
            let total = usize::try_from(count)
                .ok()
                .and_then(|c| c.checked_mul(unit.len()));
            match total {
                _ if unit.is_empty() || count == 0 => {
                    Err("text_repeat: unit and count must not be empty".to_string())
                }
                Some(n) if n <= MAX_TEXT_BYTES => Ok(Value::Text(unit.repeat(count as usize))),
                _ => Err(format!("text_repeat: more than {MAX_TEXT_BYTES} bytes")),
            }
        }
        _ => Err("value does not fit the field type".to_string()),
    }
}

fn model_id(json: &Json, schema: &[Model], at: &str) -> Result<ModelId> {
    let n = number(json, at)?;
    ModelId::try_from(n)
        .ok()
        .filter(|m| (*m as usize) < schema.len())
        .ok_or_else(|| format!("{at}: unknown model {n}"))
}

fn row_id(json: &Json, at: &str) -> Result<RowId> {
    let s = string(json, at)?;
    canonical_decimal(s)
        .and_then(|s| s.parse::<u128>().ok())
        .map(RowId)
        .ok_or_else(|| format!("{at}: row id {s:?} is not a u128 in decimal"))
}

/// Digits without sign and without leading zeros.
fn canonical_decimal(s: &str) -> Option<&str> {
    let ok =
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'));
    ok.then_some(s)
}

// ---------------------------------------------------------------------------------------------
// Access helpers
// ---------------------------------------------------------------------------------------------

/// Checks that `json` is an object with every key of `required`, optionally keys of
/// `optional`, and nothing else.
fn object<'j>(
    json: &'j Json,
    required: &[&str],
    optional: &[&str],
    at: &str,
) -> Result<BTreeMap<&'j str, &'j Json>> {
    let Json::Object(members) = json else {
        return Err(format!("{at}: expected an object"));
    };
    let mut out = BTreeMap::new();
    for (k, v) in members {
        if !required.contains(&k.as_str()) && !optional.contains(&k.as_str()) {
            return Err(format!("{at}: unknown key {k:?}"));
        }
        out.insert(k.as_str(), v);
    }
    if let Some(missing) = required.iter().find(|k| !out.contains_key(*k)) {
        return Err(format!("{at}: missing key {missing:?}"));
    }
    Ok(out)
}

/// A key that [`object`] has checked to be present.
fn get<'j>(map: &BTreeMap<&str, &'j Json>, key: &str) -> &'j Json {
    map.get(key).copied().unwrap_or(&Json::Null)
}

fn array<'j>(json: &'j Json, at: &str) -> Result<&'j [Json]> {
    match json {
        Json::Array(items) => Ok(items),
        _ => Err(format!("{at}: expected an array")),
    }
}

fn string<'j>(json: &'j Json, at: &str) -> Result<&'j str> {
    match json {
        Json::String(s) => Ok(s),
        _ => Err(format!("{at}: expected a string")),
    }
}

/// A non negative integer written without fraction or exponent.
fn number(json: &Json, at: &str) -> Result<u64> {
    match json {
        Json::Number(s) => canonical_decimal(s)
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| format!("{at}: expected a non negative integer, got {s}")),
        _ => Err(format!("{at}: expected a number")),
    }
}
