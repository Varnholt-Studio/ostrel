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
/// A control character such as a lone carriage return is named by its code
/// point, so the message never carries an invisible character.
pub fn msg_unexpected_char(c: char) -> String {
    if c.is_control() {
        format!("unexpected character U+{:04X}", u32::from(c))
    } else {
        format!("unexpected character `{c}`")
    }
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

/// Message of [`BAD_ESCAPE`] for an escape that does not exist, as written.
pub fn msg_unknown_escape(escape: &str) -> String {
    format!("unknown escape sequence `{escape}`")
}

/// Message of [`BAD_ESCAPE`] for a malformed `\u{...}` escape, as written.
pub fn msg_malformed_unicode_escape(escape: &str) -> String {
    format!("malformed escape `{escape}`; write 1 to 6 hex digits between the braces")
}

/// Message of [`NOT_SCALAR`] for a `\u{...}` escape, as written.
pub fn msg_not_scalar(escape: &str) -> String {
    format!("`{escape}` is not a Unicode scalar value")
}

/// True for the nine bidirectional control characters of D53:
/// U+202A to U+202E and U+2066 to U+2069.
pub const fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}
