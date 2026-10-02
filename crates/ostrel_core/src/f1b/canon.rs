//! Canonical JSON writer (ARCHITECTURE 5.3, RED-A #27), byte identical to
//! `runtime/js/crdt/canon/canon.mjs`.
//!
//! * object keys sorted by UTF-8 bytes (equal to code point order), no whitespace;
//! * strings escape only `"`, `\` and characters below U+0020 (`\b \f \n \r \t`, otherwise
//!   `\u00xx` in lowercase); everything else is written as UTF-8, not normalised;
//! * integers without exponent, sign only when negative;
//! * floats as ECMAScript `Number.prototype.toString` writes them (RFC 8785 3.2.2.3), `-0`
//!   as `0`.
//!
//! NaN, infinities, out of range integers and unpaired surrogates cannot reach the writer:
//! [`Value`] rules them out by construction. Recursion follows the nesting of the value; the
//! wire parser in `ostrel_sync` bounds that depth before a value is built.

use std::fmt;
use std::fmt::Write as _;

use super::value::{Value, base64url, sorted, sorted_entries};

/// Two fields with the same key passed to [`encode_object`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateKey(pub String);

impl fmt::Display for DuplicateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "duplicate object key {:?}", self.0)
    }
}

impl std::error::Error for DuplicateKey {}

/// Canonical encoding of a value. `Set` elements and `Map` entries are written in
/// [`compare_key`](super::value::compare_key) order, so equal sets encode equally whatever
/// the order of their vector.
pub fn encode(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

/// Canonical encoding of a JSON object whose field values are already canonical encodings.
///
/// Keys are sorted by UTF-8 bytes. A repeated key is an error, because two parsers may keep
/// different copies of it. The values are trusted to be canonical; this function does not
/// parse them.
///
/// ASSUMPTION: returns `Result` (ARCHITECTURE 5.3 sketches `-> String`) so that a repeated key
/// cannot produce ambiguous bytes.
pub fn encode_object(fields: &[(&str, String)]) -> Result<String, DuplicateKey> {
    let mut sorted: Vec<&(&str, String)> = fields.iter().collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    if let Some(pair) = sorted
        .windows(2)
        .find(|w| matches!(w, [a, b] if a.0 == b.0))
    {
        let key = pair.first().map(|f| f.0).unwrap_or_default();
        return Err(DuplicateKey(key.to_owned()));
    }
    let mut out = String::from("{");
    for (i, (key, value)) in sorted.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_str(&mut out, key);
        out.push(':');
        out.push_str(value);
    }
    out.push('}');
    Ok(out)
}

/// Canonical encoding of a JSON array whose items are already canonical encodings.
pub fn encode_array(items: &[String]) -> String {
    let mut out = String::from("[");
    out.push_str(&items.join(","));
    out.push(']');
    out
}

/// Canonical encoding of a string.
pub fn encode_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    write_str(&mut out, text);
    out
}

/// Canonical encoding of a finite float; the caller guarantees finiteness (see
/// [`FiniteFloat`](super::value::FiniteFloat)). A non finite input is written as `null`, which
/// no valid value ever produces.
pub fn encode_f64(v: f64) -> String {
    let mut out = String::new();
    write_f64(&mut out, v);
    out
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Int(i) | Value::Time(i) => {
            let _ = write!(out, "{}", i.get());
        }
        Value::Float(f) => write_f64(out, f.get()),
        Value::Text(s) | Value::Enum(s) | Value::Rank(s) => write_str(out, s),
        Value::Bytes(b) => write_str(out, &base64url(b)),
        Value::Ref(r) => write_str(out, &r.to_hex()),
        Value::Set(items) => write_items(out, sorted(items)),
        Value::List(items) => write_items(out, items.iter().collect()),
        Value::Map(entries) => {
            out.push('[');
            for (i, (k, v)) in sorted_entries(entries).into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('[');
                write_value(out, k);
                out.push(',');
                write_value(out, v);
                out.push(']');
            }
            out.push(']');
        }
    }
}

fn write_items(out: &mut String, items: Vec<&Value>) {
    out.push('[');
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_value(out, item);
    }
    out.push(']');
}

