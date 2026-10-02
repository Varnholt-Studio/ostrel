//! Runs the shared canonical encoding cases of `tests/crdt-vectors/canon/cases.json` (T5
//! format, see its README) against the Rust writer and the hex forms of the ids, so Rust and
//! `runtime/js/crdt/canon/canon.mjs` are checked against the same expectations.
//!
//! `ostrel_core` has no JSON parser and takes no dependency for one (ARCHITECTURE 5.3), so the
//! test carries a small strict reader for the subset the case file uses.

use super::canon::{encode, encode_array, encode_object};
use super::ids::{Hlc, OpId, ReplicaId, RowId, ServerSeq};
use super::value::Value;

const CASES: &str = include_str!("../../../../tests/crdt-vectors/canon/cases.json");

#[derive(Debug, Clone)]
enum Json {
    Null,
    Bool(bool),
    /// The number token as written, so integers and floats are told apart like the README says.
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
    fn ws(&mut self) {
        while matches!(self.src.get(self.pos), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, b: u8) {
        self.ws();
        assert_eq!(
            self.src.get(self.pos),
            Some(&b),
            "expected {:?} at {}",
            b as char,
            self.pos
        );
        self.pos += 1;
    }

    fn value(&mut self) -> Json {
        self.ws();
        match self.src[self.pos] {
            b'n' => self.word("null", Json::Null),
            b't' => self.word("true", Json::Bool(true)),
            b'f' => self.word("false", Json::Bool(false)),
            b'"' => Json::Str(self.string()),
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                self.ws();
                if self.src[self.pos] == b']' {
                    self.pos += 1;
                    return Json::Arr(items);
                }
                loop {
                    items.push(self.value());
                    self.ws();
                    let c = self.src[self.pos];
                    self.pos += 1;
                    match c {
                        b',' => continue,
                        b']' => return Json::Arr(items),
                        _ => panic!("bad array at {}", self.pos),
                    }
                }
            }
            b'{' => {
                self.pos += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.src[self.pos] == b'}' {
                    self.pos += 1;
                    return Json::Obj(fields);
                }
                loop {
                    self.ws();
                    let key = self.string();
                    self.eat(b':');
                    fields.push((key, self.value()));
                    self.ws();
                    let c = self.src[self.pos];
                    self.pos += 1;
                    match c {
                        b',' => continue,
                        b'}' => return Json::Obj(fields),
                        _ => panic!("bad object at {}", self.pos),
                    }
                }
            }
            _ => {
                let start = self.pos;
                while matches!(
                    self.src.get(self.pos),
                    Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                ) {
                    self.pos += 1;
                }
                assert!(self.pos > start, "unexpected byte at {start}");
                Json::Num(String::from_utf8(self.src[start..self.pos].to_vec()).unwrap())
            }
        }
    }

    fn word(&mut self, w: &str, v: Json) -> Json {
        assert!(self.src[self.pos..].starts_with(w.as_bytes()));
        self.pos += w.len();
        v
    }

    fn hex4(&mut self) -> u16 {
        let s = std::str::from_utf8(&self.src[self.pos..self.pos + 4]).unwrap();
        self.pos += 4;
        u16::from_str_radix(s, 16).unwrap()
    }

    fn string(&mut self) -> String {
        assert_eq!(self.src[self.pos], b'"');
        self.pos += 1;
        let mut units: Vec<u16> = Vec::new();
        loop {
            let rest = std::str::from_utf8(&self.src[self.pos..]).unwrap();
            let c = rest.chars().next().unwrap();
            self.pos += c.len_utf8();
            match c {
                '"' => break,
                '\\' => {
                    let e = self.src[self.pos];
                    self.pos += 1;
                    match e {
                        b'"' => units.push(0x22),
                        b'\\' => units.push(0x5c),
                        b'/' => units.push(0x2f),
                        b'b' => units.push(0x08),
                        b'f' => units.push(0x0c),
                        b'n' => units.push(0x0a),
                        b'r' => units.push(0x0d),
                        b't' => units.push(0x09),
                        b'u' => {
                            let u = self.hex4();
                            units.push(u);
                        }
                        _ => panic!("bad escape"),
                    }
                }
                c => {
                    let mut buf = [0u16; 2];
                    units.extend_from_slice(c.encode_utf16(&mut buf));
                }
            }
        }
        // The case file itself must be valid text; unpaired surrogates come as "utf16" input.
        String::from_utf16(&units).expect("case file string with an unpaired surrogate")
    }
}

fn parse(src: &str) -> Json {
    let mut r = Reader {
        src: src.as_bytes(),
        pos: 0,
    };
    let v = r.value();
    r.ws();
    assert_eq!(r.pos, src.len(), "trailing bytes in the case file");
    v
}

fn field<'a>(obj: &'a Json, name: &str) -> Option<&'a Json> {
    match obj {
        Json::Obj(fields) => fields.iter().find(|(k, _)| k == name).map(|(_, v)| v),
        _ => None,
    }
}

