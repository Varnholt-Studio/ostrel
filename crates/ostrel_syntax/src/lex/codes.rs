//! Diagnostic codes and messages of the lexer (E0001 to E0019).
//!
//! The single catalog of codes is `tests/errors/README.md` (SPEC 12.1). The
//! constants here mirror it, so the driver and the literal scanner use the same
//! numbers and texts.

use ostrel_core::Code;

/// Tab in indentation.
pub const TAB_INDENT: Code = Code::new(1);
/// Indentation that is not a multiple of two spaces.
pub const ODD_INDENT: Code = Code::new(2);
/// String literal without its closing quote on the same line.
pub const UNTERMINATED_STRING: Code = Code::new(3);
/// Unknown or malformed escape sequence.
pub const BAD_ESCAPE: Code = Code::new(4);
/// `\u{...}` escape whose value is not a Unicode scalar value.
pub const NOT_SCALAR: Code = Code::new(5);
/// String literal inside an interpolation.
pub const STRING_IN_INTERPOLATION: Code = Code::new(6);
/// Interpolation braces nested deeper than [`MAX_INTERPOLATION_DEPTH`].
pub const INTERPOLATION_TOO_DEEP: Code = Code::new(7);
/// Character that cannot start a token.
pub const UNEXPECTED_CHAR: Code = Code::new(8);
/// `;` between statements.
pub const SEMICOLON: Code = Code::new(9);
/// Integer literal outside the `Int` range (G9).
pub const INT_OUT_OF_RANGE: Code = Code::new(10);
/// Raw bidirectional control character (D53, D68).
pub const BIDI_CONTROL: Code = Code::new(11);
/// `}` in string text outside an interpolation (D68).
pub const LONE_CLOSE_BRACE: Code = Code::new(12);
/// Indentation more than one level deeper than the line above (D68).
pub const DEEP_INDENT: Code = Code::new(13);
/// Source file that is not valid UTF-8, reported at the first invalid byte
/// (D84, SPEC 12.6). The lexer reads `&str` and never sees such a file; the
/// caller that decodes the bytes reports it with [`msg_invalid_utf8`].
pub const INVALID_UTF8: Code = Code::new(14);

/// Deepest accepted interpolation brace depth (G1, SPEC 12.1).
pub const MAX_INTERPOLATION_DEPTH: u32 = 32;

/// Message of [`TAB_INDENT`].
pub const MSG_TAB_INDENT: &str = "tab in indentation; indent with two spaces";
/// Message of [`ODD_INDENT`].
pub const MSG_ODD_INDENT: &str = "indentation must be a multiple of two spaces";
/// Message of [`UNTERMINATED_STRING`].
pub const MSG_UNTERMINATED_STRING: &str = "unterminated string literal";
/// Message of [`STRING_IN_INTERPOLATION`].
pub const MSG_STRING_IN_INTERPOLATION: &str =
    "string literal inside interpolation; bind it with `let` first";
/// Message of [`INTERPOLATION_TOO_DEEP`].
pub const MSG_INTERPOLATION_TOO_DEEP: &str = "interpolation nests braces deeper than 32";
/// Message of [`SEMICOLON`].
pub const MSG_SEMICOLON: &str = "`;` is not used in Ostrel; write one statement per line";
/// Message of [`INT_OUT_OF_RANGE`].
pub const MSG_INT_OUT_OF_RANGE: &str =
    "integer literal is outside the `Int` range of -9007199254740991 to 9007199254740991";

/// Message of [`LONE_CLOSE_BRACE`] (ARCHITECTURE 3.5).
pub const MSG_LONE_CLOSE_BRACE: &str = "`}` in a string literal must be written as `\\}`";
/// Message of [`DEEP_INDENT`] (ARCHITECTURE 3.5).
pub const MSG_DEEP_INDENT: &str =
    "indentation is more than one level deeper than the line above; indent a block by two spaces";

/// Message of [`UNEXPECTED_CHAR`] for the character `c`.
///
/// Only printable ASCII (U+0021 to U+007E) is written raw between backticks.
/// Every other character, such as a lone carriage return, U+FEFF, a zero width
/// space or a letter that looks like ASCII, is named by its code point, so the
/// message never carries an invisible or confusable character (SPEC 12.6).
pub fn msg_unexpected_char(c: char) -> String {
    if matches!(c, '\u{21}'..='\u{7E}') {
        format!("unexpected character `{c}`")
    } else {
        format!("unexpected character {}", code_point(c))
    }
}

