//! Scanning of integer literals (G9) and string literals with interpolation (G1).
//!
//! The lexer calls [`scan_int`] when it sees an ASCII digit and [`scan_string`] when it
//! sees a `"`. Both functions read only the literal itself, never recurse and never
//! allocate more than the literal needs, so hostile input is handled in linear time.
//!
//! A string literal is returned as a list of [`StringPart`]s. Text parts carry the decoded
//! characters. An interpolation part carries only the span between its braces: the lexer
//! tokenizes that range like any other code, so there is no lexer mode stack (G1). An
//! interpolation that contains an error is not returned as a part, which keeps the lexer
//! from reporting follow up errors for the same fault.
//!
//! Rules: `docs/grammar.md` sections 2.5 and 2.6, SPEC 12.1 and 12.4, ARCHITECTURE 3.1
//! (D53) and 3.2 (G1, G9). Codes and messages: `tests/errors/README.md`.

// This module reads untrusted input: no unwrap, expect, panic or unchecked indexing.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use ostrel_core::{Code, Diagnostic, FileId, Span};

/// Largest value of `Int`: 2^53 minus 1 (G9, D24). The smallest value is its negation.
pub const INT_MAX: i64 = 9_007_199_254_740_991;

/// Deepest accepted brace nesting inside one interpolation (G1, SPEC 12.1). The brace that
/// opens the interpolation is depth 1.
pub const MAX_INTERPOLATION_DEPTH: u32 = 32;

/// Number of hex digits shown in a malformed `\u{...}` escape before it is shortened.
const MAX_SHOWN_HEX_DIGITS: usize = 8;

/// The result of [`scan_int`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntLiteral {
    /// The digits of the literal.
    pub span: Span,
    /// The value, or `E0010` when the literal is outside the `Int` range.
    pub value: Result<i64, LiteralError>,
}

/// The result of [`scan_string`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StringLiteral {
    /// The whole literal including both quotes. For an unterminated literal it ends
    /// before the line end (or at the end of the file), so the lexer continues there.
    pub span: Span,
    /// Text and interpolations in source order. Adjacent text is merged into one part.
    pub parts: Vec<StringPart>,
    /// Every fault found in the literal, in source order. Empty for a valid literal.
    pub errors: Vec<LiteralError>,
}

/// One piece of a string literal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StringPart {
    /// Literal text with escapes already decoded.
    Text {
        /// The decoded characters.
        value: String,
        /// The source of this text, escapes included.
        span: Span,
    },
    /// An interpolation `{expr}` without errors.
    Interpolation {
        /// The source between the opening and the closing brace, braces excluded. The
        /// lexer tokenizes this range as an expression.
        inner: Span,
    },
}

/// A fault in a literal, with the position the diagnostic points to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiteralError {
    /// What is wrong.
    pub kind: LiteralErrorKind,
    /// The offending character, escape or token. Diagnostics use its start.
    pub span: Span,
}

/// The kinds of literal faults. Each has one code of `tests/errors/README.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiteralErrorKind {
    /// `E0003`: the line or the file ends before the closing quote. The span is the
    /// opening quote.
    UnterminatedString,
    /// `E0004`: a backslash followed by a character that starts no escape, for example
    /// `\q`, `\r` or `\0`. Holds the escape as written.
    UnknownEscape(String),
    /// `E0004`: `\u` without `{`, without hex digits, with more than 6 digits or without
    /// the closing `}`. Holds the escape as written, shortened when very long.
    MalformedUnicodeEscape(String),
    /// `E0005`: a well formed `\u{...}` whose value is a surrogate or above 10FFFF.
    /// Holds the escape as written.
    NotUnicodeScalar(String),
    /// `E0006`: a `"` inside an interpolation. The span is that quote.
    StringInInterpolation,
    /// `E0007`: the brace that would reach depth 33 inside an interpolation.
    InterpolationTooDeep,
    /// `E0010`: an integer literal above [`INT_MAX`].
    IntOutOfRange,
    /// A raw bidirectional control character (D53). The span is the character.
    BidiControl(char),
    /// A `}` in the text of a string literal outside any interpolation.
    UnescapedCloseBrace,
}

