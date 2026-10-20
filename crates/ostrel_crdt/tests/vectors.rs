//! Runs the shared op vectors of `tests/crdt-vectors/` (format in its README) against the
//! Rust CRDTs, so Rust and `runtime/js/crdt/` are checked against the same hand written
//! expectations and must produce the same canonical output (AC-41).
//!
//! The crate takes no JSON dependency, so this test carries a small strict reader for the
//! subset the vector files use. The format itself is validated by `format.mjs` in the JS
//! gate; this runner checks only what it needs to run a case and fails on anything else.

use std::fs;
use std::path::{Path, PathBuf};

use ostrel_core::canon::{encode, encode_array, encode_object};
use ostrel_core::ids::{Hlc, OpId};
use ostrel_core::value::Value;
use ostrel_crdt::{AddWinsSet, LwwMap, LwwRegister};

/// Strategy directories of op vectors that have no Rust model in this crate yet, with the
/// reason. Every other directory with vectors must be run below.
const NOT_IN_THIS_CRATE: &[(&str, &str)] = &[("rank", "Rust Rank is a separate T5 task (D88)")];

/// Directories under `tests/crdt-vectors/` that do not hold op vectors.
const OTHER_FORMATS: &[&str] = &["canon"];

fn vector_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/crdt-vectors")
}

// ---------------------------------------------------------------------------------------
// Minimal strict JSON reader.

#[derive(Clone, Debug)]
enum Json {
    Null,
    Bool(bool),
    /// The number token as written, so integers and floats stay apart (README).
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

struct Reader<'a> {
    src: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn parse(text: &str) -> Json {
        let mut r = Reader {
            src: text.as_bytes(),
            pos: 0,
        };
        let value = r.value();
        r.ws();
        assert_eq!(r.pos, r.src.len(), "trailing bytes after the JSON value");
        value
    }

    fn ws(&mut self) {
        while matches!(self.src.get(self.pos), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> u8 {
        self.ws();
        *self.src.get(self.pos).expect("unexpected end of JSON")
    }

    fn next_byte(&mut self) -> u8 {
        let b = *self.src.get(self.pos).expect("unexpected end of JSON");
        self.pos += 1;
        b
    }

    fn value(&mut self) -> Json {
        match self.peek() {
            b'n' => self.word("null", Json::Null),
            b't' => self.word("true", Json::Bool(true)),
            b'f' => self.word("false", Json::Bool(false)),
            b'"' => Json::Str(self.string()),
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                if self.peek() == b']' {
                    self.pos += 1;
                    return Json::Arr(items);
                }
                loop {
                    items.push(self.value());
                    match (self.peek(), self.next_byte()) {
                        (_, b',') => continue,
                        (_, b']') => return Json::Arr(items),
                        (c, _) => panic!("bad array at byte {}: {:?}", self.pos, c as char),
                    }
                }
            }
            b'{' => {
                self.pos += 1;
                let mut fields: Vec<(String, Json)> = Vec::new();
                if self.peek() == b'}' {
                    self.pos += 1;
                    return Json::Obj(fields);
                }
                loop {
                    assert_eq!(self.peek(), b'"', "object key expected at {}", self.pos);
                    let key = self.string();
                    assert!(
                        fields.iter().all(|(k, _)| *k != key),
                        "duplicate key {key:?}"
                    );
                    assert_eq!(self.peek(), b':', "colon expected at {}", self.pos);
                    self.pos += 1;
                    let value = self.value();
                    fields.push((key, value));
                    match (self.peek(), self.next_byte()) {
                        (_, b',') => continue,
                        (_, b'}') => return Json::Obj(fields),
                        (c, _) => panic!("bad object at byte {}: {:?}", self.pos, c as char),
                    }
                }
            }
            b'-' | b'0'..=b'9' => self.number(),
            c => panic!("unexpected {:?} at byte {}", c as char, self.pos),
        }
    }

    fn word(&mut self, word: &str, value: Json) -> Json {
        let end = self.pos + word.len();
        assert_eq!(self.src.get(self.pos..end), Some(word.as_bytes()));
        self.pos = end;
        value
    }

    fn number(&mut self) -> Json {
        let start = self.pos;
        while matches!(
            self.src.get(self.pos),
            Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
        ) {
            self.pos += 1;
        }
        Json::Num(String::from_utf8(self.src[start..self.pos].to_vec()).unwrap())
    }

    fn hex4(&mut self) -> u32 {
        let digits = std::str::from_utf8(&self.src[self.pos..self.pos + 4]).unwrap();
        self.pos += 4;
        u32::from_str_radix(digits, 16).expect("bad \\u escape")
    }

    fn string(&mut self) -> String {
        assert_eq!(self.next_byte(), b'"');
        let mut out = Vec::new();
        loop {
            match self.next_byte() {
                b'"' => return String::from_utf8(out).expect("JSON string is not UTF-8"),
                b'\\' => {
                    let c = match self.next_byte() {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let high = self.hex4();
                            let code = if (0xd800..0xdc00).contains(&high) {
                                assert_eq!(self.next_byte(), b'\\');
                                assert_eq!(self.next_byte(), b'u');
                                let low = self.hex4();
                                assert!((0xdc00..0xe000).contains(&low), "lone surrogate");
                                0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)
                            } else {
                                high
                            };
                            char::from_u32(code).expect("lone surrogate")
                        }
                        other => panic!("bad escape \\{}", other as char),
                    };
                    let mut buf = [0; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                b if b < 0x20 => panic!("control byte in JSON string"),
                b => out.push(b),
            }
        }
    }
}

impl Json {
    fn field(&self, name: &str) -> &Json {
        match self {
            Json::Obj(fields) => fields
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("missing field {name:?}")),
            _ => panic!("field {name:?} of a non object"),
        }
    }

