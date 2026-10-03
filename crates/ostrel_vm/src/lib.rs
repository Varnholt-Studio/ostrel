//! Bytecode compiler and virtual machine for the Ostrel programming language.
//!
//! This file fixes the v0.1 contract of the VM (freeze wave F1a, ARCHITECTURE 3.4
//! and 13): the entry point [`run`], the [`Host`] through which the VM reaches the
//! outside world, the [`Limits`] it enforces and the [`RuntimeError`] it reports.
//! [`compile`] turns a validated `ostrel_ir::Program` into a bytecode [`Image`].
//!
//! Frames live on the heap, never on the native stack, so deep recursion in a
//! program ends in `CallDepth` or `HeapLimit`, never in a native stack overflow.

// This crate executes programs derived from untrusted input: no unwrap, expect,
// panic or unchecked indexing.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fmt;

mod bytecode;
mod exec;

pub use bytecode::CompileError;
pub use ostrel_ir::Span;

/// Everything the VM needs from its embedder. The CLI writes to stdout; the
/// server (ARCHITECTURE 4.1) adds database, RPC and clock access by addition.
pub trait Host {
    /// Writes one line of program output; `text` carries no trailing newline.
    fn print(&mut self, text: &str);
}

/// Resource limits of one run. Every limit is checked before the work or the
/// allocation it guards, so exceeding one is a [`RuntimeError`], never an abort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum number of steps (`StepLimit`, flag `--max-steps`). An instruction
    /// costs one step plus one step for every started 64 bytes of text or registers
    /// it handles, so the limit bounds the running time, not only the instruction
    /// count.
    pub max_steps: u64,
    /// Maximum number of frames held at once; `main` is frame 1 (`CallDepth`).
    pub max_frames: u32,
    /// Maximum VM heap size in bytes (`HeapLimit`).
    pub max_heap_bytes: u64,
    /// Maximum size of one `Text` value in UTF 8 bytes (`TextLimit`).
    pub max_text_bytes: u64,
}

impl Default for Limits {
    /// The v0.1 defaults of ARCHITECTURE 3.4 and SPEC 12.4.
    fn default() -> Self {
        Limits {
            max_steps: 100_000_000,
            max_frames: 10_000,
            max_heap_bytes: 256 * 1024 * 1024,
            max_text_bytes: 16 * 1024 * 1024,
        }
    }
}

/// The kinds of runtime error of v0.1 (ARCHITECTURE 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeErrorKind {
    /// An `Int` result left [`ostrel_ir::INT_MIN`, `ostrel_ir::INT_MAX`] (D24).
    IntOverflow,
    /// `/` or `%` with a zero divisor.
    DivisionByZero,
    /// A call would create more frames than [`Limits::max_frames`].
    CallDepth,
    /// More steps than [`Limits::max_steps`].
    StepLimit,
    /// An allocation would grow the heap beyond [`Limits::max_heap_bytes`].
    HeapLimit,
    /// A `Text` would exceed [`Limits::max_text_bytes`].
    TextLimit,
}

impl RuntimeErrorKind {
    /// The name printed in `runtime error[Kind]`.
    pub fn name(self) -> &'static str {
        match self {
            RuntimeErrorKind::IntOverflow => "IntOverflow",
            RuntimeErrorKind::DivisionByZero => "DivisionByZero",
            RuntimeErrorKind::CallDepth => "CallDepth",
            RuntimeErrorKind::StepLimit => "StepLimit",
            RuntimeErrorKind::HeapLimit => "HeapLimit",
            RuntimeErrorKind::TextLimit => "TextLimit",
        }
    }

    /// The fixed message printed after `runtime error[Kind]: ` (ARCHITECTURE 3.4,
    /// D83). One text per kind; no text depends on [`Limits`] or a CLI flag, and the
    /// numbers are the fixed limits of ARCHITECTURE 5.9.
    pub fn message(self) -> &'static str {
        match self {
            RuntimeErrorKind::IntOverflow => {
                "`Int` result is outside the range of -9007199254740991 to 9007199254740991"
            }
            RuntimeErrorKind::DivisionByZero => "division or remainder by zero",
            RuntimeErrorKind::CallDepth => "call would exceed the limit of 10000 frames",
            RuntimeErrorKind::StepLimit => {
                "program exceeded its step limit; raise it with `--max-steps`"
            }
            RuntimeErrorKind::HeapLimit => "VM heap would grow beyond 268435456 bytes",
            RuntimeErrorKind::TextLimit => "`Text` value would be longer than 16777216 bytes",
        }
    }
}

