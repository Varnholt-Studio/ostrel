//! Shared compiler foundations: source files, spans and diagnostics.
//!
//! F1a contract (ARCHITECTURE 15, CORE-1): [`FileId`], [`Span`], [`SourceMap`]
//! with line and column from byte offsets, and [`Diagnostic`] with its renderer
//! `file:line:column: error[E####]: message` (SPEC 12.1).
//!
//! F1b contract (ARCHITECTURE 5.3, F1B-CORE): [`ids`] for replica, op, row and log ids and the
//! hybrid logical clock, [`value`] for the persisted and wire value and the key order of D61,
//! and [`canon`] for the canonical JSON writer.

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
mod f1b;
mod source_map;
mod span;

pub use diagnostic::{Code, Diagnostic, Severity};
pub use f1b::{canon, ids, value};
pub use source_map::{AddError, Location, MAX_FILE_BYTES, SourceMap};
pub use span::{FileId, Span};
