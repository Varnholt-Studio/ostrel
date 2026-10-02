//! Diagnostic codes and messages of the parser (E0020 to E0099) and the
//! parser's use of E0100.
//!
//! The single catalog of codes is `tests/errors/README.md` (SPEC 12.1). The
//! constants here mirror it, so the statement parser in `mod.rs` and the
//! expression parser use the same numbers and texts.

use ostrel_core::Code;

use crate::lex::Keyword;

/// `+=`, `-=`, `*=`, `/=`, `%=` or `:=` (SYNTAX 9).
pub const ASSIGN_OPERATOR: Code = Code::new(20);
/// `is` used as an operator (SYNTAX 9, G3).
pub const IS_OPERATOR: Code = Code::new(21);
/// `{` opening a block (SYNTAX 2 P1).
pub const BRACE_BLOCK: Code = Code::new(22);
/// `:` at the end of a block head (SYNTAX 2 P1).
pub const COLON_BLOCK_HEAD: Code = Code::new(23);
/// `else if` on one line (SPEC 12.2).
pub const ELSE_IF_ONE_LINE: Code = Code::new(24);
/// `(` without its `)` before the end of the logical line.
pub const UNCLOSED_PAREN: Code = Code::new(25);
/// Keyword where a name is expected (G4).
pub const KEYWORD_AS_NAME: Code = Code::new(26);
/// Comparison operators chained (SYNTAX 7 `cmpExpr`).
pub const COMPARISON_CHAIN: Code = Code::new(27);
/// A token that the grammar does not allow at this place.
///
/// ASSUMPTION: E0028 is the next free parser code. It enters the catalog with
/// its first golden.
pub const EXPECTED: Code = Code::new(28);
/// Brackets and blocks nested deeper than the nesting limit (ARCHITECTURE 3.4).
///
/// ASSUMPTION: code number as for [`EXPECTED`].
pub const NESTING_TOO_DEEP: Code = Code::new(29);
/// Syntax tree higher than the height limit (D54).
///
/// ASSUMPTION: code number as for [`EXPECTED`].
pub const TREE_TOO_HIGH: Code = Code::new(30);
/// More syntax tree nodes in one file than the node limit (D54).
///
/// ASSUMPTION: code number as for [`EXPECTED`].
pub const TOO_MANY_NODES: Code = Code::new(31);
/// An indented line that no block head opens.
///
/// ASSUMPTION: code number as for [`EXPECTED`].
pub const UNEXPECTED_INDENT: Code = Code::new(32);
/// A construct of the grammar outside the v0.1 slice (ARCHITECTURE 3.4).
pub const NOT_IN_V0_1: Code = Code::new(100);

/// Message of [`ASSIGN_OPERATOR`] for `+=` and its relatives.
pub const MSG_COMPOUND_ASSIGN: &str = "Ostrel assigns with `=`; compound assignment does not exist";
/// Message of [`ASSIGN_OPERATOR`] for `:=`.
pub const MSG_WALRUS_ASSIGN: &str =
    "Ostrel assigns with `=`; `:=` does not exist, declare a new name with `let`";
/// Message of [`IS_OPERATOR`].
pub const MSG_IS_OPERATOR: &str = "`is` is not Ostrel; to unwrap an optional write `if let y = x`";
/// Message of [`BRACE_BLOCK`].
pub const MSG_BRACE_BLOCK: &str = "blocks use indentation, not braces; remove `{`";
/// Message of [`COLON_BLOCK_HEAD`].
pub const MSG_COLON_BLOCK_HEAD: &str = "block heads have no `:`; remove it and indent the block";
/// Message of [`ELSE_IF_ONE_LINE`].
pub const MSG_ELSE_IF_ONE_LINE: &str =
    "`else` ends its line; put the `if` on the next line and indent it";
/// Message of [`UNCLOSED_PAREN`].
pub const MSG_UNCLOSED_PAREN: &str = "unclosed `(`";
/// Message of [`COMPARISON_CHAIN`].
pub const MSG_COMPARISON_CHAIN: &str = "comparisons do not chain; write `a < b and b < c`";
/// Message of [`UNEXPECTED_INDENT`].
pub const MSG_UNEXPECTED_INDENT: &str =
    "unexpected indentation; only a block head such as `fn`, `if` or `else` opens a block";

/// Message of [`KEYWORD_AS_NAME`] for the keyword `kw`.
pub fn msg_keyword_as_name(kw: Keyword) -> String {
    format!(
        "`{}` is a keyword and cannot be used as a name",
        kw.as_str()
    )
}

/// Message of [`EXPECTED`]: what the grammar wants and what the source has.
pub fn msg_expected(expected: &str, found: &str) -> String {
    format!("expected {expected}, found {found}")
}

/// Message of [`NESTING_TOO_DEEP`] for the limit `limit`.
pub fn msg_nesting_too_deep(limit: u32) -> String {
    format!("brackets and blocks nest deeper than {limit} levels")
}

/// Message of [`TREE_TOO_HIGH`] for the limit `limit`.
pub fn msg_tree_too_high(limit: u32) -> String {
    format!(
        "expression is too deeply nested: the syntax tree may be at most {limit} levels high; \
         split it with `let`"
    )
}

/// Message of [`TOO_MANY_NODES`] for the limit `limit`.
pub fn msg_too_many_nodes(limit: u32) -> String {
    format!("file is too large: it has more than {limit} syntax tree nodes")
}

/// Message of [`NOT_IN_V0_1`] for the construct `what`.
pub fn msg_not_in_v0_1(what: &str) -> String {
    format!("{what} is not available in v0.1")
}
