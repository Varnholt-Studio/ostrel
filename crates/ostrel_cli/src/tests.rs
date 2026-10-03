//! Tests of the pipeline wiring.
//!
//! The stages after the parser are driven with syntax trees built by hand for
//! real source texts, so the checker, lowering, bytecode compiler and VM run
//! exactly as under `ostrel run`. The stages before it run on the committed
//! goldens and examples of the repository.
//!
//! The crate denies `unwrap`, `expect`, `panic` and indexing in tests too, so
//! every helper returns a `Result` and the tests use `?`.

use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};

use ostrel_core::{Diagnostic, FileId, SourceMap};
use ostrel_syntax::ast::{
    BinaryOp, ExprId, ExprKind, FnDecl, Ident, Module, StmtKind, StrPart, TextRange,
};
use ostrel_vm::{Host, Limits, RuntimeErrorKind};

use crate::pipeline::{self, Exit, Mode};

type R<T = ()> = Result<T, String>;

/// Turns any error into the test failure text.
fn ok<T, E: Debug>(r: Result<T, E>) -> R<T> {
    r.map_err(|e| format!("{e:?}"))
}

/// Collects program output like stdout would receive it.
#[derive(Default)]
struct Capture(String);

impl Host for Capture {
    fn print(&mut self, text: &str) {
        self.0.push_str(text);
        self.0.push('\n');
    }
}

/// Builds a tree for a real source text; every node range is the first
/// occurrence of a piece of that text at or after a byte offset.
struct Tree {
    src: &'static str,
    m: Module,
}

impl Tree {
    fn new(src: &'static str) -> Tree {
        Tree {
            src,
            m: Module::new(),
        }
    }

    fn find(&self, needle: &str, from: usize) -> R<usize> {
        let rest = self.src.get(from..).ok_or("offset outside the source")?;
        let at = rest
            .find(needle)
            .ok_or_else(|| format!("`{needle}` not in the source"))?;
        Ok(from + at)
    }

    fn r(&self, needle: &str, from: usize) -> R<TextRange> {
        let start = self.find(needle, from)?;
        let range = TextRange::new(
            ok(u32::try_from(start))?,
            ok(u32::try_from(start + needle.len()))?,
        );
        range.ok_or_else(|| "bad range".to_string())
    }

    fn e(&mut self, kind: ExprKind, needle: &str, from: usize) -> R<ExprId> {
        let range = self.r(needle, from)?;
        ok(self.m.add_expr(kind, range))
    }

    fn int(&mut self, text: &str, from: usize) -> R<ExprId> {
        self.e(ExprKind::Int(ok(text.parse())?), text, from)
    }

    fn text(&mut self, value: &str, from: usize) -> R<ExprId> {
        let quoted = format!("\"{value}\"");
        let kind = ExprKind::Str(Box::new([StrPart::Text(value.into())]));
        self.e(kind, &quoted, from)
    }

    /// `print(arg)` where the call text `whole` starts at or after `from`.
    fn print(&mut self, arg: ExprId, whole: &str, from: usize) -> R<ExprId> {
        let name = Ident::new("print", self.r("print", from)?);
        let callee = self.e(ExprKind::Name(name), "print", from)?;
        let kind = ExprKind::Call {
            callee,
            args: Box::new([arg]),
        };
        self.e(kind, whole, from)
    }

    /// `fn main()` whose body is one expression statement per call.
    fn main(&mut self, calls: Vec<ExprId>) -> R {
        let mut stmts = Vec::new();
        for call in calls {
            let stmt = self.m.add_stmt(StmtKind::Expr(call), TextRange::default());
            stmts.push(ok(stmt)?);
        }
        let body = ok(self.m.add_block(stmts, TextRange::default()))?;
        let decl = FnDecl {
            name: Ident::new("main", self.r("main", 0)?),
            params: Box::new([]),
            result: None,
            body,
            range: self.r(self.src.trim_end(), 0)?,
        };
        ok(self.m.add_fn(decl))
    }

