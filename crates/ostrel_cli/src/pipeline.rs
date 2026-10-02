//! The compiler pipeline behind `ostrel run` and `ostrel check` (ARCHITECTURE 3.4).
//!
//! Stages: UTF 8 decoding, lexer (`ostrel_syntax::lex`), parser
//! ([`crate::frontend`]), checker (`ostrel_sema::check`), lowering
//! (`ostrel_ir::lower`), bytecode compiler (`ostrel_vm::compile`) and the VM
//! (`ostrel_vm::run`). Each stage runs only when the stages before it reported
//! no error (SPEC 12.1: the checker does not run on a file with lexer or parser
//! diagnostics).
//!
//! Output contract:
//! * Program output goes through the [`Host`] given by the caller.
//! * Diagnostics, runtime errors and internal errors are appended to an error
//!   text, one line each, which the caller writes to stderr after it has
//!   flushed stdout.
//! * The returned [`Exit`] is the process exit code.

use std::fmt::Write as _;

use ostrel_core::{Code, Diagnostic, FileId, SourceMap, Span};
use ostrel_syntax::ast::Module;
use ostrel_vm::{Host, Limits, RuntimeError, RuntimeErrorKind};

use crate::frontend;

/// Exit codes of the CLI (ARCHITECTURE 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The program was accepted (and, for `run`, ran to its end).
    Success,
    /// A diagnostic, a runtime error or an unreadable input file.
    Failure,
    /// The command line was not understood.
    Usage,
    /// A defect of the compiler itself, never caused by the input alone.
    Internal,
}

impl Exit {
    /// The process exit code.
    pub fn code(self) -> u8 {
        match self {
            Exit::Success => 0,
            Exit::Failure => 1,
            Exit::Usage => 2,
            // EX_SOFTWARE of sysexits.h: distinct from 0, 1, 2 and from the 101 of a panic.
            Exit::Internal => 70,
        }
    }
}

/// What to do with a file that compiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Stop after the checker.
    Check,
    /// Run `fn main()` within these limits.
    Run(Limits),
}

/// The code reported for a source file that is not valid UTF 8.
///
/// ASSUMPTION: the catalog `tests/errors/README.md` has no code for invalid
/// UTF 8 yet. E0008 (unexpected character, SPEC 12.2) is the nearest lexer code
/// until T1 registers one.
const INVALID_UTF8: Code = Code::new(8);

/// Compiles the file `path` with content `bytes` and, for [`Mode::Run`], runs it.
pub fn execute(
    mode: Mode,
    path: &str,
    bytes: Vec<u8>,
    host: &mut dyn Host,
    err: &mut String,
) -> Exit {
    let mut map = SourceMap::new();
    let (text, invalid_at) = match String::from_utf8(bytes) {
        Ok(text) => (text, None),
        Err(e) => {
            let at = e.utf8_error().valid_up_to();
            let bytes = e.into_bytes();
            let byte = bytes.get(at).copied().unwrap_or_default();
            let prefix = bytes.get(..at).unwrap_or_default();
            (
                String::from_utf8_lossy(prefix).into_owned(),
                Some((at, byte)),
            )
        }
    };
    let file = match map.add(path, text) {
        Ok(file) => file,
        Err(e) => {
            let _ = writeln!(err, "ostrel: {}: {e}", escape(path));
            return Exit::Failure;
        }
    };
    if let Some((at, byte)) = invalid_at {
        let offset = u32::try_from(at).unwrap_or(u32::MAX);
        let diag = Diagnostic::error(
            INVALID_UTF8,
            Span::point(file, offset),
            format!("source is not valid UTF-8 (byte 0x{byte:02X})"),
        );
        report(&map, &[diag], err);
        return Exit::Failure;
    }
    let src = map.text(file).unwrap_or_default();

    let (tokens, mut diags) = ostrel_syntax::lex::lex(src, file);
    let module = match frontend::parse(src, file, &tokens, &diags) {
        Ok((module, parsed)) => {
            diags.extend(parsed);
            module
        }
        Err(frontend::ParserMissing) => {
            if has_error(&diags) {
                report(&map, &diags, err);
                return Exit::Failure;
            }
            let _ = writeln!(
                err,
                "ostrel: internal error: the parser is not part of this build"
            );
            return Exit::Internal;
        }
    };
    if has_error(&diags) {
        report(&map, &diags, err);
        return Exit::Failure;
    }
    report(&map, &diags, err);
    execute_module(mode, &map, file, &module, host, err)
}

