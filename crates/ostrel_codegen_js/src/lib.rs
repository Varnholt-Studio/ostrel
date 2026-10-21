//! Browser backend of the Ostrel programming language (ARCHITECTURE 3, 4.2).
//!
//! [`emit`] turns a full IR program into a [`JsBundle`] of ES2020 modules
//! that run on the hand written runtime in `runtime/js` (D5: no WASM VM). The
//! bundle holds one application module, [`APP_MODULE`], with:
//!
//! * `schema`: the compiled schema of the program (app, enums, models with
//!   field names, types, write modes and merge strategies), deeply frozen.
//!   Names travel on the wire, positions are the ids (D132, D135).
//! * one exported function `f<id>` per function that runs on the client
//!   ([`Placement::Client`] and [`Placement::Shared`], `Program::target`),
//!   named by its [`FuncId`]. Server functions are never emitted.
//! * `OstrelError`, the error class of the runtime errors the generated code
//!   raises.
//!
//! The module imports the runtime through relative specifiers below
//! [`RUNTIME_DIR`], always spelled the same, so one page loads one instance of
//! each runtime module. The build places `runtime/js` there.
//!
//! Semantics shared with the VM: `Int` arithmetic is checked (`IntOverflow`,
//! ARCHITECTURE 7.4), `/` truncates toward zero and `%` takes the sign of the
//! dividend (`DivisionByZero` for a zero divisor), `-0` never appears as an
//! `Int`, `Text` and `Rank` order by code point through `compareText` (D50),
//! enum values order by declaration, and a `Text` above 16 777 216 UTF-8 bytes
//! is `TextLimit`.
//!
//! Status: first part of T121. Covered are the schema and the value and
//! control flow instructions; every other instruction kind is
//! [`EmitError::Unsupported`], never a silent gap in the output. Not covered
//! yet: views and the safe sinks, handlers, `var`s, `style` blocks, field
//! defaults, rows and queries, remote calls, `for` loops, collections, `Float`
//! arithmetic, std functions, source maps, `.d.ts`, and the service worker
//! (T81, v0.3). A program with any of them is [`EmitError::Unsupported`] at
//! its first occurrence, so a program with UI never gives a bundle without
//! it.

// This crate processes data derived from untrusted input: no unwrap, expect,
// panic or unchecked indexing.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod func;
mod lit;
mod schema;

use ostrel_core::{FileId, Span};
use ostrel_ir::full::{Program, Target};

/// Path of the application module inside the bundle.
pub const APP_MODULE: &str = "app.mjs";

/// Directory of the bundle, next to [`APP_MODULE`], that holds a copy of
/// `runtime/js`; generated modules import the runtime from there.
pub const RUNTIME_DIR: &str = "runtime";

/// The fixed start of the application module: the runtime imports and the
/// helpers that generated code calls.
const PRELUDE: &str = include_str!("prelude.mjs");

/// The output of the browser backend: the files of the client target, without
/// the runtime itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsBundle {
    /// The files, in a fixed order.
    pub files: Vec<JsFile>,
}

/// One file of a [`JsBundle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsFile {
    /// Path relative to the bundle root, with `/` as separator.
    pub path: String,
    /// The file content.
    pub text: String,
}

/// Why a program could not be emitted. Like `LowerError` and the VM's
/// `CompileError`, this is never caused by a program the checker accepted
/// once the backend is complete; until then [`EmitError::Unsupported`] names
/// what is missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmitError {
    /// The instruction or type at `span` is valid IR that this backend does
    /// not translate yet.
    Unsupported {
        /// The instruction, or the declaration, that cannot be emitted.
        span: Span,
        /// A short name of what is missing.
        what: &'static str,
    },
    /// The IR is inconsistent: an id, local or block out of range, or an
    /// operand of the wrong type. A defect of the lowering.
    Invalid(Span),
}

/// Emits the client target of `program`.
pub fn emit(program: &Program) -> Result<JsBundle, EmitError> {
    ui(program)?;
    let mut out = String::from(PRELUDE);
    out.push('\n');
    schema::emit(program, &mut out)?;
    for (id, function) in program.target(Target::Client) {
        out.push('\n');
        func::emit(program, id, function, &mut out)?;
    }
    Ok(JsBundle {
        files: vec![JsFile {
            path: APP_MODULE.to_string(),
            text: out,
        }],
    })
}

/// Refuses every part of the user interface: a program with UI must never
/// give a bundle without it. The home view is checked first, so a `home`
/// outside `views` is [`EmitError::Invalid`] whatever else the program has.
fn ui(program: &Program) -> Result<(), EmitError> {
    if let Some(home) = program.app.as_ref().and_then(|app| app.home) {
        // `App` carries no span in the IR; the error points at the start of
        // the first source file.
        let view = program
            .views
            .get(home.index())
            .ok_or(EmitError::Invalid(Span::point(FileId::from_raw(0), 0)))?;
        return Err(EmitError::Unsupported {
            span: view.span,
            what: "home view",
        });
    }
    if let Some(view) = program.views.first() {
        return Err(EmitError::Unsupported {
            span: view.span,
            what: "view",
        });
    }
    if let Some(var) = program.vars.first() {
        return Err(EmitError::Unsupported {
            span: var.span,
            what: "var",
        });
    }
    if let Some(style) = program.styles.first() {
        return Err(EmitError::Unsupported {
            span: *style,
            what: "style",
        });
    }
    Ok(())
}
