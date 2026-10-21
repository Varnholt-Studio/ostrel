//! The official formatter of the Ostrel programming language (`ostrel fmt`,
//! ARCHITECTURE 3.3, SYNTAX 3, G10).
//!
//! The formatter works on the token stream of the lexer, comments included,
//! and keeps every token: it only decides the white space between them. It
//! never joins lines, so the line structure of the source survives; within
//! that structure the output is canonical:
//!
//! * two spaces per block level, taken from the `INDENT` and `DEDENT` tokens;
//! * a continuation line inside an open bracket is one level deeper than the
//!   line that opened the bracket, and a line that starts with the closing
//!   bracket stands at the column of that line (SYNTAX 3);
//! * one space or none between two tokens of a line, by fixed rules
//!   ([`space`]);
//! * a comment on a line of its own keeps its block level within the levels
//!   the lexer allows there;
//! * at most one blank line in a row, none at the start or the end;
//! * `\n` line ends, no byte order mark, no trailing blanks, one final `\n`.
//!
//! Lines inside a `style` body keep the gaps between their CSS pieces as they
//! are (none or one space). Breaking lines over 100 columns (G10) and the CSS
//! rules of D25 are not done yet; such lines are reported in
//! [`Formatted::long_lines`] and left as they are.
//!
//! Safety net: the output is lexed again and must give the same token stream
//! as the input, apart from the white space and line ends the formatter owns
//! (kinds and text of every token, `NL`, `INDENT` and `DEDENT` included).
//! Since the parser reads nothing else, the syntax tree of the output equals
//! the syntax tree of the input apart from spans (AC-48). A formatter defect
//! that would change the program is a [`Refusal::Defect`], never output.

// This crate handles untrusted input: no unwrap, expect, panic or unchecked indexing.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod print;
pub mod space;

use std::hash::{DefaultHasher, Hash, Hasher};

use ostrel_core::{Diagnostic, FileId};
use ostrel_syntax::lex::{self, Token, TokenKind};

/// The line width of formatted code (D14).
pub const WIDTH: usize = 100;

/// Spaces per block level (SYNTAX 3).
pub const INDENT: usize = 2;

/// The result of formatting one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Formatted {
    /// The formatted source text.
    pub text: String,
    /// Line numbers (1 based) of the lines of [`Formatted::text`] that are
    /// wider than [`WIDTH`] columns. A column is one Unicode scalar value.
    pub long_lines: Vec<u32>,
}

/// Why a file was not formatted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The source does not lex or parse. These are the error diagnostics of
    /// the lexer and the parser, as `ostrel check` reports them.
    Invalid(Vec<Diagnostic>),
    /// The formatter produced text that would not give back the same program.
    /// This is a defect of the formatter; the reason names the failed check.
    Defect(&'static str),
}

/// Formats the source text of one file.
///
/// The file is lexed and parsed first; a file with any error diagnostic is
/// not formatted ([`Refusal::Invalid`]), because the formatter never guesses
/// at the meaning of broken code.
pub fn format_source(src: &str, file: FileId) -> Result<Formatted, Refusal> {
    let (tokens, lexed) = lex::lex(src, file);
    let errors = {
        let (module, parsed) = ostrel_syntax::parse::parse(src, file, &tokens, &lexed);
        // The tree is not needed: the formatter reads only tokens. Freeing it
        // before printing keeps the peak memory at that of `ostrel check`.
        drop(module);
        let mut errors: Vec<Diagnostic> = lexed.into_iter().filter(Diagnostic::is_error).collect();
        errors.extend(parsed.into_iter().filter(Diagnostic::is_error));
        errors
    };
    if !errors.is_empty() {
        return Err(Refusal::Invalid(errors));
    }
    let printed = print::print(src, &tokens);
    let expected = digest(src, &tokens);
    drop(tokens);
    verify(printed, expected, file)
}

/// Formats `src` from its tokens, as [`lex::lex`] returned them.
///
/// The caller has checked that `src` lexes and parses without an error
/// diagnostic. Tokens that do not belong to `src` give a
/// [`Refusal::Defect`] or text that does not round trip, never a panic.
pub fn format(src: &str, file: FileId, tokens: &[Token]) -> Result<Formatted, Refusal> {
    let printed = print::print(src, tokens);
    verify(printed, digest(src, tokens), file)
}

/// Lexes the output again and compares its token stream with the input's.
fn verify(printed: print::Printed, expected: Digest, file: FileId) -> Result<Formatted, Refusal> {
    let (tokens, diags) = lex::lex(&printed.text, file);
    if diags.iter().any(Diagnostic::is_error) {
        return Err(Refusal::Defect("the formatted text does not lex"));
    }
    if digest(&printed.text, &tokens) != expected {
        return Err(Refusal::Defect("the formatted text has other tokens"));
    }
    Ok(Formatted {
        text: printed.text,
        long_lines: printed.long_lines,
    })
}

/// A fingerprint of a token stream: the number of tokens and a hash of their
/// kinds and of the text of every token that has text of its own. The text of
/// `NL`, `INDENT` and `DEDENT` is white space, which the formatter owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Digest {
    count: usize,
    hash: u64,
}

fn digest(src: &str, tokens: &[Token]) -> Digest {
    let mut h = DefaultHasher::new();
    for tok in tokens {
        tok.kind.hash(&mut h);
        match tok.kind {
            TokenKind::Nl | TokenKind::Indent | TokenKind::Dedent | TokenKind::Eof => {}
            TokenKind::Comment => {
                for line in comment_lines(text(src, *tok)) {
                    line.hash(&mut h);
                }
            }
            _ => text(src, *tok).hash(&mut h),
        }
    }
    Digest {
        count: tokens.len(),
        hash: h.finish(),
    }
}

/// The source text of a token; empty if its span is not inside `src`.
fn text(src: &str, tok: Token) -> &str {
    src.get(tok.span.start as usize..tok.span.end as usize)
        .unwrap_or("")
}

/// The lines of a comment as the formatter writes them: without trailing
/// blanks and carriage returns. A `/* */` comment of a `style` body may span
/// lines; a `//` comment is one line.
fn comment_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n').map(str::trim_end)
}

#[cfg(test)]
mod tests;