impl fmt::Display for RuntimeErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A runtime error at the source location of the failing instruction. The CLI
/// renders it as `file:line:column: runtime error[Kind]: message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeError {
    /// What went wrong.
    pub kind: RuntimeErrorKind,
    /// Where (ARCHITECTURE 3.4, D83): the failing instruction, the call expression
    /// for `CallDepth`, and for `StepLimit` the call expression in `main` that is
    /// running when the limit is reached (the instruction only while `main` itself
    /// is the running frame).
    pub span: Span,
}

/// Executable bytecode of one program, produced from `ostrel_ir::Program` by
/// [`compile`]. Its contents are private to this crate. The default image holds no
/// program; running it does nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Image {
    code: bytecode::Code,
}

/// Compiles a lowered program to bytecode.
///
/// The IR is validated completely (indexes, operand types, call arity, the shape of
/// `main`), so a malformed program is a [`CompileError`] here and never undefined
/// behaviour or a panic in [`run`]. The checker and the lowering never produce
/// malformed IR, so a `CompileError` always indicates a compiler bug.
pub fn compile(program: &ostrel_ir::Program) -> Result<Image, CompileError> {
    bytecode::compile_program(program).map(|code| Image { code })
}

/// Runs `fn main()` of `image` to completion within `limits`.
///
/// Output goes through `host`. Output written before an error stays written.
pub fn run<H: Host + ?Sized>(
    image: &Image,
    host: &mut H,
    limits: Limits,
) -> Result<(), RuntimeError> {
    exec::run(&image.code, host, limits)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Capture(Vec<String>);

    impl Host for Capture {
        fn print(&mut self, text: &str) {
            self.0.push(text.to_string());
        }
    }

    #[test]
    fn default_limits_match_the_architecture() {
        let limits = Limits::default();
        assert_eq!(limits.max_steps, 100_000_000);
        assert_eq!(limits.max_frames, 10_000);
        assert_eq!(limits.max_heap_bytes, 268_435_456);
        assert_eq!(limits.max_text_bytes, 16_777_216);
    }

    #[test]
    fn kind_names_are_stable() {
        let names: Vec<String> = [
            RuntimeErrorKind::IntOverflow,
            RuntimeErrorKind::DivisionByZero,
            RuntimeErrorKind::CallDepth,
            RuntimeErrorKind::StepLimit,
            RuntimeErrorKind::HeapLimit,
            RuntimeErrorKind::TextLimit,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            names,
            [
                "IntOverflow",
                "DivisionByZero",
                "CallDepth",
                "StepLimit",
                "HeapLimit",
                "TextLimit"
            ]
        );
    }

    #[test]
    fn kind_messages_are_fixed() {
        let messages: Vec<&str> = [
            RuntimeErrorKind::IntOverflow,
            RuntimeErrorKind::DivisionByZero,
            RuntimeErrorKind::CallDepth,
            RuntimeErrorKind::StepLimit,
            RuntimeErrorKind::HeapLimit,
            RuntimeErrorKind::TextLimit,
        ]
        .iter()
        .map(|kind| kind.message())
        .collect();
        assert_eq!(
            messages,
            [
                "`Int` result is outside the range of -9007199254740991 to 9007199254740991",
                "division or remainder by zero",
                "call would exceed the limit of 10000 frames",
                "program exceeded its step limit; raise it with `--max-steps`",
                "VM heap would grow beyond 268435456 bytes",
                "`Text` value would be longer than 16777216 bytes",
            ]
        );
    }

    #[test]
    fn messages_name_the_fixed_limits() {
        let limits = Limits::default();
        assert!(
            RuntimeErrorKind::IntOverflow
                .message()
                .contains(&ostrel_ir::INT_MAX.to_string())
        );
        assert!(
            RuntimeErrorKind::IntOverflow
                .message()
                .contains(&ostrel_ir::INT_MIN.to_string())
        );
        assert!(
            RuntimeErrorKind::CallDepth
                .message()
                .ends_with(&format!(" {} frames", limits.max_frames))
        );
        assert!(
            RuntimeErrorKind::HeapLimit
                .message()
                .ends_with(&format!(" {} bytes", limits.max_heap_bytes))
        );
        assert!(
            RuntimeErrorKind::TextLimit
                .message()
                .ends_with(&format!(" {} bytes", limits.max_text_bytes))
        );
    }

    #[test]
    fn empty_image_runs_without_output() {
        let mut host = Capture(Vec::new());
        assert_eq!(run(&Image::default(), &mut host, Limits::default()), Ok(()));
        assert!(host.0.is_empty());
        let dyn_host: &mut dyn Host = &mut host;
        assert_eq!(run(&Image::default(), dyn_host, Limits::default()), Ok(()));
    }
}