/// The stages after the parser: checker, lowering, bytecode compiler and VM.
///
/// `module` must be the tree parsed from the text of `file` in `map`.
pub fn execute_module(
    mode: Mode,
    map: &SourceMap,
    file: FileId,
    module: &Module,
    host: &mut dyn Host,
    err: &mut String,
) -> Exit {
    let src = map.text(file).unwrap_or_default();
    let (checked, diags) = ostrel_sema::check(module, file, src);
    report(map, &diags, err);
    if has_error(&diags) {
        return Exit::Failure;
    }
    let Mode::Run(limits) = mode else {
        return Exit::Success;
    };
    let program = match ostrel_ir::lower(module, &checked, file) {
        Ok(program) => program,
        Err(e) => {
            let _ = writeln!(err, "ostrel: internal error: lowering failed: {e:?}");
            return Exit::Internal;
        }
    };
    let image = match ostrel_vm::compile(&program) {
        Ok(image) => image,
        Err(e) => {
            let _ = writeln!(err, "ostrel: internal error: {e}");
            return Exit::Internal;
        }
    };
    match ostrel_vm::run(&image, host, limits) {
        Ok(()) => Exit::Success,
        Err(e) => {
            render_runtime_error(map, &e, limits, err);
            Exit::Failure
        }
    }
}

fn has_error(diags: &[Diagnostic]) -> bool {
    diags.iter().any(Diagnostic::is_error)
}

/// Appends one line per diagnostic, in source order (a stable sort keeps the
/// order of diagnostics at the same position).
fn report(map: &SourceMap, diags: &[Diagnostic], err: &mut String) {
    let mut sorted: Vec<&Diagnostic> = diags.iter().collect();
    sorted.sort_by_key(|d| (d.span.file, d.span.start));
    for d in sorted {
        err.push_str(&d.render(map));
        err.push('\n');
    }
}

/// Appends `file:line:column: runtime error[Kind]: message` (ARCHITECTURE 3.4).
pub fn render_runtime_error(map: &SourceMap, e: &RuntimeError, limits: Limits, err: &mut String) {
    let file = FileId::from_raw(e.span.file);
    let (path, line, column) = match (map.path(file), map.location(file, e.span.start)) {
        (Some(path), Some(loc)) => (path, loc.line, loc.column),
        _ => ("<unknown>", 0, 0),
    };
    let _ = writeln!(
        err,
        "{}:{line}:{column}: runtime error[{}]: {}",
        escape(path),
        e.kind,
        runtime_message(e.kind, limits)
    );
}

/// The message of a runtime error.
///
/// ASSUMPTION: ARCHITECTURE 3.4 fixes the kinds and the line format but no
/// message texts; the goldens of `tests/runtime/` (T2-4) pin them.
pub fn runtime_message(kind: RuntimeErrorKind, limits: Limits) -> String {
    match kind {
        RuntimeErrorKind::IntOverflow => format!(
            "result is outside the `Int` range of {} to {}",
            ostrel_ir::INT_MIN,
            ostrel_ir::INT_MAX
        ),
        RuntimeErrorKind::DivisionByZero => "division by zero".to_string(),
        RuntimeErrorKind::CallDepth => format!(
            "call depth exceeds {} frames; is the recursion unbounded?",
            limits.max_frames
        ),
        RuntimeErrorKind::StepLimit => format!(
            "program exceeds the limit of {} steps; raise it with `--max-steps`",
            limits.max_steps
        ),
        RuntimeErrorKind::HeapLimit => {
            format!(
                "memory use exceeds the limit of {} bytes",
                limits.max_heap_bytes
            )
        }
        RuntimeErrorKind::TextLimit => format!(
            "text exceeds the limit of {} UTF-8 bytes",
            limits.max_text_bytes
        ),
    }
}

/// Escapes characters that could break the one line form or mislead a
/// terminal, with the rule of `ostrel_core::Diagnostic::render`.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control()
            || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        {
            let _ = write!(out, "\\u{{{:04X}}}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}