    fn keys(&self) -> Vec<&str> {
        match self {
            Json::Obj(fields) => fields.iter().map(|(k, _)| k.as_str()).collect(),
            _ => panic!("keys of a non object"),
        }
    }

    fn str(&self) -> &str {
        match self {
            Json::Str(s) => s,
            other => panic!("string expected, got {other:?}"),
        }
    }

    fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(items) => items,
            other => panic!("array expected, got {other:?}"),
        }
    }

    fn index(&self) -> usize {
        match self {
            Json::Num(n) => n.parse().expect("op index"),
            other => panic!("op index expected, got {other:?}"),
        }
    }

    /// The wire value this JSON stands for: integers as `Int`, other numbers as `Float`,
    /// strings as `Text`, arrays as `List`. Objects are no values.
    fn to_value(&self) -> Value {
        match self {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Num(n) if n.contains(['.', 'e', 'E']) => {
                Value::float(n.parse().unwrap()).unwrap()
            }
            Json::Num(n) => Value::int(n.parse().unwrap()).unwrap(),
            Json::Str(s) => Value::Text(s.clone()),
            Json::Arr(items) => Value::List(items.iter().map(Json::to_value).collect()),
            Json::Obj(_) => panic!("an object is not a wire value"),
        }
    }

    /// Canonical encoding (ARCHITECTURE 5.3) of this JSON document.
    fn canonical(&self) -> String {
        match self {
            Json::Arr(items) => {
                encode_array(&items.iter().map(Json::canonical).collect::<Vec<_>>())
            }
            Json::Obj(fields) => {
                let fields: Vec<(&str, String)> = fields
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.canonical()))
                    .collect();
                encode_object(&fields).unwrap()
            }
            scalar => encode(&scalar.to_value()),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Models in the shape of the README: apply one op envelope, read value and state.

trait Model {
    fn apply(&mut self, id: OpId, hlc: Hlc, body: &Json);
    fn value(&self) -> String;
    fn state(&self) -> String;
}

impl Model for LwwRegister {
    fn apply(&mut self, _id: OpId, hlc: Hlc, body: &Json) {
        assert_eq!(body.keys(), ["set"], "lww op body");
        LwwRegister::apply(self, hlc, body.field("set").to_value()).unwrap();
    }
    fn value(&self) -> String {
        self.encode_value()
    }
    fn state(&self) -> String {
        self.encode_state()
    }
}

