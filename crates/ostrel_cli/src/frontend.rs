//! The one place where the CLI calls the parser (WP T1-3, T1-3b).
//!
//! The parser is not part of this build yet. Until it is, [`parse`] reports
//! [`ParserMissing`] and the pipeline ends with an internal error for every
//! file the lexer accepts. Once `ostrel_syntax::parse` exists, the body of
//! [`parse`] becomes a single call to it and nothing else in the CLI changes.

use ostrel_core::{Diagnostic, FileId};
use ostrel_syntax::ast::Module;
use ostrel_syntax::lex::Token;

/// The parser is not available in this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParserMissing;

/// Parses the tokens of one file.
///
/// `lexed` holds the diagnostics the lexer reported for `tokens`, so the
/// parser can keep to one diagnostic per fault (SPEC 12.1).
pub fn parse(
    src: &str,
    file: FileId,
    tokens: &[Token],
    lexed: &[Diagnostic],
) -> Result<(Module, Vec<Diagnostic>), ParserMissing> {
    let _ = (src, file, tokens, lexed);
    Err(ParserMissing)
}
