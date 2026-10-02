//! Shared compiler foundations: source files, spans and diagnostics.
//!
//! F1a contract (ARCHITECTURE 15, CORE-1): [`FileId`], [`Span`], [`SourceMap`]
//! with line and column from byte offsets, and [`Diagnostic`] with its renderer
//! `file:line:column: error[E####]: message` (SPEC 12.1).

// Spans and messages come from untrusted input: no unwrap, expect, panic or
// unchecked indexing outside of tests.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod diagnostic;
mod source_map;
mod span;

pub use diagnostic::{Code, Diagnostic, Severity};
pub use source_map::{AddError, Location, MAX_FILE_BYTES, SourceMap};
pub use span::{FileId, Span};