fn str_field<'a>(obj: &'a Json, name: &str) -> &'a str {
    match field(obj, name) {
        Some(Json::Str(s)) => s,
        other => panic!("field {name} is not a string: {other:?}"),
    }
}

/// Encodes a JSON tree with the library: scalars and arrays through [`Value`] and the array
/// helper, objects through [`encode_object`].
fn canonical(json: &Json) -> String {
    match json {
        Json::Null => encode(&Value::Null),
        Json::Bool(b) => encode(&Value::Bool(*b)),
        Json::Num(n) if n.contains(['.', 'e', 'E']) => {
            encode(&Value::float(n.parse().unwrap()).unwrap())
        }
        Json::Num(n) => encode(&Value::int(n.parse().unwrap()).unwrap()),
        Json::Str(s) => encode(&Value::Text(s.clone())),
        Json::Arr(items) => encode_array(&items.iter().map(canonical).collect::<Vec<_>>()),
        Json::Obj(fields) => {
            let encoded: Vec<(&str, String)> = fields
                .iter()
                .map(|(k, v)| (k.as_str(), canonical(v)))
                .collect();
            encode_object(&encoded).unwrap()
        }
    }
}

fn dec<T: std::str::FromStr>(input: &Json, name: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    str_field(input, name).parse().unwrap()
}

fn clock(input: &Json) -> Hlc {
    Hlc::new(
        dec(input, "wall_ms"),
        dec(input, "counter"),
        ReplicaId(dec(input, "replica")),
    )
    .unwrap()
}

#[test]
fn shared_canon_cases() {
    let file = parse(CASES);
    assert_eq!(str_field(&file, "strategy"), "canon");
    let Some(Json::Arr(cases)) = field(&file, "cases") else {
        panic!("cases is not an array");
    };
    assert!(cases.len() >= 30, "case file looks truncated");
    let mut names = std::collections::HashSet::new();
    let mut kinds = std::collections::BTreeSet::new();
    for case in cases {
        let name = str_field(case, "name");
        assert!(names.insert(name.to_owned()), "duplicate case {name}");
        let kind = str_field(case, "kind");
        kinds.insert(kind.to_owned());
        let input = field(case, "input").expect("input");
        let want = field(case, "canonical").map(|_| str_field(case, "canonical"));
        let got: Result<String, ()> = match kind {
            "value" => Ok(canonical(input)),
            "utf16" => {
                let Json::Arr(units) = input else {
                    panic!("{name}: input")
                };
                let units: Vec<u16> = units
                    .iter()
                    .map(|u| match u {
                        Json::Num(n) => n.parse().unwrap(),
                        _ => panic!("{name}: unit"),
                    })
                    .collect();
                // The Rust boundary for UTF-16 text: an unpaired surrogate is refused.
                String::from_utf16(&units)
                    .map(|s| encode(&Value::Text(s)))
                    .map_err(|_| ())
            }
            "replica_id" => {
                let Json::Str(s) = input else {
                    panic!("{name}: input")
                };
                Ok(ReplicaId(s.parse().unwrap()).to_hex())
            }
            "server_seq" => {
                let Json::Str(s) = input else {
                    panic!("{name}: input")
                };
                Ok(ServerSeq(s.parse().unwrap()).to_hex())
            }
            "op_id" => Ok(OpId {
                replica: ReplicaId(dec(input, "replica")),
                seq: dec(input, "seq"),
            }
            .to_hex()),
            "hlc" => Ok(clock(input).to_hex()),
            "row_id" => Ok(RowId::new(clock(input)).to_hex()),
            other => panic!("{name}: unknown kind {other}"),
        };
        match want {
            Some(want) => {
                assert_eq!(got.as_deref(), Ok(want), "case {name}");
                // Ids also parse back from their canonical form.
                let back = match kind {
                    "replica_id" => ReplicaId::from_hex(want).map(|v| v.to_hex()).ok(),
                    "server_seq" => ServerSeq::from_hex(want).map(|v| v.to_hex()).ok(),
                    "op_id" => OpId::from_hex(want).map(|v| v.to_hex()).ok(),
                    "hlc" => Hlc::from_hex(want).map(|v| v.to_hex()).ok(),
                    "row_id" => RowId::from_hex(want).map(|v| v.to_hex()).ok(),
                    _ => Some(want.to_owned()),
                };
                assert_eq!(back.as_deref(), Some(want), "case {name} round trip");
            }
            None => {
                assert_eq!(str_field(case, "error"), "Invalid", "case {name}");
                assert!(got.is_err(), "case {name} must be refused");
            }
        }
    }
    for kind in [
        "value",
        "utf16",
        "replica_id",
        "server_seq",
        "op_id",
        "hlc",
        "row_id",
    ] {
        assert!(kinds.contains(kind), "no case of kind {kind}");
    }
}