impl LiteralErrorKind {
    /// The diagnostic code.
    ///
    /// ASSUMPTION: `E0011` for bidirectional control characters (D53) and `E0012` for a
    /// lone `}` are the next free lexer codes. They enter `tests/errors/README.md` with
    /// their first golden, which may still renumber them.
    pub fn code(&self) -> Code {
        let number = match self {
            LiteralErrorKind::UnterminatedString => 3,
            LiteralErrorKind::UnknownEscape(_) | LiteralErrorKind::MalformedUnicodeEscape(_) => 4,
            LiteralErrorKind::NotUnicodeScalar(_) => 5,
            LiteralErrorKind::StringInInterpolation => 6,
            LiteralErrorKind::InterpolationTooDeep => 7,
            LiteralErrorKind::IntOutOfRange => 10,
            LiteralErrorKind::BidiControl(_) => 11,
            LiteralErrorKind::UnescapedCloseBrace => 12,
        };
        Code::new(number)
    }

    /// The one line message, exactly as in `tests/errors/README.md`.
    pub fn message(&self) -> String {
        match self {
            LiteralErrorKind::UnterminatedString => "unterminated string literal".to_string(),
            LiteralErrorKind::UnknownEscape(escape) => {
                format!("unknown escape sequence `{escape}`")
            }
            LiteralErrorKind::MalformedUnicodeEscape(escape) => {
                format!("malformed escape `{escape}`; write 1 to 6 hex digits between the braces")
            }
            LiteralErrorKind::NotUnicodeScalar(escape) => {
                format!("`{escape}` is not a Unicode scalar value")
            }
            LiteralErrorKind::StringInInterpolation => {
                "string literal inside interpolation; bind it with `let` first".to_string()
            }
            LiteralErrorKind::InterpolationTooDeep => {
                format!("interpolation nests braces deeper than {MAX_INTERPOLATION_DEPTH}")
            }
            LiteralErrorKind::IntOutOfRange => {
                format!("integer literal is outside the `Int` range of -{INT_MAX} to {INT_MAX}")
            }
            LiteralErrorKind::BidiControl(ch) => {
                let value = u32::from(*ch);
                format!(
                    "bidirectional control character U+{value:04X} is not allowed; \
                     write it as `\\u{{{value:X}}}` if it is needed"
                )
            }
            LiteralErrorKind::UnescapedCloseBrace => {
                "`}` in a string literal must be written as `\\}`".to_string()
            }
        }
    }
}

impl LiteralError {
    /// Converts the fault into an error diagnostic.
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::error(self.kind.code(), self.span, self.kind.message())
    }
}

/// Whether `ch` is one of the nine bidirectional control characters that D53 rejects
/// anywhere in raw source: U+202A to U+202E and U+2066 to U+2069.
pub fn is_bidi_control(ch: char) -> bool {
    matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Reads the integer literal that starts at byte `start` of `src`.
///
/// The literal is the longest run of ASCII digits. Leading zeros are allowed. A value
/// above [`INT_MAX`] is `E0010` at the first digit; a negative number is the unary `-`
/// applied to a literal, so the range check is symmetric. Any number of digits is read
/// without overflow.
///
/// If `start` is not on a digit, the span is empty and the value is 0; the lexer only
/// calls this function on a digit.
pub fn scan_int(file: FileId, src: &str, start: usize) -> IntLiteral {
    let rest = src.get(start..).unwrap_or("");
    let mut value: i64 = 0;
    let mut too_large = false;
    let mut len = 0;
    for byte in rest.bytes().take_while(u8::is_ascii_digit) {
        len += 1;
        if too_large {
            continue;
        }
        let digit = i64::from(byte - b'0');
        match value.checked_mul(10).and_then(|v| v.checked_add(digit)) {
            Some(next) if next <= INT_MAX => value = next,
            _ => too_large = true,
        }
    }
    let span = span_of(file, start, start + len);
    let value = if too_large {
        Err(LiteralError {
            kind: LiteralErrorKind::IntOutOfRange,
            span,
        })
    } else {
        Ok(value)
    };
    IntLiteral { span, value }
}

/// Reads the string literal whose opening quote is at byte `start` of `src`.
///
/// The literal ends at the matching closing quote. It never spans lines: a line end or
/// the end of the file before the closing quote gives `E0003` at the opening quote and
/// the literal ends just before the line end. All other faults are reported and the scan
/// continues, so every independent fault in the literal is found.
pub fn scan_string(file: FileId, src: &str, start: usize) -> StringLiteral {
    let mut scanner = Scanner {
        file,
        src,
        pos: start,
        parts: Vec::new(),
        errors: Vec::new(),
        text: String::new(),
        text_start: start,
    };
    let end = scanner.run(start);
    StringLiteral {
        span: span_of(file, start, end),
        parts: scanner.parts,
        errors: scanner.errors,
    }
}

/// Converts byte offsets to a span. Sources are at most `u32::MAX` bytes
/// (`ostrel_core::MAX_FILE_BYTES`), so the saturation never happens in practice.
fn span_of(file: FileId, start: usize, end: usize) -> Span {
    let start = u32::try_from(start).unwrap_or(u32::MAX);
    let end = u32::try_from(end).unwrap_or(u32::MAX);
    Span::new(file, start, end)
}

/// How a scanned interpolation ended.
enum InterpolationEnd {
    /// The matching `}` was found; the scan continues after it.
    Closed,
    /// The line or the file ended first; the literal is unterminated.
    LineEnd,
}

/// State of one [`scan_string`] call.
struct Scanner<'a> {
    file: FileId,
    src: &'a str,
    /// Byte offset of the next unread character.
    pos: usize,
    parts: Vec<StringPart>,
    errors: Vec<LiteralError>,
    /// Decoded text not yet pushed as a part.
    text: String,
    /// Where the pending text starts in the source.
    text_start: usize,
}

