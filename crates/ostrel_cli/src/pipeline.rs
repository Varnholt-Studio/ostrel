//! The compiler pipeline behind `ostrel run` and `ostrel check` (ARCHITECTURE 3.4).
//!
//! Stages: UTF 8 decoding, lexer (`ostrel_syntax::lex`), parser
//! ([`crate::frontend`]), checker (`ostrel_sema::check`), lowering
//! (`ostrel_ir::lower`), bytecode compiler (`ostrel_vm::compile`) and the VM
//! (`ostrel_vm::run`). Each stage runs only when the stages before it reported
//! no error (SPEC 12.1: the checker does not run on a file with lexer or parser
//! diagnostics).
//!
//! Two front ends follow the parser (SPEC 2: a script program has no `app`
//! declaration, an app has one):
//! * A script program goes through the v0.1 checker in the crate root of
//!   `ostrel_sema`, for `check` and `run` alike. Declarations outside the v0.1
//!   slice stay E0100 there.
//! * An app under `check` goes through the HIR: name resolution
//!   (`ostrel_sema::hir::resolve`) and, when that reported no error, the HIR
//!   checker (`ostrel_sema::hir::check`). An app under `run` takes the script
//!   path, because `run` only runs script programs.
//!
//! `ostrel check --list-elevated` ([`Mode::ListElevated`]) checks like
//! `check` and, when the program is accepted, prints the elevated functions
//! (AC-32, AC-54) to stdout. An accepted app gets one warning per elevated
//! function that returns rows without `see` (D99) under both modes.
//!
//! Output contract:
//! * Program output goes through the [`Host`] given by the caller.
//! * Diagnostics, runtime errors and internal errors are appended to an error
//!   text, one line each, which the caller writes to stderr after it has
//!   flushed stdout.
//! * The returned [`Exit`] is the process exit code.

use std::fmt::Write as _;

use ostrel_core::{Diagnostic, FileId, SourceMap, Span};
use ostrel_sema::elevated::{self, Elevated};
use ostrel_sema::hir;
use ostrel_syntax::ast::{Item, Module};
use ostrel_syntax::lex::codes::{INVALID_UTF8, msg_invalid_utf8};
use ostrel_vm::{Host, Limits, RuntimeError};

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
    /// Stop after the checker and list the elevated functions on stdout.
    ListElevated,
    /// Run `fn main()` within these limits.
    Run(Limits),
}

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
            msg_invalid_utf8(byte),
        );
        report(&map, &[diag], err);
        return Exit::Failure;
    }
    let src = map.text(file).unwrap_or_default();

    let (tokens, mut diags) = ostrel_syntax::lex::lex(src, file);
    let (module, parsed) = frontend::parse(src, file, &tokens, &diags);
    diags.extend(parsed);
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
    if !matches!(mode, Mode::Run(_)) && is_app(module) {
        return check_app(mode, map, file, src, module, host, err);
    }
    let (checked, diags) = ostrel_sema::check(module, file, src);
    report(map, &diags, err);
    if has_error(&diags) {
        return Exit::Failure;
    }
    let limits = match mode {
        Mode::Run(limits) => limits,
        Mode::Check => return Exit::Success,
        Mode::ListElevated => {
            // A script program has no server functions (SPEC 2).
            list_elevated(map, &hir::Program::default(), &[], host);
            return Exit::Success;
        }
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
            render_runtime_error(map, &e, err);
            Exit::Failure
        }
    }
}

/// Whether `module` declares an `app` (SPEC 2), which makes it an app rather
/// than a script program.
fn is_app(module: &Module) -> bool {
    module
        .items()
        .iter()
        .any(|item| matches!(item, Item::App(_)))
}

/// `ostrel check` of an app: name resolution into the HIR, then the HIR
/// checker. The checker runs only on a program that resolved without error,
/// so one fault never gives a second diagnostic (SPEC 12.1). An accepted app
/// gets the warnings of `ostrel_sema::elevated`, and under
/// [`Mode::ListElevated`] the list of its elevated functions.
fn check_app(
    mode: Mode,
    map: &SourceMap,
    file: FileId,
    src: &str,
    module: &Module,
    host: &mut dyn Host,
    err: &mut String,
) -> Exit {
    let sources = [hir::Source {
        file,
        text: src,
        module,
    }];
    let (mut program, diags) = hir::resolve(&sources);
    report(map, &diags, err);
    if has_error(&diags) {
        return Exit::Failure;
    }
    let mut diags = hir::check(&mut program);
    if has_error(&diags) {
        report(map, &diags, err);
        return Exit::Failure;
    }
    diags.extend(elevated::warnings(&program));
    report(map, &diags, err);
    if mode == Mode::ListElevated {
        list_elevated(map, &program, &elevated::list(&program), host);
    }
    Exit::Success
}

