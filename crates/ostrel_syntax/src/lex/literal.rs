//! The literal scanner: contents of integer literals (G9) and of string text (G1).
//!
//! [`Literals`] implements [`LiteralScan`], the seam to the lexer driver in `mod.rs`. The
//! driver owns the structure of a string literal: its quotes, the interpolations and the
//! brace count inside them. This module reads only one piece of string text at a time,
//! from just after a `"` or a closing `}` up to the next `"`, the next unescaped `{` or the
//! line end, and checks what is written there (escapes, raw characters).
//!
//! Every function reads each byte at most a constant number of times and never looks
//! back, so a scan is linear in the length of what it consumes. That keeps the whole
//! lexer linear on hostile input (AC-04, AC-36).
//!
//! Rules: `docs/grammar.md` sections 2.5 and 2.6, SPEC 12.1 and 12.4, ARCHITECTURE 3.2
//! (G1, G9) and 3.5 (D53, D68). Codes and messages come from [`codes`] only.

use ostrel_core::value::INT_MAX;
use ostrel_core::{Code, Diagnostic, FileId, Span};

use super::{LiteralScan, TextStop, codes};

/// Number of hex digits shown in a malformed `\u{...}` escape before it is shortened.
const MAX_SHOWN_HEX_DIGITS: usize = 8;

/// Most characters after the hex digits of a broken `\u{...}` that still count as part
/// of the escape when a `}` follows (see [`TextScanner::stray_escape_tail`]).
const MAX_STRAY_TAIL_CHARS: usize = 8;

/// The literal scanner used by [`super::lex`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Literals;

impl LiteralScan for Literals {
    /// The literal is the longest run of ASCII digits; leading zeros are allowed. A value
    /// above [`INT_MAX`] is `E0010`, spanning the digits. A negative number is the unary
    /// `-` applied to a literal, so the range is symmetric. Any number of digits is read
    /// without overflow.
    fn int(&self, src: &str, start: usize, file: FileId, diags: &mut Vec<Diagnostic>) -> usize {
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
        let end = start + len;
        if too_large {
            diags.push(Diagnostic::error(
                codes::INT_OUT_OF_RANGE,
                span_of(file, start, end),
                codes::MSG_INT_OUT_OF_RANGE,
            ));
        }
        end
    }

    fn string_text(
        &self,
        src: &str,
        start: usize,
        file: FileId,
        diags: &mut Vec<Diagnostic>,
    ) -> (usize, TextStop) {
        let mut scanner = TextScanner {
            src,
            file,
            pos: start,
            diags,
        };
        let stop = scanner.run();
        (scanner.pos, stop)
    }
}

/// Converts byte offsets to a span. Sources are at most `u32::MAX` bytes
/// (`ostrel_core::MAX_FILE_BYTES`), so the saturation never happens in practice.
fn span_of(file: FileId, start: usize, end: usize) -> Span {
    let start = u32::try_from(start).unwrap_or(u32::MAX);
    let end = u32::try_from(end).unwrap_or(u32::MAX);
    Span::new(file, start, end)
}

/// State of one [`Literals::string_text`] call.
struct TextScanner<'a, 'd> {
    src: &'a str,
    file: FileId,
    /// Byte offset of the next unread character.
    pos: usize,
    diags: &'d mut Vec<Diagnostic>,
}

impl TextScanner<'_, '_> {
    /// Reads text up to and including the stop, and says which stop it was.
    fn run(&mut self) -> TextStop {
        loop {
            let at = self.pos;
            let Some(ch) = self.peek() else {
                return TextStop::LineEnd;
            };
            match ch {
                '\n' => return TextStop::LineEnd,
                '\r' if self.peek_second() == Some('\n') => return TextStop::LineEnd,
                '"' => {
                    self.pos += 1;
                    return TextStop::Quote;
                }
                '{' => {
                    self.pos += 1;
                    return TextStop::Brace;
                }
                '\\' => self.escape(),
                '}' => {
                    self.pos += 1;
                    self.error(
                        codes::LONE_CLOSE_BRACE,
                        at,
                        codes::MSG_LONE_CLOSE_BRACE.to_string(),
                    );
                }
                '\r' => {
                    // A lone carriage return is not a line end (D68).
                    self.pos += 1;
                    self.error(codes::UNEXPECTED_CHAR, at, codes::msg_unexpected_char(ch));
                }
                ch if codes::is_bidi_control(ch) => {
                    self.pos += ch.len_utf8();
                    self.error(codes::BIDI_CONTROL, at, codes::msg_bidi_control(ch));
                }
                ch => self.pos += ch.len_utf8(),
            }
        }
    }

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