/// `c` as `U+` followed by its code point in upper case hex, at least four digits.
fn code_point(c: char) -> String {
    format!("U+{:04X}", u32::from(c))
}

/// Source text as a lexer message quotes it (SPEC 12.6): every character outside
/// U+0020 to U+007E is replaced in its place by its code point, so `\` followed by
/// U+200B is quoted as `\U+200B`.
pub fn quote_source(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\u{20}'..='\u{7E}') {
            quoted.push(c);
        } else {
            quoted.push_str(&code_point(c));
        }
    }
    quoted
}

/// Message of [`INVALID_UTF8`] for the first invalid byte `byte` (D84).
///
/// The byte is written as two upper case hex digits, for example
/// `source is not valid UTF-8 (byte 0xFF)`.
pub fn msg_invalid_utf8(byte: u8) -> String {
    format!("source is not valid UTF-8 (byte 0x{byte:02X})")
}

/// Message of [`BIDI_CONTROL`] for the character `c` (ARCHITECTURE 3.5).
///
/// The character is named by its code point, never written raw.
pub fn msg_bidi_control(c: char) -> String {
    let value = u32::from(c);
    format!(
        "bidirectional control character U+{value:04X} is not allowed in source; \
         write `\\u{{{value:X}}}` inside a string if it is needed"
    )
}

/// Message of [`BAD_ESCAPE`] for an escape that does not exist, as written and
/// quoted by [`quote_source`].
pub fn msg_unknown_escape(escape: &str) -> String {
    format!("unknown escape sequence `{}`", quote_source(escape))
}

/// Message of [`BAD_ESCAPE`] for a malformed `\u{...}` escape, as written and
/// quoted by [`quote_source`].
pub fn msg_malformed_unicode_escape(escape: &str) -> String {
    format!(
        "malformed escape `{}`; write 1 to 6 hex digits between the braces",
        quote_source(escape)
    )
}

/// Message of [`NOT_SCALAR`] for a `\u{...}` escape, as written and quoted by
/// [`quote_source`].
pub fn msg_not_scalar(escape: &str) -> String {
    format!("`{}` is not a Unicode scalar value", quote_source(escape))
}

/// True for the nine bidirectional control characters of D53:
/// U+202A to U+202E and U+2066 to U+2069.
pub const fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_ascii_is_written_raw() {
        assert_eq!(msg_unexpected_char('@'), "unexpected character `@`");
        assert_eq!(msg_unexpected_char('!'), "unexpected character `!`");
        assert_eq!(msg_unexpected_char('~'), "unexpected character `~`");
    }

    #[test]
    fn every_other_character_is_named_by_code_point() {
        let cases = [
            ('\r', "U+000D"),
            (' ', "U+0020"),
            ('\u{7F}', "U+007F"),
            ('\u{A0}', "U+00A0"),
            ('\u{FC}', "U+00FC"),
            ('\u{430}', "U+0430"),
            ('\u{200B}', "U+200B"),
            ('\u{2060}', "U+2060"),
            ('\u{FEFF}', "U+FEFF"),
            ('\u{1F600}', "U+1F600"),
            ('\u{10FFFF}', "U+10FFFF"),
        ];
        for (c, name) in cases {
            let msg = msg_unexpected_char(c);
            assert_eq!(msg, format!("unexpected character {name}"));
            assert!(msg.is_ascii(), "{msg:?}");
        }
    }

    #[test]
    fn quoted_source_keeps_only_u0020_to_u007e() {
        assert_eq!(msg_unknown_escape("\\q"), "unknown escape sequence `\\q`");
        assert_eq!(
            msg_unknown_escape("\\\u{200B}"),
            "unknown escape sequence `\\U+200B`"
        );
        assert_eq!(
            msg_unknown_escape("\\\u{430}"),
            "unknown escape sequence `\\U+0430`"
        );
        assert_eq!(
            msg_unknown_escape("\\\u{7F}"),
            "unknown escape sequence `\\U+007F`"
        );
        assert_eq!(quote_source("a b\u{9}\u{FEFF}"), "a bU+0009U+FEFF");
        assert!(msg_malformed_unicode_escape("\\u\u{FC}").is_ascii());
        assert!(msg_not_scalar("\\u{\u{FEFF}}").is_ascii());
    }
}