/// Prints the elevated functions, one header line and per function one line
/// with its position, name and result, then one line per client path:
///
/// ```text
/// elevated functions: 1
/// app.ostl:12:1: `export` -> List[Note]: returns rows without see
///   client path: report -> export
/// ```
fn list_elevated(map: &SourceMap, program: &hir::Program, list: &[Elevated], host: &mut dyn Host) {
    host.print(&format!("elevated functions: {}", list.len()));
    for e in list {
        let (path, line, column) = match (
            map.path(e.span.file),
            map.location(e.span.file, e.span.start),
        ) {
            (Some(path), Some(loc)) => (path, loc.line, loc.column),
            _ => ("<unknown>", 0, 0),
        };
        let mut head = format!("{}:{line}:{column}: `{}`", escape(path), escape(&e.name));
        if e.result != hir::Ty::Unit {
            let _ = write!(
                head,
                " -> {}",
                escape(&elevated::type_name(program, &e.result))
            );
        }
        if e.unfiltered.is_some() {
            head.push_str(": returns rows without see");
        }
        host.print(&head);
        if e.paths.is_empty() {
            host.print("  client path: none");
        }
        for p in &e.paths {
            let names: Vec<String> = p
                .iter()
                .map(|d| {
                    program
                        .def(*d)
                        .map_or_else(|| "?".to_string(), |d| escape(&d.name))
                })
                .collect();
            host.print(&format!("  client path: {}", names.join(" -> ")));
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
///
/// The message is the fixed text of the kind (D83), so it never depends on the
/// limits or flags of the run.
pub fn render_runtime_error(map: &SourceMap, e: &RuntimeError, err: &mut String) {
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
        e.kind.message()
    );
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

#[cfg(test)]
mod tests {
    //! Routing between the two front ends. The diagnostics of the HIR front
    //! end are pinned by the goldens under `tests/errors/v0_2/`.

    use ostrel_vm::{Host, Limits};

    use super::{Exit, Mode, execute};

    #[derive(Default)]
    struct Capture(String);

    impl Host for Capture {
        fn print(&mut self, text: &str) {
            self.0.push_str(text);
            self.0.push('\n');
        }
    }

    fn outcome(mode: Mode, src: &str) -> (Exit, String) {
        let mut out = Capture::default();
        let mut err = String::new();
        let exit = execute(mode, "t.ostl", src.as_bytes().to_vec(), &mut out, &mut err);
        assert!(out.0.is_empty(), "{:?}", out.0);
        (exit, err)
    }

    fn check(src: &str) -> (Exit, String) {
        outcome(Mode::Check, src)
    }

    const NOTES: &str = "\
app Notes

data Note
  title: Text
  see if true
  make if true

view Main
  col
    for n in Note sort title
      text n.title
";

    #[test]
    fn an_app_is_checked_through_the_hir() {
        // The v0.1 checker would report E0100 at `app` and E0400.
        assert_eq!(check(NOTES), (Exit::Success, String::new()));
        let src = NOTES.replace("sort title", "sort titel");
        assert_eq!(
            check(&src),
            (
                Exit::Failure,
                "t.ostl:10:24: error[E0204]: data type `Note` has no field `titel`\n".to_string()
            )
        );
    }

    #[test]
    fn author_is_a_field_only_with_auth() {
        let read =
            "data Post\n  pos: Int\n  see if true\n\nfn f(p: Post) -> Text\n  p.author.handle\n";
        let with_auth = format!("app A\n  auth paseto open\n\n{read}");
        assert_eq!(check(&with_auth), (Exit::Success, String::new()));
        let (exit, err) = check(&format!("app A\n\n{read}"));
        assert_eq!(exit, Exit::Failure);
        assert!(err.starts_with("t.ostl:8:5: error[E0204]: "), "{err}");
        assert_eq!(err.lines().count(), 1, "{err}");
        // A declared field `author` without `auth` is an ordinary field.
        let own = "app A\n\ndata Post\n  author: Text\n\nfn f(p: Post) -> Text\n  p.author\n";
        assert_eq!(check(own), (Exit::Success, String::new()));
    }

    #[test]
    fn a_script_program_keeps_the_v0_1_checker() {
        let src = "fn main()\n  print(\"ok\")\n\nenum Color = red | green\n";
        assert_eq!(
            check(src),
            (
                Exit::Failure,
                "t.ostl:4:1: error[E0100]: `enum` declaration is not available in v0.1\n"
                    .to_string()
            )
        );
    }

    #[test]
    fn run_takes_the_script_path_for_an_app() {
        let (exit, err) = outcome(Mode::Run(Limits::default()), NOTES);
        assert_eq!(exit, Exit::Failure);
        assert!(
            err.starts_with(
                "t.ostl:1:1: error[E0100]: `app` declaration is not available in v0.1\n"
            ),
            "{err}"
        );
    }

    #[test]
    fn the_hir_checker_does_not_run_after_a_resolve_error() {
        // Two independent faults, both from name resolution, each reported
        // once and in source order; nothing follows from the HIR checker.
        let src = "app A\n\ndata Post\n  pos: Int\n  pos: Text\n  see if nope\n";
        let (exit, err) = check(src);
        assert_eq!(exit, Exit::Failure);
        assert_eq!(
            err.lines()
                .map(|l| l.split(": ").next().unwrap_or_default())
                .collect::<Vec<_>>(),
            ["t.ostl:5:3", "t.ostl:6:10"],
            "{err}"
        );
    }
}
