//! AC-56 (query naming and newest N), compile side, through the `ostrel` binary.
//!
//! The naming rule, its positions and the accepted values of `n` follow SPEC 12.10; the
//! examples in `examples/v0_2/queries/` follow their `EXPECTED.md`. Expected output is written
//! by hand from the spec, never copied from compiler output.
//!
//! Golden cases live in `tests/ac_56/`: `NAME.ostl` with `NAME.expected_err` must exit 1 with
//! exactly that stderr; `accept_NAME.ostl` must exit 0 with empty stderr. Not covered here yet:
//! E0312 (constant negative `n`) and E0210 (`sort id`), which come with their checker tasks, and
//! the run time half of AC-56 (`see` before `last n` on the server, `NegativeCount`).

use std::path::{Path, PathBuf};
use std::process::Output;

const CASES: &str = "crates/ostrel_cli/tests/ac_56";
const NAMING: &str = "examples/v0_2/queries/naming";
const RENAME: &str = "rename the local";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `ostrel check PATH`, with PATH relative to the repository root as it appears in messages.
fn check(path: &str) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_ostrel"))
        .args(["check", path])
        .current_dir(repo())
        .output()
        .unwrap_or_else(|e| panic!("cannot run ostrel: {e}"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap_or_else(|e| panic!("output is not UTF-8: {e}"))
}

/// Exit 0 and nothing on either stream.
fn assert_accepted(path: &str) {
    let out = check(path);
    assert_eq!(out.status.code(), Some(0), "{path}: {}", text(&out.stderr));
    assert!(out.stderr.is_empty(), "{path}: {}", text(&out.stderr));
    assert!(out.stdout.is_empty(), "{path}: {}", text(&out.stdout));
}

/// Exit 1, empty stdout and exactly `want` on stderr.
fn assert_rejected(path: &str, want: &str) {
    let out = check(path);
    assert_eq!(out.status.code(), Some(1), "{path}: {}", text(&out.stderr));
    assert!(out.stdout.is_empty(), "{path}: {}", text(&out.stdout));
    assert_eq!(text(&out.stderr), want, "{path}");
}

#[test]
fn ac_56_naming_rule_goldens() {
    let dir = repo().join(CASES);
    let mut stems: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{CASES}: {e}"))
        .map(|e| e.unwrap_or_else(|e| panic!("{CASES}: {e}")).path())
        .filter(|p| p.extension().is_some_and(|x| x == "ostl"))
        .map(|p| {
            p.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    stems.sort();
    let (mut rejected, mut accepted) = (0, 0);
    for stem in &stems {
        let path = format!("{CASES}/{stem}.ostl");
        let expected = dir.join(format!("{stem}.expected_err"));
        if stem.starts_with("accept_") {
            assert!(
                !expected.exists(),
                "{path}: an accept_ case has no expected_err"
            );
            assert_accepted(&path);
            accepted += 1;
        } else {
            let want = std::fs::read_to_string(&expected)
                .unwrap_or_else(|e| panic!("{path}: no {stem}.expected_err: {e}"));
            assert!(
                want.contains("error[E0207]") && want.contains(RENAME),
                "{path}"
            );
            assert_rejected(&path, &want);
            rejected += 1;
        }
    }
    // Every expected file has its case.
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{CASES}: {e}")) {
        let p = entry.unwrap_or_else(|e| panic!("{CASES}: {e}")).path();
        if p.extension().is_some_and(|x| x == "expected_err") {
            assert!(
                p.with_extension("ostl").exists(),
                "{} has no case",
                p.display()
            );
        }
    }
    // A deleted case must not pass unnoticed.
    assert!(rejected >= 8, "{rejected} rejected cases in {CASES}");
    assert!(accepted >= 3, "{accepted} accepted cases in {CASES}");
}

/// Cases S-01 to S-06 of `examples/v0_2/queries/EXPECTED.md` 4, with the positions of SPEC
/// 12.10: the first bare occurrence of the name in the query, or the type name in the query
/// head when the query does not mention it (S-06).
#[test]
fn ac_56_query_examples_report_the_local_at_its_position() {
    let local = |name: &str| format!("local `{name}` has the name of a field of `Post`; {RENAME}");
    for (file, line_col, name) in [
        ("s01_param_in_where", "11:19", "pos"),
        ("s02_let_in_where", "12:19", "pos"),
        ("s03_loop_var_in_where", "16:27", "pos"),
        ("s04_loop_var_in_sort", "16:26", "pos"),
        ("s05_implicit_field", "11:19", "made"),
        ("s06_unused_local", "11:8", "pos"),
    ] {
        let path = format!("{NAMING}/{file}.ostl");
        let want = format!("{path}:{line_col}: error[E0207]: {}\n", local(name));
        assert_rejected(&path, &want);
    }
}

/// Cases C-01 and C-02 and the feed of `examples/v0_2/queries/` compile.
#[test]
fn ac_56_query_examples_without_clash_compile() {
    for path in [
        format!("{NAMING}/c01_local_of_other_type.ostl"),
        format!("{NAMING}/c02_no_where_no_sort.ostl"),
        "examples/v0_2/queries/feed.ostl".to_owned(),
    ] {
        assert_accepted(&path);
    }
}
