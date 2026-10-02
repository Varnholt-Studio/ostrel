//! Compiler diagnostics and their one line rendering (SPEC 12.1, AC-03).

use std::fmt::{self, Write as _};

use crate::source_map::SourceMap;
use crate::span::Span;

/// A compile diagnostic code, rendered as `E` plus four digits.
///
/// The single catalog of codes and their ranges is `tests/errors/README.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Code(u16);

impl Code {
    /// Largest code that renders with four digits.
    pub const MAX: u16 = 9999;

    /// Creates a code. Values above [`Code::MAX`] are clamped to it, so a code
    /// always renders as exactly four digits.
    pub const fn new(number: u16) -> Self {
        if number > Self::MAX {
            Code(Self::MAX)
        } else {
            Code(number)
        }
    }

    /// The numeric value of the code.
    pub const fn number(self) -> u16 {
        self.0
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E{:04}", self.0)
    }
}

/// How severe a diagnostic is. Any `Error` makes the CLI exit with code 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The program is rejected.
    Error,
    /// The program is accepted, for example a formatter warning (G10).
    Warning,
}

impl Severity {
    /// The word used in the rendered line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// One finding of a compiler stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Catalog code.
    pub code: Code,
    /// Error or warning.
    pub severity: Severity,
    /// Where the fault is. Only the start is used for the rendered position.
    pub span: Span,
    /// One sentence, no trailing period required, may quote source text.
    pub message: String,
    /// Optional extra hints. They are not part of the one line form.
    pub notes: Vec<String>,
}

impl Diagnostic {
    /// Creates an error diagnostic without notes.
    pub fn error(code: Code, span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            code,
            severity: Severity::Error,
            span,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    /// Creates a warning diagnostic without notes.
    pub fn warning(code: Code, span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            code,
            severity: Severity::Warning,
            span,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    /// Adds a note and returns the diagnostic.
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// True for [`Severity::Error`].
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// Renders the one line form `PATH:LINE:COLUMN: error[E####]: MESSAGE`,
    /// without a line end.
    ///
    /// Path and message are printed unchanged except for characters that could
    /// break the one line form or mislead a terminal: control characters, line
    /// and paragraph separators and the bidirectional controls of D53 are
    /// written as `\u{XXXX}`. A span whose file is not in `map` renders as
    /// `<unknown>:0:0`.
    pub fn render(&self, map: &SourceMap) -> String {
        let mut out = String::new();
        let (path, line, column) = match (
            map.path(self.span.file),
            map.location(self.span.file, self.span.start),
        ) {
            (Some(path), Some(loc)) => (path, loc.line, loc.column),
            _ => ("<unknown>", 0, 0),
        };
        push_escaped(&mut out, path);
        // Writing to a String cannot fail.
        let _ = write!(
            out,
            ":{line}:{column}: {}[{}]: ",
            self.severity.as_str(),
            self.code
        );
        push_escaped(&mut out, &self.message);
        out
    }

    /// Renders the one line form followed by one line `  note: TEXT` per note,
    /// each terminated by LF.
    pub fn render_with_notes(&self, map: &SourceMap) -> String {
        let mut out = self.render(map);
        out.push('\n');
        for note in &self.notes {
            out.push_str("  note: ");
            push_escaped(&mut out, note);
            out.push('\n');
        }
        out
    }
}

/// True for characters that must not reach the terminal raw.
fn needs_escape(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn push_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        if needs_escape(c) {
            let _ = write!(out, "\\u{{{:04X}}}", u32::from(c));
        } else {
            out.push(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_renders_four_digits() {
        assert_eq!(Code::new(3).to_string(), "E0003");
        assert_eq!(Code::new(100).to_string(), "E0100");
        assert_eq!(Code::new(9999).to_string(), "E9999");
        assert_eq!(Code::new(u16::MAX).to_string(), "E9999");
    }

    #[test]
    fn escapes_terminal_and_bidi_controls() {
        let mut s = String::new();
        push_escaped(
            &mut s,
            "a\nb\u{1b}[31m\u{202E}c\u{7f}\u{85}\u{2028}\tz\u{e9}\u{1F600}\u{200D}",
        );
        assert_eq!(
            s,
            "a\\u{000A}b\\u{001B}[31m\\u{202E}c\\u{007F}\\u{0085}\\u{2028}\\u{0009}z\u{e9}\u{1F600}\u{200D}"
        );
    }
}
