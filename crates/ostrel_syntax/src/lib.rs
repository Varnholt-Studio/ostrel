//! Lexer, parser and syntax tree of the Ostrel programming language.
//!
//! The syntax tree is in [`ast`]. Lexer and parser follow according to the v0.1
//! build plan.

// This crate handles untrusted input: no unwrap, expect, panic or unchecked indexing.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

pub mod ast;