impl Model for AddWinsSet {
    fn apply(&mut self, id: OpId, _hlc: Hlc, body: &Json) {
        let mut keys = body.keys();
        keys.sort_unstable();
        match keys.as_slice() {
            ["add"] => {
                self.add(id, body.field("add").to_value()).unwrap();
            }
            ["remove", "tags"] => {
                let tags: Vec<OpId> = body
                    .field("tags")
                    .arr()
                    .iter()
                    .map(|t| OpId::from_hex(t.str()).unwrap())
                    .collect();
                self.remove(&body.field("remove").to_value(), &tags)
                    .unwrap();
            }
            other => panic!("set op body with fields {other:?}"),
        }
    }
    fn value(&self) -> String {
        self.encode_value()
    }
    fn state(&self) -> String {
        self.encode_state()
    }
}

impl Model for LwwMap {
    fn apply(&mut self, _id: OpId, hlc: Hlc, body: &Json) {
        match body.keys().as_slice() {
            ["put"] => {
                let [key, value] = body.field("put").arr() else {
                    panic!("put takes [key, value]");
                };
                self.put(hlc, key.to_value(), value.to_value()).unwrap();
            }
            ["remove"] => {
                LwwMap::remove(self, hlc, body.field("remove").to_value()).unwrap();
            }
            other => panic!("map op body with fields {other:?}"),
        }
    }
    fn value(&self) -> String {
        self.encode_value()
    }
    fn state(&self) -> String {
        self.encode_state()
    }
}

// ---------------------------------------------------------------------------------------
// Runner.

/// Runs every vector of `strategy`; returns the number of files run.
fn run_strategy(strategy: &str, new_model: fn() -> Box<dyn Model>) -> usize {
    let dir = vector_root().join(strategy);
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "{strategy}/ has no vectors");
    for path in &files {
        let name = format!("{strategy}/{}", path.file_name().unwrap().to_string_lossy());
        let vector = Reader::parse(&fs::read_to_string(path).unwrap());
        let mut fields = vector.keys();
        fields.sort_unstable();
        assert_eq!(
            fields,
            ["deliveries", "description", "expect", "ops", "strategy"],
            "{name}: fields"
        );
        assert_eq!(vector.field("strategy").str(), strategy, "{name}: strategy");
        let ops: Vec<(OpId, Hlc, &Json)> = vector
            .field("ops")
            .arr()
            .iter()
            .map(|op| {
                (
                    OpId::from_hex(op.field("id").str()).unwrap(),
                    Hlc::from_hex(op.field("hlc").str()).unwrap(),
                    op.field("op"),
                )
            })
            .collect();
        let expect = vector.field("expect");
        let want_value = expect.field("value").canonical();
        let want_state = expect.field("state").canonical();
        for (number, order) in vector.field("deliveries").arr().iter().enumerate() {
            let mut model = new_model();
            for position in order.arr() {
                let (id, hlc, body) = ops[position.index()];
                model.apply(id, hlc, body);
            }
            assert_eq!(model.state(), want_state, "{name} delivery {number}: state");
            assert_eq!(model.value(), want_value, "{name} delivery {number}: value");
        }
    }
    files.len()
}

#[test]
fn ac_41_crdt_vectors_rust_lww() {
    assert!(run_strategy("lww", || Box::new(LwwRegister::new())) > 0);
}

#[test]
fn ac_41_crdt_vectors_rust_set() {
    assert!(run_strategy("set", || Box::new(AddWinsSet::new())) > 0);
}

#[test]
fn ac_41_crdt_vectors_rust_map() {
    assert!(run_strategy("map", || Box::new(LwwMap::new())) > 0);
}

/// A new strategy directory must get a Rust runner here or an entry with a reason in
/// `NOT_IN_THIS_CRATE`; it is never skipped silently.
#[test]
fn every_vector_directory_is_run_or_named() {
    let run = ["lww", "map", "set"];
    let mut dirs: Vec<String> = fs::read_dir(vector_root())
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    let mut known: Vec<String> = run
        .iter()
        .chain(NOT_IN_THIS_CRATE.iter().map(|(dir, _)| dir))
        .chain(OTHER_FORMATS.iter())
        .map(|s| (*s).to_owned())
        .collect();
    known.sort();
    assert_eq!(dirs, known);
}

#[test]
fn reader_handles_escapes_and_number_kinds() {
    let json = Reader::parse(r#"["\u0001\"\\😀", 9, 1.5, -0, null, true]"#);
    assert_eq!(
        json.canonical(),
        "[\"\\u0001\\\"\\\\\u{1f600}\",9,1.5,0,null,true]"
    );
}