    /// Reports a fault that spans from `start` to the current position.
    fn error(&mut self, code: Code, start: usize, message: String) {
        let span = span_of(self.file, start, self.pos);
        self.diags.push(Diagnostic::error(code, span, message));
    }

    /// Reads one escape. The backslash is the next character.
    ///
    /// A backslash never consumes a line end or a lone carriage return: the caller then
    /// stops at the line end (the driver reports the unterminated string) or reports the
    /// carriage return, so one fault gives one diagnostic.
    fn escape(&mut self) {
        let start = self.pos;
        match self.peek_second() {
            None | Some('\n' | '\r') => self.pos += 1,
            Some('{' | '}' | '"' | '\\' | 'n' | 't') => self.pos += 2,
            Some('u') => self.unicode_escape(),
            Some(ch) if codes::is_bidi_control(ch) => {
                // The character is the fault; it is named by its code point only.
                let at = start + 1;
                self.pos = at + ch.len_utf8();
                self.error(codes::BIDI_CONTROL, at, codes::msg_bidi_control(ch));
            }
            Some(ch) => {
                self.pos += 1 + ch.len_utf8();
                self.error(
                    codes::BAD_ESCAPE,
                    start,
                    codes::msg_unknown_escape(&format!("\\{ch}")),
                );
            }
        }
    }

    /// Reads `\u{h}` with 1 to 6 hex digits. The backslash is the next character.
    fn unicode_escape(&mut self) {
        let start = self.pos;
        self.pos += 2;
        let mut written = String::from("\\u");
        if self.peek() != Some('{') {
            self.error(
                codes::BAD_ESCAPE,
                start,
                codes::msg_malformed_unicode_escape(&written),
            );
            return;
        }
        self.pos += 1;
        written.push('{');

        let mut digits = 0usize;
        let mut value: u32 = 0;
        while let Some(ch) = self.peek().filter(char::is_ascii_hexdigit) {
            self.pos += 1;
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
            self.pos += 1;
            written.push('}');
        } else if let Some(tail) = self.stray_escape_tail() {
            // `\u{4G}`: the escape visibly ends at the `}`, so it is one malformed escape
            // and the `}` is not reported again as a lone brace.
            written.push_str(tail);
            self.pos += tail.len();
            self.error(
                codes::BAD_ESCAPE,
                start,
                codes::msg_malformed_unicode_escape(&written),
            );
            return;
        }

        if !closed || digits == 0 || digits > 6 {
            self.error(
                codes::BAD_ESCAPE,
                start,
                codes::msg_malformed_unicode_escape(&written),
            );
            return;
        }
        if char::from_u32(value).is_none() {
            self.error(codes::NOT_SCALAR, start, codes::msg_not_scalar(&written));
        }
    }