impl Scanner<'_> {
    /// The next unread character.
    fn peek(&self) -> Option<char> {
        self.src
            .get(self.pos..)
            .and_then(|rest| rest.chars().next())
    }

    /// The character after the next one.
    fn peek_second(&self) -> Option<char> {
        let mut chars = self.src.get(self.pos..)?.chars();
        chars.next();
        chars.next()
    }

    /// Consumes and returns the next character.
    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn error(&mut self, kind: LiteralErrorKind, start: usize, end: usize) {
        let span = span_of(self.file, start, end);
        self.errors.push(LiteralError { kind, span });
    }

    /// Pushes the pending text as a part, if there is any source for it.
    fn flush_text(&mut self, end: usize) {
        if end > self.text_start {
            let value = std::mem::take(&mut self.text);
            let span = span_of(self.file, self.text_start, end);
            self.parts.push(StringPart::Text { value, span });
        }
        self.text.clear();
    }

    /// Scans the whole literal and returns its end offset.
    fn run(&mut self, quote: usize) -> usize {
        // The opening quote.
        self.bump();
        self.text_start = self.pos;
        loop {
            let at = self.pos;
            match self.peek() {
                None | Some('\n') => {
                    self.flush_text(at);
                    self.unterminated(quote);
                    return at;
                }
                Some('"') => {
                    self.flush_text(at);
                    self.bump();
                    return self.pos;
                }
                Some('\\') => self.escape(),
                Some('{') => {
                    self.flush_text(at);
                    match self.interpolation() {
                        InterpolationEnd::Closed => self.text_start = self.pos,
                        InterpolationEnd::LineEnd => {
                            self.unterminated(quote);
                            return self.pos;
                        }
                    }
                }
                Some('}') => {
                    self.bump();
                    self.error(LiteralErrorKind::UnescapedCloseBrace, at, self.pos);
                }
                Some(ch) => {
                    self.bump();
                    if is_bidi_control(ch) {
                        self.error(LiteralErrorKind::BidiControl(ch), at, self.pos);
                    } else {
                        self.text.push(ch);
                    }
                }
            }
        }
    }

    fn unterminated(&mut self, quote: usize) {
        self.error(LiteralErrorKind::UnterminatedString, quote, quote + 1);
    }

    /// Reads one escape in text. The backslash is the next character. A backslash at the
    /// line end is left for the caller, which reports the unterminated literal.
    fn escape(&mut self) {
        let start = self.pos;
        let decoded = match self.peek_second() {
            None | Some('\n') => {
                self.bump();
                return;
            }
            Some('{') => '{',
            Some('}') => '}',
            Some('"') => '"',
            Some('\\') => '\\',
            Some('n') => '\n',
            Some('t') => '\t',
            Some('u') => {
                self.unicode_escape();
                return;
            }
            Some(other) => {
                self.bump();
                self.bump();
                self.error(
                    LiteralErrorKind::UnknownEscape(format!("\\{other}")),
                    start,
                    self.pos,
                );
                return;
            }
        };
        self.bump();
        self.bump();
        self.text.push(decoded);
    }

    /// Reads `\u{h}` with 1 to 6 hex digits. The backslash is the next character.
    fn unicode_escape(&mut self) {
        let start = self.pos;
        self.bump();
        self.bump();
        let mut written = String::from("\\u");
        if self.peek() != Some('{') {
            self.error(
                LiteralErrorKind::MalformedUnicodeEscape(written),
                start,
                self.pos,
            );
            return;
        }
        self.bump();
        written.push('{');

        let mut digits = 0usize;
        let mut value: u32 = 0;
        while let Some(ch) = self.peek().filter(char::is_ascii_hexdigit) {
            self.bump();
            digits += 1;
            if digits <= MAX_SHOWN_HEX_DIGITS {
                written.push(ch);
            } else if digits == MAX_SHOWN_HEX_DIGITS + 1 {
                written.push_str("...");
            }
            if digits <= 6 {
                value = value * 16 + ch.to_digit(16).unwrap_or(0);
            }
        }
        let closed = self.peek() == Some('}');
        if closed {
            self.bump();
            written.push('}');
        } else if let Some(stray) = self.stray_escape_tail() {
            // `\u{4G}`: the escape visibly ends at the `}`, so it is one malformed escape
            // and the `}` is not reported again as a lone brace.
            written.push_str(stray);
            self.pos += stray.len();
            self.error(
                LiteralErrorKind::MalformedUnicodeEscape(written),
                start,
                self.pos,
            );
            return;
        }

        if !closed || digits == 0 || digits > 6 {
            self.error(
                LiteralErrorKind::MalformedUnicodeEscape(written),
                start,
                self.pos,
            );
            return;
        }
        match char::from_u32(value) {
            Some(ch) => self.text.push(ch),
            None => self.error(LiteralErrorKind::NotUnicodeScalar(written), start, self.pos),
        }
    }

    /// The rest of a broken `\u{...}` up to and including its `}`, if a `}` follows within
    /// a few characters and before anything that ends or restarts text (a quote, a
    /// backslash, a brace, a line end or a bidirectional control character).
    fn stray_escape_tail(&self) -> Option<&str> {
        const MAX_TAIL_CHARS: usize = 8;
        let rest = self.src.get(self.pos..)?;
        for (index, ch) in rest.char_indices().take(MAX_TAIL_CHARS) {
            match ch {
                '}' => return rest.get(..index + 1),
                '"' | '\\' | '{' | '\n' => return None,
                ch if is_bidi_control(ch) => return None,
                _ => {}
            }
        }
        None
    }

    /// Reads one interpolation. The opening `{` is the next character.
    ///
    /// Braces are counted, with escaped braces not counting (SPEC 12.1). Everything else
    /// is left to the lexer, which tokenizes the inner range. Bidirectional control
    /// characters are reported here only if the interpolation is not returned as a part,
    /// because otherwise the lexer reports them while tokenizing it.
    fn interpolation(&mut self) -> InterpolationEnd {
        self.bump();
        let inner_start = self.pos;
        let errors_before = self.errors.len();
        let mut bidi = Vec::new();
        let mut depth: u32 = 1;
        let mut too_deep_reported = false;
        loop {
            let at = self.pos;
            match self.peek() {
                None | Some('\n') => {
                    self.errors.append(&mut bidi);
                    return InterpolationEnd::LineEnd;
                }
                Some('"') => {
                    if !self.skip_inner_string() {
                        // No second quote on this line: the quote is most likely meant to
                        // close the literal, and the real fault is the unclosed `{`. The
                        // literal is unterminated; one diagnostic for one fault.
                        self.pos = self.line_end();
                        self.errors.append(&mut bidi);
                        return InterpolationEnd::LineEnd;
                    }
                    self.error(LiteralErrorKind::StringInInterpolation, at, at + 1);
                }
                Some('\\') if matches!(self.peek_second(), Some('{' | '}')) => {
                    self.bump();
                    self.bump();
                }
                Some('{') => {
                    self.bump();
                    depth = depth.saturating_add(1);
                    if depth > MAX_INTERPOLATION_DEPTH && !too_deep_reported {
                        too_deep_reported = true;
                        self.error(LiteralErrorKind::InterpolationTooDeep, at, self.pos);
                    }
                }
                Some('}') => {
                    self.bump();
                    depth -= 1;
                    if depth == 0 {
                        if self.errors.len() == errors_before {
                            let inner = span_of(self.file, inner_start, at);
                            self.parts.push(StringPart::Interpolation { inner });
                        } else {
                            self.errors.append(&mut bidi);
                        }
                        return InterpolationEnd::Closed;
                    }
                }
                Some(ch) => {
                    self.bump();
                    if is_bidi_control(ch) {
                        let span = span_of(self.file, at, self.pos);
                        bidi.push(LiteralError {
                            kind: LiteralErrorKind::BidiControl(ch),
                            span,
                        });
                    }
                }
            }
        }
    }

    /// Skips a string literal inside an interpolation, starting at its quote. Returns
    /// false, without moving, if there is no closing quote on the same line.
    fn skip_inner_string(&mut self) -> bool {
        let Some(rest) = self.src.get(self.pos + 1..) else {
            return false;
        };
        let line = rest.split('\n').next().unwrap_or("");
        match line.find('"') {
            Some(offset) => {
                self.pos += 1 + offset + 1;
                true
            }
            None => false,
        }
    }

    /// Byte offset of the next line end, or the end of the source.
    fn line_end(&self) -> usize {
        let rest = self.src.get(self.pos..).unwrap_or("");
        self.pos + rest.find('\n').unwrap_or(rest.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: FileId = FileId::from_raw(0);

    fn int(src: &str) -> IntLiteral {
        scan_int(FILE, src, 0)
    }

    fn string(src: &str) -> StringLiteral {
        scan_string(FILE, src, 0)
    }

    fn span(start: u32, end: u32) -> Span {
        Span::new(FILE, start, end)
    }

    fn text(value: &str, start: u32, end: u32) -> StringPart {
        StringPart::Text {
            value: value.to_string(),
            span: span(start, end),
        }
    }

    fn interpolation(start: u32, end: u32) -> StringPart {
        StringPart::Interpolation {
            inner: span(start, end),
        }
    }

    fn error(kind: LiteralErrorKind, start: u32, end: u32) -> LiteralError {
        LiteralError {
            kind,
            span: span(start, end),
        }
    }

    /// The only fault of `src`, as (code, start offset, message). Any other number of
    /// faults gives a value that no expectation matches, with the faults in the message.
    fn single_error(src: &str) -> (String, u32, String) {
        let literal = string(src);
        match literal.errors.as_slice() {
            [error] => (
                error.kind.code().to_string(),
                error.span.start,
                error.kind.message(),
            ),
            other => (
                format!("{} faults", other.len()),
                u32::MAX,
                format!("{src:?}: {other:?}"),
            ),
        }
    }

    // Integer literals (G9).

    #[test]
    fn int_reads_the_digit_run() {
        let literal = int("1234 + 1");
        assert_eq!(literal.value, Ok(1234));
        assert_eq!(literal.span, span(0, 4));
    }

    #[test]
    fn int_allows_zero_and_leading_zeros() {
        assert_eq!(int("0").value, Ok(0));
        assert_eq!(int("007").value, Ok(7));
    }

    #[test]
    fn int_accepts_the_largest_value() {
        assert_eq!(int("9007199254740991").value, Ok(INT_MAX));
        assert_eq!(int("0009007199254740991").value, Ok(INT_MAX));
    }

    #[test]
    fn int_rejects_one_above_the_range() {
        let literal = int("9007199254740992");
        assert_eq!(literal.span, span(0, 16));
        assert_eq!(
            literal.value,
            Err(error(LiteralErrorKind::IntOutOfRange, 0, 16))
        );
        assert_eq!(LiteralErrorKind::IntOutOfRange.code().to_string(), "E0010");
        assert_eq!(
            LiteralErrorKind::IntOutOfRange.message(),
            "integer literal is outside the `Int` range of \
             -9007199254740991 to 9007199254740991"
        );
    }

    #[test]
    fn int_with_many_digits_does_not_overflow() {
        let digits = "9".repeat(100_000);
        let literal = int(&digits);
        assert_eq!(literal.span, span(0, 100_000));
        assert_eq!(
            literal.value,
            Err(error(LiteralErrorKind::IntOutOfRange, 0, 100_000))
        );
        assert_eq!(
            int("18446744073709551616").value,
            Err(error(LiteralErrorKind::IntOutOfRange, 0, 20))
        );
    }

    #[test]
    fn int_starts_at_the_given_offset() {
        let literal = scan_int(FILE, "x = 42)", 4);
        assert_eq!(literal.span, span(4, 6));
        assert_eq!(literal.value, Ok(42));
    }

    #[test]
    fn int_on_a_non_digit_is_empty() {
        let literal = int("x");
        assert_eq!(literal.span, span(0, 0));
        assert_eq!(literal.value, Ok(0));
        assert_eq!(scan_int(FILE, "1", 5).span, span(5, 5));
    }

    // Plain strings and escapes.

    #[test]
    fn string_plain_text() {
        let literal = string("\"hello\" rest");
        assert_eq!(literal.span, span(0, 7));
        assert_eq!(literal.parts, vec![text("hello", 1, 6)]);
        assert!(literal.errors.is_empty());
    }

    #[test]
    fn string_empty_has_no_parts() {
        let literal = string("\"\"");
        assert_eq!(literal.span, span(0, 2));
        assert!(literal.parts.is_empty());
        assert!(literal.errors.is_empty());
    }

    #[test]
    fn string_decodes_every_escape() {
        let literal = string(r#""\{\}\"\\\n\t\u{41}\u{1F600}\u{0}\u{202e}""#);
        assert!(literal.errors.is_empty(), "{:?}", literal.errors);
        let expected = "{}\"\\\n\tA\u{1F600}\u{0}\u{202E}";
        assert_eq!(literal.parts, vec![text(expected, 1, 41)]);
    }

    #[test]
    fn string_keeps_non_ascii_and_invisible_characters() {
        let literal = string("\"grüße \u{200B}\u{200D}\u{2060}\"");
        assert!(literal.errors.is_empty());
        assert_eq!(
            literal.parts,
            vec![text("grüße \u{200B}\u{200D}\u{2060}", 1, 18)]
        );
    }

    #[test]
    fn string_unknown_escape_is_e0004_at_the_backslash() {
        // Golden lex_unknown_escape: `print("a \q b")`, the backslash is column 12.
        let (code, start, message) = single_error(r#""a \q b""#);
        assert_eq!(code, "E0004");
        assert_eq!(start, 3);
        assert_eq!(message, "unknown escape sequence `\\q`");
        for escape in [r"\r", r"\0", r"\'", r"\ "] {
            let (code, _, _) = single_error(&format!("\"{escape}\""));
            assert_eq!(code, "E0004", "{escape}");
        }
    }

    #[test]
    fn string_unknown_escape_continues_with_the_text() {
        let literal = string(r#""a \q b""#);
        assert_eq!(literal.span, span(0, 8));
        assert_eq!(literal.parts, vec![text("a  b", 1, 7)]);
    }

    #[test]
    fn string_malformed_unicode_escapes_are_e0004() {
        // Goldens lex_escape_empty_unicode and lex_escape_unicode_too_long.
        let cases = [
            (r#""a \u{} b""#, "\\u{}"),
            (r#""a \u{0000041} b""#, "\\u{0000041}"),
            (r#""a \u{41 b""#, "\\u{41"),
            (r#""a \u41 b""#, "\\u"),
            (r#""a \u{4G} b""#, "\\u{4G}"),
            (r#""a \u{123456789ABC} b""#, "\\u{12345678...}"),
        ];
        for (src, written) in cases {
            let (code, start, message) = single_error(src);
            assert_eq!(code, "E0004", "{src}");
            assert_eq!(start, 3, "{src}");
            assert_eq!(
                message,
                format!("malformed escape `{written}`; write 1 to 6 hex digits between the braces"),
                "{src}"
            );
        }
    }

    #[test]
    fn string_unicode_escape_accepts_both_cases_and_six_digits() {
        let literal = string(r#""\u{00e9}\u{00E9}\u{10FFFF}""#);
        assert!(literal.errors.is_empty());
        assert_eq!(literal.parts, vec![text("éé\u{10FFFF}", 1, 27)]);
    }

    #[test]
    fn string_non_scalar_escapes_are_e0005() {
        // Goldens lex_escape_not_scalar and lex_escape_surrogate.
        for (src, start, written) in [
            (r#""bad \u{110000} end""#, 5, "\\u{110000}"),
            (r#""x \u{D800}""#, 3, "\\u{D800}"),
            (r#""x \u{dfff}""#, 3, "\\u{dfff}"),
            (r#""x \u{FFFFFF}""#, 3, "\\u{FFFFFF}"),
        ] {
            let (code, at, message) = single_error(src);
            assert_eq!(code, "E0005", "{src}");
            assert_eq!(at, start, "{src}");
            assert_eq!(
                message,
                format!("`{written}` is not a Unicode scalar value")
            );
        }
    }

    #[test]
    fn string_unterminated_at_line_end_is_e0003_at_the_quote() {
        // Golden lex_unterminated_string: `let s = "hello` followed by a line end.
        let src = "\"hello\n  print(s)\n";
        let (code, start, message) = single_error(src);
        assert_eq!((code.as_str(), start), ("E0003", 0));
        assert_eq!(message, "unterminated string literal");
        let literal = string(src);
        assert_eq!(literal.span, span(0, 6));
        assert_eq!(literal.parts, vec![text("hello", 1, 6)]);
    }

    #[test]
    fn string_unterminated_at_end_of_file() {
        let literal = string("\"abc");
        assert_eq!(literal.span, span(0, 4));
        assert_eq!(
            literal.errors,
            vec![error(LiteralErrorKind::UnterminatedString, 0, 1)]
        );
    }

    #[test]
    fn string_backslash_at_line_end_is_only_unterminated() {
        let literal = string("\"abc\\\nx\"");
        assert_eq!(literal.span, span(0, 5));
        let kinds: Vec<_> = literal.errors.iter().map(|e| e.kind.clone()).collect();
        assert_eq!(kinds, vec![LiteralErrorKind::UnterminatedString]);
        assert_eq!(string("\"\\").errors.len(), 1);
    }

    #[test]
    fn string_reports_every_independent_fault() {
        let literal = string(r#""\q \u{} \u{D800}""#);
        let codes: Vec<_> = literal
            .errors
            .iter()
            .map(|e| e.kind.code().to_string())
            .collect();
        assert_eq!(codes, vec!["E0004", "E0004", "E0005"]);
        let starts: Vec<_> = literal.errors.iter().map(|e| e.span.start).collect();
        assert_eq!(starts, vec![1, 4, 9]);
    }

    #[test]
    fn string_starts_at_the_given_offset() {
        let literal = scan_string(FILE, "let s = \"a{x}\"\n", 8);
        assert_eq!(literal.span, span(8, 14));
        assert_eq!(literal.parts, vec![text("a", 9, 10), interpolation(11, 12)]);
    }

    // Bidirectional control characters (D53).

    #[test]
    fn bidi_controls_are_exactly_the_nine_characters() {
        let rejected: Vec<char> = ('\u{2000}'..='\u{20FF}')
            .filter(|c| is_bidi_control(*c))
            .collect();
        assert_eq!(
            rejected,
            vec![
                '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}',
                '\u{2068}', '\u{2069}'
            ]
        );
        assert!(!is_bidi_control('\u{061C}'));
        assert!(!is_bidi_control('\u{200F}'));
    }

    #[test]
    fn string_reports_each_raw_bidi_control() {
        let literal = string("\"a\u{202E}b\u{2066}\"");
        assert_eq!(literal.span, span(0, 10));
        assert_eq!(
            literal.errors,
            vec![
                error(LiteralErrorKind::BidiControl('\u{202E}'), 2, 5),
                error(LiteralErrorKind::BidiControl('\u{2066}'), 6, 9),
            ]
        );
        assert_eq!(
            LiteralErrorKind::BidiControl('\u{202E}').message(),
            "bidirectional control character U+202E is not allowed; \
             write it as `\\u{202E}` if it is needed"
        );
    }

    // Interpolation (G1).

    #[test]
    fn interpolation_splits_text_and_expression() {
        let literal = string(r#""a {x + 1} b {y}""#);
        assert!(literal.errors.is_empty());
        assert_eq!(
            literal.parts,
            vec![
                text("a ", 1, 3),
                interpolation(4, 9),
                text(" b ", 10, 13),
                interpolation(14, 15),
            ]
        );
        assert_eq!(literal.span, span(0, 17));
    }

    #[test]
    fn interpolation_counts_nested_braces() {
        let literal = string(r#""{ {a, b} }!""#);
        assert!(literal.errors.is_empty());
        assert_eq!(literal.parts, vec![interpolation(2, 10), text("!", 11, 12)]);
    }

    #[test]
    fn double_brace_has_no_special_meaning() {
        let literal = string(r#""{{x}}""#);
        assert!(literal.errors.is_empty());
        assert_eq!(literal.parts, vec![interpolation(2, 5)]);
    }

    #[test]
    fn empty_interpolation_is_left_to_the_parser() {
        let literal = string(r#""a{}""#);
        assert!(literal.errors.is_empty());
        assert_eq!(literal.parts, vec![text("a", 1, 2), interpolation(3, 3)]);
    }

    #[test]
    fn escaped_braces_are_text_and_do_not_count() {
        let literal = string(r#""\{x\}""#);
        assert!(literal.errors.is_empty());
        assert_eq!(literal.parts, vec![text("{x}", 1, 6)]);

        // Inside an interpolation the escaped brace is skipped and left to the lexer.
        let literal = string(r#""{a \} b}""#);
        assert!(literal.errors.is_empty());
        assert_eq!(literal.parts, vec![interpolation(2, 8)]);
    }

    #[test]
    fn depth_32_is_accepted() {
        let src = format!("\"{}x{}\"", "{".repeat(32), "}".repeat(32));
        let literal = string(&src);
        assert!(literal.errors.is_empty(), "{:?}", literal.errors);
        assert_eq!(literal.parts, vec![interpolation(2, 65)]);
    }

    #[test]
    fn depth_33_is_e0007_at_the_33rd_brace() {
        // Golden lex_interpolation_too_deep: 33 braces, the error is at the 33rd.
        let src = format!("\"{}x{}\" rest", "{".repeat(33), "}".repeat(33));
        let (code, start, message) = single_error(&src);
        assert_eq!((code.as_str(), start), ("E0007", 33));
        assert_eq!(message, "interpolation nests braces deeper than 32");
        let literal = string(&src);
        assert_eq!(literal.span, span(0, 69));
        assert!(literal.parts.is_empty());
    }

    #[test]
    fn very_deep_nesting_reports_once_and_stays_linear() {
        let src = format!("\"{}{}\"", "{".repeat(200_000), "}".repeat(200_000));
        let literal = string(&src);
        assert_eq!(literal.errors.len(), 1);
        assert_eq!(literal.span, span(0, 400_002));
    }

    #[test]
    fn string_in_interpolation_is_e0006_at_the_inner_quote() {
        // Golden lex_string_in_interpolation: `print("a {"b"} c")`.
        let src = r#""a {"b"} c""#;
        let (code, start, message) = single_error(src);
        assert_eq!((code.as_str(), start), ("E0006", 4));
        assert_eq!(
            message,
            "string literal inside interpolation; bind it with `let` first"
        );
        let literal = string(src);
        assert_eq!(literal.span, span(0, 11));
        assert_eq!(literal.parts, vec![text("a ", 1, 3), text(" c", 8, 10)]);
    }

    #[test]
    fn inner_string_with_braces_is_skipped_whole() {
        let literal = string(r#""{"}"} ok""#);
        let kinds: Vec<_> = literal.errors.iter().map(|e| e.kind.clone()).collect();
        assert_eq!(kinds, vec![LiteralErrorKind::StringInInterpolation]);
        assert_eq!(literal.span, span(0, 10));
        assert_eq!(literal.parts, vec![text(" ok", 6, 9)]);
    }

    #[test]
    fn missing_close_brace_is_only_unterminated() {
        // `"a {x"`: the quote cannot close the literal, the `{` is still open.
        let literal = string("\"a {x\"\nnext");
        assert_eq!(
            literal.errors,
            vec![error(LiteralErrorKind::UnterminatedString, 0, 1)]
        );
        assert_eq!(literal.span, span(0, 6));
        assert_eq!(literal.parts, vec![text("a ", 1, 3)]);
    }

    #[test]
    fn interpolation_does_not_span_lines() {
        let literal = string("\"a {x\n}\"");
        let kinds: Vec<_> = literal.errors.iter().map(|e| e.kind.clone()).collect();
        assert_eq!(kinds, vec![LiteralErrorKind::UnterminatedString]);
        assert_eq!(literal.span, span(0, 5));
    }

    #[test]
    fn bidi_in_a_valid_interpolation_is_left_to_the_lexer() {
        let literal = string("\"{a\u{202E}}\"");
        assert!(literal.errors.is_empty());
        assert_eq!(literal.parts, vec![interpolation(2, 6)]);
    }

    #[test]
    fn bidi_in_a_broken_interpolation_is_reported_here() {
        let literal = string("\"{\"b\" \u{2067}}\"");
        let kinds: Vec<_> = literal.errors.iter().map(|e| e.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![
                LiteralErrorKind::StringInInterpolation,
                LiteralErrorKind::BidiControl('\u{2067}')
            ]
        );
    }

    #[test]
    fn lone_close_brace_in_text_is_rejected() {
        let (code, start, message) = single_error(r#""a } b""#);
        assert_eq!((code.as_str(), start), ("E0012", 3));
        assert_eq!(message, "`}` in a string literal must be written as `\\}`");
    }

    #[test]
    fn diagnostics_render_in_the_golden_format() {
        let mut map = ostrel_core::SourceMap::new();
        let src = "fn main()\n  print(\"a \\q b\")\n";
        let Ok(file) = map.add("tests/errors/lex_unknown_escape.ostl", src) else {
            unreachable!("small source");
        };
        let literal = scan_string(file, src, 18);
        let rendered: Vec<_> = literal
            .errors
            .iter()
            .map(|e| e.to_diagnostic().render(&map))
            .collect();
        assert_eq!(
            rendered,
            vec![
                "tests/errors/lex_unknown_escape.ostl:2:12: error[E0004]: \
                 unknown escape sequence `\\q`"
            ]
        );
    }

    #[test]
    fn hostile_input_never_panics() {
        let samples = [
            "\"",
            "\"\\",
            "\"\\u",
            "\"\\u{",
            "\"\\u{1",
            "\"{",
            "\"{\"",
            "\"{\"\"",
            "\"}",
            "\"{{{",
            "\"\u{202E}",
            "\"\\\u{202E}\"",
            "\"{\\",
            "\"{\\{",
            "\"é\\é\"",
        ];
        for sample in samples {
            for start in 0..=sample.len() {
                let literal = scan_string(FILE, sample, start);
                assert!(literal.span.end as usize <= sample.len(), "{sample:?}");
                let _ = scan_int(FILE, sample, start);
            }
        }
    }
}
