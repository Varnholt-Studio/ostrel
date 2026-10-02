//! A small strict JSON reader for the case files (RFC 8259).
//!
//! The workspace has no JSON crate on the v0.1 path, and the case files are small, so the
//! harness reads them itself. Strictness is deliberate: duplicate object keys, lone surrogates,
//! trailing commas and trailing data are errors, because a silently ignored part of a case file
//! would be a test that never runs.

use std::fmt;

/// Deepest nesting of arrays and objects the reader accepts.
pub const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// The number exactly as written, already checked against the JSON grammar.
    Number(String),
    String(String),
    Array(Vec<Json>),
    /// Members in file order; keys are unique.
    Object(Vec<(String, Json)>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON error at byte {}: {}", self.offset, self.message)
    }
}

pub fn parse(text: &str) -> Result<Json, Error> {
    let mut p = Parser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    p.skip_ws();
    let value = p.value(0)?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return Err(p.error("trailing data after the value"));
    }
    Ok(value)
}

struct Parser<'t> {
    bytes: &'t [u8],
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, message: &str) -> Error {
        Error {
            offset: self.pos,
            message: message.to_string(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let b = self.peek();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect_word(&mut self, word: &str, value: Json) -> Result<Json, Error> {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.error("unknown literal"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, Error> {
        match self.peek() {
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => self.string().map(Json::String),
            Some(b't') => self.expect_word("true", Json::Bool(true)),
            Some(b'f') => self.expect_word("false", Json::Bool(false)),
            Some(b'n') => self.expect_word("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn check_depth(&self, depth: usize) -> Result<(), Error> {
        if depth > MAX_DEPTH {
            Err(self.error("nesting too deep"))
        } else {
            Ok(())
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, Error> {
        self.check_depth(depth)?;
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth)?);
            self.skip_ws();
            match self.next() {
                Some(b',') => {}
                Some(b']') => return Ok(Json::Array(items)),
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, Error> {
        self.check_depth(depth)?;
        self.pos += 1;
        let mut members: Vec<(String, Json)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(members));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a string key"));
            }
            let at = self.pos;
            let key = self.string()?;
            if members.iter().any(|(k, _)| *k == key) {
                return Err(Error {
                    offset: at,
                    message: format!("duplicate key {key:?}"),
                });
            }
            self.skip_ws();
            if self.next() != Some(b':') {
                return Err(self.error("expected ':'"));
            }
            self.skip_ws();
            let value = self.value(depth)?;
            members.push((key, value));
            self.skip_ws();
            match self.next() {
                Some(b',') => {}
                Some(b'}') => return Ok(Json::Object(members)),
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.pos - start
    }

    fn number(&mut self) -> Result<Json, Error> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.error("expected a digit")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if self.digits() == 0 {
                return Err(self.error("expected a digit after '.'"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.digits() == 0 {
                return Err(self.error("expected a digit in the exponent"));
            }
        }
        if matches!(self.peek(), Some(b'0'..=b'9')) {
            return Err(self.error("leading zero"));
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| self.error("number is not ASCII"))?;
        Ok(Json::Number(text.to_string()))
    }

    fn hex4(&mut self) -> Result<u32, Error> {
        let mut n = 0;
        for _ in 0..4 {
            let d = match self.next() {
                Some(b @ b'0'..=b'9') => b - b'0',
                Some(b @ b'a'..=b'f') => b - b'a' + 10,
                Some(b @ b'A'..=b'F') => b - b'A' + 10,
                _ => return Err(self.error("expected four hex digits")),
            };
            n = n * 16 + u32::from(d);
        }
        Ok(n)
    }

    fn escape(&mut self) -> Result<char, Error> {
        let c = match self.next() {
            Some(b'"') => '"',
            Some(b'\\') => '\\',
            Some(b'/') => '/',
            Some(b'b') => '\u{8}',
            Some(b'f') => '\u{c}',
            Some(b'n') => '\n',
            Some(b'r') => '\r',
            Some(b't') => '\t',
            Some(b'u') => {
                let hi = self.hex4()?;
                let code = if (0xD800..0xDC00).contains(&hi) {
                    if self.next() != Some(b'\\') || self.next() != Some(b'u') {
                        return Err(self.error("lone high surrogate"));
                    }
                    let lo = self.hex4()?;
                    if !(0xDC00..0xE000).contains(&lo) {
                        return Err(self.error("high surrogate without low surrogate"));
                    }
                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                } else if (0xDC00..0xE000).contains(&hi) {
                    return Err(self.error("lone low surrogate"));
                } else {
                    hi
                };
                char::from_u32(code).ok_or_else(|| self.error("invalid code point"))?
            }
            _ => return Err(self.error("invalid escape")),
        };
        Ok(c)
    }

    fn string(&mut self) -> Result<String, Error> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let start = self.pos;
            while matches!(self.peek(), Some(b) if b != b'"' && b != b'\\' && b >= 0x20) {
                self.pos += 1;
            }
            // The input is a &str and the run stops only at ASCII bytes, so the slice is
            // valid UTF-8.
            out.push_str(
                std::str::from_utf8(&self.bytes[start..self.pos])
                    .map_err(|_| self.error("invalid UTF-8"))?,
            );
            match self.next() {
                Some(b'"') => return Ok(out),
                Some(b'\\') => out.push(self.escape()?),
                Some(_) => {
                    self.pos -= 1;
                    return Err(self.error("control character in string"));
                }
                None => return Err(self.error("unterminated string")),
            }
        }
    }
}
