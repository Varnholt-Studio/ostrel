//! Lexer, parser and syntax tree of the Ostrel programming language.
//!
//! The lexer is in [`lex`], the syntax tree in [`ast`]. The parser follows
//! according to the v0.1 build plan.
//!
//! The parse limits of D54 live in [`ast`] and are re-exported here, so the
//! parser and its callers name one source for them.

// This crate handles untrusted input: no unwrap, expect, panic or unchecked indexing.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

pub mod ast;
pub mod lex;

pub use ast::{Limits, MAX_HEIGHT, MAX_NODES};