    /// The rest of a broken `\u{...}` up to and including its `}`, if a `}` follows within
    /// a few characters and before anything that ends or restarts text: a quote, a
    /// backslash, a brace, a line end or any other control character, or a bidirectional
    /// control character. Looks at no more than [`MAX_STRAY_TAIL_CHARS`] characters.
    fn stray_escape_tail(&self) -> Option<&str> {
        let rest = self.src.get(self.pos..)?;
        for (index, ch) in rest.char_indices().take(MAX_STRAY_TAIL_CHARS) {
            match ch {
                '}' => return rest.get(..index + 1),
                '"' | '\\' | '{' => return None,
                ch if ch.is_control() || codes::is_bidi_control(ch) => return None,
                _ => {}
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{TokenKind, lex};
    use ostrel_core::SourceMap;

    const FILE: FileId = FileId::from_raw(0);

    /// A diagnostic as (code, start, end, message), easy to compare.
    type Found = (String, u32, u32, String);

    /// Scans string text of `src` from `start`: end, stop and the diagnostics as
    /// (code, start, end, message).
    fn text_at(src: &str, start: usize) -> (usize, TextStop, Vec<Found>) {
        let mut diags = Vec::new();
        let (end, stop) = Literals.string_text(src, start, FILE, &mut diags);
        (end, stop, flat(&diags))
    }

    /// Scans string text that starts at offset 0, as just after an opening quote.
    fn text(src: &str) -> (usize, TextStop, Vec<Found>) {
        text_at(src, 0)
    }

    fn int(src: &str) -> (usize, Vec<Found>) {
        let mut diags = Vec::new();
        let end = Literals.int(src, 0, FILE, &mut diags);
        (end, flat(&diags))
    }

    fn flat(diags: &[Diagnostic]) -> Vec<Found> {
        diags
            .iter()
            .map(|d| {
                (
                    d.code.to_string(),
                    d.span.start,
                    d.span.end,
                    d.message.clone(),
                )
            })
            .collect()
    }

    fn diag(code: &str, start: u32, end: u32, message: &str) -> Found {
        (code.to_string(), start, end, message.to_string())
    }

    /// The codes of all diagnostics of the full lexer for `src`.
    fn lex_codes(src: &str) -> Vec<String> {
        let (_, diags) = lex(src, FILE);
        diags.iter().map(|d| d.code.to_string()).collect()
    }

    // Integer literals (G9).

    #[test]
    fn int_reads_the_digit_run() {
        assert_eq!(int("1234 + 1"), (4, vec![]));
        assert_eq!(int("0"), (1, vec![]));
        assert_eq!(int("007)"), (3, vec![]));
    }

    #[test]
    fn int_accepts_the_largest_value() {
        assert_eq!(int("9007199254740991"), (16, vec![]));
        assert_eq!(int("0009007199254740991"), (19, vec![]));
    }

    #[test]
    fn int_rejects_one_above_the_range_at_its_start() {
        let message = "integer literal is outside the `Int` range of \
                       -9007199254740991 to 9007199254740991";
        assert_eq!(
            int("9007199254740992"),
            (16, vec![diag("E0010", 0, 16, message)])
        );
        let mut diags = Vec::new();
        assert_eq!(
            Literals.int("x = 18446744073709551616", 4, FILE, &mut diags),
            24
        );
        assert_eq!(flat(&diags), vec![diag("E0010", 4, 24, message)]);
    }

    #[test]
    fn int_with_many_digits_does_not_overflow() {
        let digits = "9".repeat(100_000);
        let (end, diags) = int(&digits);
        assert_eq!(end, 100_000);
        assert_eq!(diags.len(), 1);
    }

    // Plain text and the three stops.

    #[test]
    fn text_stops_after_the_closing_quote() {
        assert_eq!(text("hello\" rest"), (6, TextStop::Quote, vec![]));
        assert_eq!(text("\""), (1, TextStop::Quote, vec![]));
    }

    #[test]
    fn text_stops_after_an_opening_brace() {
        assert_eq!(text("a {x}\""), (3, TextStop::Brace, vec![]));
        // `{{` has no special meaning: the first brace opens the interpolation.
        assert_eq!(text("{{x}}\""), (1, TextStop::Brace, vec![]));
    }

    #[test]
    fn text_starts_at_the_given_offset() {
        let src = "let s = \"a{x} b\"\n";
        assert_eq!(text_at(src, 9), (11, TextStop::Brace, vec![]));
        assert_eq!(text_at(src, 13), (16, TextStop::Quote, vec![]));
    }

    #[test]
    fn text_stops_before_a_line_end() {
        assert_eq!(text("hello\n  print(s)\n"), (5, TextStop::LineEnd, vec![]));
        assert_eq!(text("abc"), (3, TextStop::LineEnd, vec![]));
        assert_eq!(text(""), (0, TextStop::LineEnd, vec![]));
    }

    #[test]
    fn text_stops_before_crlf() {
        assert_eq!(text("ab\r\nx\""), (2, TextStop::LineEnd, vec![]));
        let (tokens, diags) = lex("x = \"ab\r\ny = 1\r\n", FILE);
        assert_eq!(
            flat(&diags),
            vec![diag("E0003", 4, 5, "unterminated string literal")]
        );
        let error = tokens.iter().find(|t| t.kind == TokenKind::Error);
        assert_eq!(error.map(|t| (t.span.start, t.span.end)), Some((4, 7)));
    }

    #[test]
    fn lone_carriage_return_in_text_is_e0008() {
        assert_eq!(
            text("a\rb\""),
            (
                4,
                TextStop::Quote,
                vec![diag("E0008", 1, 2, "unexpected character U+000D")]
            )
        );
    }

    #[test]
    fn text_keeps_non_ascii_and_invisible_characters() {
        let src = "grüße \u{200B}\u{200D}\u{2060}\u{FEFF}\u{0}\"";
        assert_eq!(text(src), (src.len(), TextStop::Quote, vec![]));
    }

    // Escapes.

    #[test]
    fn every_valid_escape_is_text() {
        let src = r#"\{\}\"\\\n\t\u{41}\u{1F600}\u{0}\u{202e}\u{00E9}\u{10FFFF}""#;
        assert_eq!(text(src), (src.len(), TextStop::Quote, vec![]));
    }

    #[test]
    fn escaped_braces_and_quotes_never_stop_the_text() {
        assert_eq!(text(r#"\{x\} \" y""#), (11, TextStop::Quote, vec![]));
        // The braces of `\u{...}` never open an interpolation.
        assert_eq!(text(r"\u{41}{"), (7, TextStop::Brace, vec![]));
    }

    #[test]
    fn unknown_escape_is_e0004_at_the_backslash() {
        assert_eq!(
            text(r#"a \q b""#),
            (
                7,
                TextStop::Quote,
                vec![diag("E0004", 2, 4, "unknown escape sequence `\\q`")]
            )
        );
        for escape in [r"\r", r"\0", r"\'", r"\ ", r"\é"] {
            let (_, stop, diags) = text(&format!("{escape}\""));
            assert_eq!(stop, TextStop::Quote, "{escape}");
            assert_eq!(diags.len(), 1, "{escape}");
            assert_eq!(
                diags.first().map(|d| d.0.as_str()),
                Some("E0004"),
                "{escape}"
            );
        }
    }

    #[test]
    fn malformed_unicode_escapes_are_e0004() {
        let cases = [
            (r#"a \u{} b""#, "\\u{}"),
            (r#"a \u{0000041} b""#, "\\u{0000041}"),
            (r#"a \u{41 b""#, "\\u{41"),
            (r#"a \u41 b""#, "\\u"),
            (r#"a \u{4G} b""#, "\\u{4G}"),
            (r#"a \u{123456789ABC} b""#, "\\u{12345678...}"),
        ];
        for (src, written) in cases {
            let (end, stop, diags) = text(src);
            assert_eq!((end, stop), (src.len(), TextStop::Quote), "{src}");
            let message =
                format!("malformed escape `{written}`; write 1 to 6 hex digits between the braces");
            assert_eq!(diags.len(), 1, "{src}");
            assert_eq!(
                diags.first().map(|d| (d.0.as_str(), d.1, d.3.as_str())),
                Some(("E0004", 2, message.as_str())),
                "{src}"
            );
        }
    }

    #[test]
    fn unclosed_unicode_escape_stops_at_the_quote() {
        // The quote ends the text; it is not swallowed by the escape.
        let (end, stop, diags) = text("\\u{41\"");
        assert_eq!((end, stop), (6, TextStop::Quote));
        assert_eq!(diags.len(), 1);
        let (end, stop, diags) = text("\\u{41\r\n");
        assert_eq!((end, stop), (5, TextStop::LineEnd));
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn non_scalar_escapes_are_e0005() {
        for (src, written) in [
            (r#"\u{110000} end""#, "\\u{110000}"),
            (r#"\u{D800}""#, "\\u{D800}"),
            (r#"\u{dfff}""#, "\\u{dfff}"),
            (r#"\u{FFFFFF}""#, "\\u{FFFFFF}"),
        ] {
            let message = format!("`{written}` is not a Unicode scalar value");
            let end = u32::try_from(written.len()).unwrap_or(0);
            assert_eq!(
                text(src),
                (
                    src.len(),
                    TextStop::Quote,
                    vec![diag("E0005", 0, end, &message)]
                ),
                "{src}"
            );
        }
    }

    #[test]
    fn backslash_never_consumes_a_line_end() {
        assert_eq!(text("abc\\\nx\""), (4, TextStop::LineEnd, vec![]));
        assert_eq!(text("abc\\\r\nx\""), (4, TextStop::LineEnd, vec![]));
        assert_eq!(text("\\"), (1, TextStop::LineEnd, vec![]));
        // A lone carriage return after a backslash is reported once, as E0008.
        let (end, stop, diags) = text("\\\rx\"");
        assert_eq!((end, stop), (4, TextStop::Quote));
        assert_eq!(flat_codes(&diags), vec!["E0008"]);
    }

    #[test]
    fn every_independent_fault_is_reported() {
        let (_, _, diags) = text(r#"\q \u{} \u{D800} } x""#);
        let found: Vec<_> = diags.iter().map(|d| (d.0.as_str(), d.1)).collect();
        assert_eq!(
            found,
            vec![("E0004", 0), ("E0004", 3), ("E0005", 8), ("E0012", 17)]
        );
    }

    fn flat_codes(diags: &[Found]) -> Vec<&str> {
        diags.iter().map(|d| d.0.as_str()).collect()
    }

    // Lone `}` (E0012) and bidirectional control characters (E0011).

    #[test]
    fn lone_close_brace_is_e0012() {
        assert_eq!(
            text(r#"a } b""#),
            (
                6,
                TextStop::Quote,
                vec![diag(
                    "E0012",
                    2,
                    3,
                    "`}` in a string literal must be written as `\\}`"
                )]
            )
        );
        // After an interpolation the text starts behind its `}`; a second `}` is lone.
        assert_eq!(lex_codes("x = \"{a}}\"\n"), vec!["E0012"]);
    }

    #[test]
    fn raw_bidi_control_in_text_is_e0011_once_each() {
        let (end, stop, diags) = text("a\u{202E}b\u{2066}\"");
        assert_eq!((end, stop), (9, TextStop::Quote));
        assert_eq!(
            diags,
            vec![
                diag("E0011", 1, 4, &codes::msg_bidi_control('\u{202E}')),
                diag("E0011", 5, 8, &codes::msg_bidi_control('\u{2066}')),
            ]
        );
        assert_eq!(
            codes::msg_bidi_control('\u{202E}'),
            "bidirectional control character U+202E is not allowed in source; \
             write `\\u{202E}` inside a string if it is needed"
        );
        // After a backslash the character is reported once, as E0011 only.
        let (_, _, diags) = text("\\\u{2067}\"");
        assert_eq!(
            diags,
            vec![diag("E0011", 1, 4, &codes::msg_bidi_control('\u{2067}'))]
        );
        // Inside an interpolation the driver reports it.
        assert_eq!(lex_codes("x = \"{a \u{2068}}\"\n"), vec!["E0011"]);
    }

    // The full lexer with this scanner.

    #[test]
    fn lexer_splits_text_and_interpolation() {
        let src = "x = \"a {y + 1} b {z}\"\n";
        let (tokens, diags) = lex(src, FILE);
        assert!(diags.is_empty(), "{diags:?}");
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Ident,
                TokenKind::Eq,
                TokenKind::StrHead,
                TokenKind::Ident,
                TokenKind::Plus,
                TokenKind::Int,
                TokenKind::StrMid,
                TokenKind::Ident,
                TokenKind::StrTail,
                TokenKind::Nl,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lexer_reports_string_in_interpolation_at_the_inner_quote() {
        // Review case: `print("a {x")` is E0006 at the inner quote, as in the driver.
        let (_, diags) = lex("print(\"a {x\")\n", FILE);
        assert_eq!(
            flat(&diags).first().map(|d| (d.0.as_str(), d.1)),
            Some(("E0006", 11))
        );
        assert_eq!(diags.len(), 1);
    }

    /// Every lexer golden about literals, run through the real lexer and compared byte
    /// for byte with its expected output.
    #[test]
    fn literal_goldens_match_byte_for_byte() {
        let goldens = [
            (
                "lex_escape_empty_unicode",
                include_str!("../../../../tests/errors/lex_escape_empty_unicode.ostl"),
                include_str!("../../../../tests/errors/lex_escape_empty_unicode.expected_err"),
            ),
            (
                "lex_escape_not_scalar",
                include_str!("../../../../tests/errors/lex_escape_not_scalar.ostl"),
                include_str!("../../../../tests/errors/lex_escape_not_scalar.expected_err"),
            ),
            (
                "lex_escape_surrogate",
                include_str!("../../../../tests/errors/lex_escape_surrogate.ostl"),
                include_str!("../../../../tests/errors/lex_escape_surrogate.expected_err"),
            ),
            (
                "lex_escape_unicode_too_long",
                include_str!("../../../../tests/errors/lex_escape_unicode_too_long.ostl"),
                include_str!("../../../../tests/errors/lex_escape_unicode_too_long.expected_err"),
            ),
            (
                "lex_unknown_escape",
                include_str!("../../../../tests/errors/lex_unknown_escape.ostl"),
                include_str!("../../../../tests/errors/lex_unknown_escape.expected_err"),
            ),
            (
                "lex_int_literal_too_large",
                include_str!("../../../../tests/errors/lex_int_literal_too_large.ostl"),
                include_str!("../../../../tests/errors/lex_int_literal_too_large.expected_err"),
            ),
            (
                "lex_interpolation_too_deep",
                include_str!("../../../../tests/errors/lex_interpolation_too_deep.ostl"),
                include_str!("../../../../tests/errors/lex_interpolation_too_deep.expected_err"),
            ),
            (
                "lex_string_in_interpolation",
                include_str!("../../../../tests/errors/lex_string_in_interpolation.ostl"),
                include_str!("../../../../tests/errors/lex_string_in_interpolation.expected_err"),
            ),
            (
                "lex_unterminated_string",
                include_str!("../../../../tests/errors/lex_unterminated_string.ostl"),
                include_str!("../../../../tests/errors/lex_unterminated_string.expected_err"),
            ),
        ];
        for (name, src, expected) in goldens {
            let mut map = SourceMap::new();
            let path = format!("tests/errors/{name}.ostl");
            let Ok(file) = map.add(&path, src) else {
                unreachable!("golden sources are small");
            };
            let (_, diags) = lex(src, file);
            let rendered: String = diags.iter().map(|d| d.render(&map) + "\n").collect();
            assert_eq!(rendered, expected, "{name}");
        }
    }

    // Hostile input (AC-04, AC-36): linear time, valid spans, no panic.

    #[test]
    fn hostile_interpolations_stay_linear() {
        // The two probes of the review of 18b7503, which took 22 s and 5.6 s before.
        let probes = [
            format!("\"{{{}", "\"x\" ".repeat(300_000)),
            format!("\"{}", "{\"".repeat(300_000)),
            format!("\"{}", "\\u{".repeat(300_000)),
            format!("\"{}\"", "}".repeat(300_000)),
            format!("\"{}\"", "a\r".repeat(300_000)),
        ];
        for src in &probes {
            let (tokens, diags) = lex(src, FILE);
            assert_eq!(tokens.last().map(|t| t.kind), Some(TokenKind::Eof));
            assert!(diags.len() <= src.len());
        }
    }

    #[test]
    fn hostile_text_never_panics_or_breaks_the_contract() {
        let samples = [
            "\"",
            "\\",
            "\\u",
            "\\u{",
            "\\u{1",
            "{",
            "}",
            "\r",
            "\r\n",
            "\\\r",
            "\u{202E}",
            "\\\u{202E}",
            "é\\é",
            "\\u{é}",
            "\\u{1\u{202E}}",
            "\\u{41}\u{FEFF}",
        ];
        for sample in samples {
            for start in (0..=sample.len()).filter(|&i| sample.is_char_boundary(i)) {
                let mut diags = Vec::new();
                let (end, stop) = Literals.string_text(sample, start, FILE, &mut diags);
                assert!(end >= start && end <= sample.len(), "{sample:?}");
                assert!(sample.is_char_boundary(end), "{sample:?}");
                let last = end.checked_sub(1).and_then(|i| sample.as_bytes().get(i));
                match stop {
                    TextStop::Quote => assert_eq!(last, Some(&b'"'), "{sample:?}"),
                    TextStop::Brace => assert_eq!(last, Some(&b'{'), "{sample:?}"),
                    TextStop::LineEnd => {
                        let rest = sample.get(end..).unwrap_or("");
                        assert!(
                            rest.is_empty() || rest.starts_with('\n') || rest.starts_with("\r\n"),
                            "{sample:?}"
                        );
                    }
                }
                for d in &diags {
                    assert!(d.span.start <= d.span.end && d.span.end as usize <= end);
                }
                let _ = Literals.int(sample, start, FILE, &mut diags);
            }
        }
    }
}