    fn execute(&self, mode: Mode, path: &str) -> R<(Exit, String, String)> {
        let mut map = SourceMap::new();
        let file = ok(map.add(path, self.src))?;
        let mut out = Capture::default();
        let mut err = String::new();
        let exit = pipeline::execute_module(mode, &map, file, &self.m, &mut out, &mut err);
        Ok((exit, out.0, err))
    }
}

fn run_mode() -> Mode {
    Mode::Run(Limits::default())
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Runs the whole pipeline on bytes, as `ostrel` would for a file at `path`.
fn execute_bytes(mode: Mode, path: &str, bytes: Vec<u8>) -> (Exit, String, String) {
    let mut out = Capture::default();
    let mut err = String::new();
    let exit = pipeline::execute(mode, path, bytes, &mut out, &mut err);
    (exit, out.0, err)
}

/// Runs the whole pipeline on a repository file, named by its path from the
/// repository root as the golden runner passes it.
fn execute_file(mode: Mode, rel: &str) -> R<(Exit, String, String)> {
    let bytes = ok(fs::read(repo().join(rel)))?;
    Ok(execute_bytes(mode, rel, bytes))
}

/// The file next to `case` (a path ending in `.ostl`) with extension `ext`.
fn sibling(case: &str, ext: &str) -> R<String> {
    let stem = case.trim_end_matches(".ostl");
    ok(fs::read_to_string(repo().join(format!("{stem}{ext}"))))
}

/// Repository files in `dir` ending in `ext`, sorted, as paths from the
/// repository root.
fn files(dir: &str, ext: &str) -> R<Vec<String>> {
    let mut names = Vec::new();
    for entry in ok(fs::read_dir(repo().join(dir)))? {
        let name = ok(ok(entry)?.file_name().into_string())?;
        if name.ends_with(ext) {
            names.push(format!("{dir}/{name}"));
        }
    }
    names.sort();
    Ok(names)
}

#[test]
fn hello_world_runs_through_checker_lowering_and_vm() -> R {
    let src = include_str!("../../../examples/v0_1/01_hello.ostl");
    let expected = include_str!("../../../examples/v0_1/01_hello.expected");
    let mut t = Tree::new(src);
    let from = t.find("print", 0)?;
    let text = t.text("Hello, world!", from)?;
    let call = t.print(text, "print(\"Hello, world!\")", from)?;
    t.main(vec![call])?;

    let path = "examples/v0_1/01_hello.ostl";
    let (exit, out, err) = t.execute(run_mode(), path)?;
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert_eq!(out, expected);

    let (exit, out, err) = t.execute(Mode::Check, path)?;
    assert_eq!((exit, out.as_str(), err.as_str()), (Exit::Success, "", ""));
    Ok(())
}

#[test]
fn runtime_error_keeps_output_and_renders_one_line() -> R {
    let src = "fn main()\n  print(\"before\")\n  print(7 / 0)\n";
    let mut t = Tree::new(src);
    let first = t.text("before", 0)?;
    let first = t.print(first, "print(\"before\")", 0)?;
    let line3 = t.find("print(7", 0)?;
    let seven = t.int("7", line3)?;
    let zero = t.int("0", line3)?;
    let div = ExprKind::Binary {
        op: BinaryOp::Div,
        lhs: seven,
        rhs: zero,
    };
    let div = t.e(div, "7 / 0", line3)?;
    let second = t.print(div, "print(7 / 0)", line3)?;
    t.main(vec![first, second])?;

    let (exit, out, err) = t.execute(run_mode(), "div.ostl")?;
    assert_eq!(exit, Exit::Failure);
    assert_eq!(out, "before\n");
    assert!(err.starts_with("div.ostl:3:"), "{err:?}");
    assert_eq!(err.lines().count(), 1, "{err:?}");
    assert!(
        err.ends_with(": runtime error[DivisionByZero]: division or remainder by zero\n"),
        "{err:?}"
    );

    // `check` does not run the program.
    let (exit, out, err) = t.execute(Mode::Check, "div.ostl")?;
    assert_eq!((exit, out.as_str(), err.as_str()), (Exit::Success, "", ""));
    Ok(())
}

#[test]
fn missing_main_is_the_catalog_golden_for_check_and_run() -> R {
    let src = include_str!("../../../tests/errors/entry_missing_main.ostl");
    let expected = include_str!("../../../tests/errors/entry_missing_main.expected_err");
    // The golden has no `main`; an empty tree gives the same entry diagnostic.
    let t = Tree::new(src);
    for mode in [Mode::Check, run_mode()] {
        let (exit, out, err) = t.execute(mode, "tests/errors/entry_missing_main.ostl")?;
        assert_eq!((exit, out.as_str()), (Exit::Failure, ""));
        assert_eq!(err, expected);
    }
    Ok(())
}

/// The CLI's part of the lexer goldens: every golden that is not valid UTF-8
/// (E0014, reported before the lexer runs) or on which the lexer of this build
/// reports a diagnostic gives exactly the expected stderr under `check` and
/// `run`. Goldens are read as bytes, as the CLI reads a file, because some of
/// them are not valid UTF-8 on purpose. Goldens the lexer accepts belong to
/// later stages and are checked by `ci/checks/55_golden.sh`.
#[test]
fn lexer_goldens_match_byte_for_byte() -> R {
    let mut matched = 0;
    let mut invalid_utf8 = 0;
    for case in files("tests/errors", ".ostl")? {
        let bytes = ok(fs::read(repo().join(&case)))?;
        let before_parser = match std::str::from_utf8(&bytes) {
            Err(_) => {
                invalid_utf8 += 1;
                true
            }
            Ok(src) => {
                let (_, lexed) = ostrel_syntax::lex::lex(src, FileId::from_raw(0));
                lexed.iter().any(Diagnostic::is_error)
            }
        };
        if !before_parser {
            continue;
        }
        let expected = sibling(&case, ".expected_err")?;
        for mode in [Mode::Check, run_mode()] {
            let (exit, out, err) = execute_file(mode, &case)?;
            assert_eq!(exit, Exit::Failure, "{case}");
            assert_eq!(out, "", "{case}");
            assert_eq!(err, expected, "{case}");
            matched += 1;
        }
    }
    assert!(matched >= 2 * 15, "only {matched} lexer golden runs");
    assert!(
        invalid_utf8 >= 3,
        "only {invalid_utf8} invalid UTF-8 goldens"
    );
    Ok(())
}

/// SPEC 12.6 (check of AC-03): a golden whose source holds a byte outside
/// U+0020 to U+007E and LF expects a message of printable ASCII and LF only, so
/// no invisible or confusable character reaches a diagnostic.
#[test]
fn goldens_with_non_ascii_source_expect_ascii_only() -> R {
    let mut checked = 0;
    for case in files("tests/errors", ".ostl")? {
        let bytes = ok(fs::read(repo().join(&case)))?;
        if bytes
            .iter()
            .all(|&b| b == b'\n' || (0x20..=0x7E).contains(&b))
        {
            continue;
        }
        let expected = sibling(&case, ".expected_err")?;
        assert!(
            expected
                .bytes()
                .all(|b| b == b'\n' || (0x20..=0x7E).contains(&b)),
            "{case}: {expected:?}"
        );
        checked += 1;
    }
    assert!(checked >= 8, "only {checked} goldens with non ASCII source");
    Ok(())
}

#[test]
fn invalid_utf8_is_one_diagnostic_at_the_first_bad_byte() {
    let src = b"fn main()\n  print(\"\xC3\xA4\xFF\")\n".to_vec();
    let (exit, out, err) = execute_bytes(run_mode(), "bad.ostl", src);
    assert_eq!((exit, out.as_str()), (Exit::Failure, ""));
    assert_eq!(
        err,
        "bad.ostl:2:11: error[E0014]: source is not valid UTF-8 (byte 0xFF)\n"
    );
}

#[test]
fn small_hostile_inputs_never_succeed_silently() {
    for src in [
        &b""[..],
        &b"\0\0\0"[..],
        &b"\n\n\n"[..],
        &b")"[..],
        &b"\xFE"[..],
    ] {
        for mode in [Mode::Check, run_mode()] {
            let (exit, _, err) = execute_bytes(mode, "x.ostl", src.to_vec());
            assert!(exit == Exit::Failure, "{src:?}: {exit:?}, stderr {err:?}");
            assert!(!err.is_empty(), "{src:?}");
        }
    }
}

/// Every example runs exactly as its expected file says.
#[test]
fn examples_run_with_their_expected_output() -> R {
    let cases = files("examples/v0_1", ".ostl")?;
    assert!(cases.len() >= 10, "examples not found: {cases:?}");
    for case in cases {
        let expected = sibling(&case, ".expected")?;
        let (exit, out, err) = execute_file(run_mode(), &case)?;
        if exit != Exit::Success {
            return Err(format!("{case}: {exit:?}, stderr {err:?}"));
        }
        assert_eq!(err, "", "{case}");
        assert_eq!(out, expected, "{case}");
    }
    Ok(())
}

#[test]
fn invalid_utf8_stops_before_the_lexer_in_check_and_run() {
    for (src, line) in [
        (
            &b"\xFE"[..],
            "x.ostl:1:1: error[E0014]: source is not valid UTF-8 (byte 0xFE)\n",
        ),
        (
            &b"fn main()\n  @\x80"[..],
            "x.ostl:2:4: error[E0014]: source is not valid UTF-8 (byte 0x80)\n",
        ),
    ] {
        for mode in [Mode::Check, run_mode()] {
            let (exit, out, err) = execute_bytes(mode, "x.ostl", src.to_vec());
            assert_eq!(
                (exit, out.as_str(), err.as_str()),
                (Exit::Failure, "", line),
                "{src:?}"
            );
        }
    }
}

#[test]
fn empty_file_is_e0400_at_the_first_column() {
    for mode in [Mode::Check, run_mode()] {
        let (exit, out, err) = execute_bytes(mode, "empty.ostl", Vec::new());
        assert_eq!((exit, out.as_str()), (Exit::Failure, ""));
        assert!(err.starts_with("empty.ostl:1:1: error[E0400]: "), "{err:?}");
        assert_eq!(err.lines().count(), 1, "{err:?}");
    }
}

/// The CLI renders the fixed message of the kind (D83): the golden of
/// `tests/runtime/step_limit_small` matches byte for byte, and a different
/// `--max-steps` value changes neither the message nor the position.
#[test]
fn runtime_error_uses_the_fixed_message_of_its_kind() -> R {
    let case = "tests/runtime/step_limit_small.ostl";
    let expected = sibling(case, ".expected")?;
    let expected_err = sibling(case, ".expected_err")?;
    for max_steps in [1000, 500] {
        let limits = Limits {
            max_steps,
            ..Limits::default()
        };
        let (exit, out, err) = execute_file(Mode::Run(limits), case)?;
        assert_eq!(exit, Exit::Failure, "--max-steps {max_steps}");
        assert_eq!(out, expected, "--max-steps {max_steps}");
        assert_eq!(err, expected_err, "--max-steps {max_steps}");
    }
    assert!(expected_err.ends_with(&format!(
        ": runtime error[StepLimit]: {}\n",
        RuntimeErrorKind::StepLimit.message()
    )));
    Ok(())
}

#[test]
fn exit_codes_are_fixed() {
    let codes: Vec<u8> = [Exit::Success, Exit::Failure, Exit::Usage, Exit::Internal]
        .iter()
        .map(|e| e.code())
        .collect();
    assert_eq!(codes, [0, 1, 2, 70]);
}

#[test]
fn escape_keeps_paths_on_one_line() {
    assert_eq!(
        pipeline::escape("a\nb\u{202E}.ostl"),
        "a\\u{000A}b\\u{202E}.ostl"
    );
    assert_eq!(pipeline::escape("dir/\u{e4}.ostl"), "dir/\u{e4}.ostl");
}