fn write_str(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// ECMAScript `Number::toString(10)` (ECMA-262 6.1.6.1.20) for finite values.
///
/// Rust's `{:e}` yields the shortest digit string that round trips, which is the `k` digits
/// of step 5; the layout below is steps 6 to 10.
fn write_f64(out: &mut String, v: f64) {
    if !v.is_finite() {
        out.push_str("null");
        return;
    }
    if v == 0.0 {
        // Both zeros.
        out.push('0');
        return;
    }
    if v < 0.0 {
        out.push('-');
    }
    let sci = format!("{:e}", v.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exp: i32 = exp.parse().unwrap_or(0);
    let k = digits.len() as i32;
    // n is the position of the decimal point relative to the digit string.
    let n = exp + 1;
    if k <= n && n <= 21 {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        let (int, frac) = digits.split_at(n as usize);
        out.push_str(int);
        out.push('.');
        out.push_str(frac);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-n) as usize));
        out.push_str(&digits);
    } else {
        let (first, rest) = digits.split_at(1);
        out.push_str(first);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        out.push('e');
        out.push(if n - 1 < 0 { '-' } else { '+' });
        let _ = write!(out, "{}", (n - 1).abs());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::f1b::ids::{Hlc, ReplicaId, RowId};

    fn int(v: i64) -> Value {
        Value::int(v).unwrap()
    }

    #[test]
    fn floats_follow_ecmascript() {
        let cases = [
            (1.0, "1"),
            (-1.5, "-1.5"),
            (123.456, "123.456"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (1.5e21, "1.5e+21"),
            (1e-6, "0.000001"),
            (1.25e-6, "0.00000125"),
            (1e-7, "1e-7"),
            (-2.5e-7, "-2.5e-7"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            (-0.0, "0"),
            (0.1 + 0.2, "0.30000000000000004"),
            (2f64.powi(53), "9007199254740992"),
        ];
        for (v, want) in cases {
            assert_eq!(encode_f64(v), want, "{v:e}");
            assert_eq!(encode(&Value::float(v).unwrap()), want);
        }
    }

    #[test]
    fn collections_and_wire_forms() {
        let row = RowId::new(Hlc::new(1, 2, ReplicaId(3)).unwrap());
        let set = Value::Set(vec![
            Value::Text("b".into()),
            int(10),
            int(9),
            Value::Bool(true),
        ]);
        assert_eq!(encode(&set), "[true,9,10,\"b\"]");
        let map = Value::Map(vec![
            (Value::Text("\u{1}".into()), int(1)),
            (Value::Text("!".into()), Value::Null),
        ]);
        assert_eq!(encode(&map), "[[\"\\u0001\",1],[\"!\",null]]");
        let list = Value::List(vec![int(2), int(1)]);
        assert_eq!(encode(&list), "[2,1]");
        assert_eq!(encode(&Value::Bytes(vec![0xfb, 0xff])), "\"-_8\"");
        assert_eq!(
            encode(&Value::Ref(row)),
            "\"00000000000100020000000000000003\""
        );
        assert_eq!(encode(&Value::time(-1).unwrap()), "-1");
        assert_eq!(encode(&Value::Enum("Open".into())), "\"Open\"");
    }

    #[test]
    fn objects_sort_keys_and_refuse_duplicates() {
        let fields = [
            ("b", "1".to_owned()),
            ("B", "2".to_owned()),
            ("a", "[]".to_owned()),
        ];
        assert_eq!(
            encode_object(&fields).unwrap(),
            "{\"B\":2,\"a\":[],\"b\":1}"
        );
        assert_eq!(encode_object(&[]).unwrap(), "{}");
        let dup = [
            ("k", "1".to_owned()),
            ("j", "0".to_owned()),
            ("k", "2".to_owned()),
        ];
        assert_eq!(encode_object(&dup), Err(DuplicateKey("k".into())));
        assert_eq!(encode_array(&[]), "[]");
        assert_eq!(encode_array(&["1".into(), "{}".into()]), "[1,{}]");
    }

    #[test]
    fn strings_escape_only_what_the_rule_names() {
        assert_eq!(encode_str("\u{7f}/\u{2028}<"), "\"\u{7f}/\u{2028}<\"");
        assert_eq!(encode_str("\u{0}\u{1f}"), "\"\\u0000\\u001f\"");
        assert_eq!(encode_str("\u{202e}"), "\"\u{202e}\"");
    }
}
