//! `ostrel check --list-elevated` and the warning for elevated functions that
//! return rows without `see` (AC-32, AC-54, D99).
//!
//! Goldens in `tests/elevated/`: per `NAME.ostl` the expected stdout of
//! `check --list-elevated` in `NAME.expected_out` and the expected stderr in
//! `NAME.expected_err`. Both commands exit with 0; a plain `check` prints the
//! same warnings and nothing on stdout.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn ostrel(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ostrel"))
        .args(args)
        .current_dir(repo())
        .output()
        .unwrap()
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

const DIR: &str = "crates/ostrel_cli/tests/elevated";

fn goldens() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(repo().join(DIR))
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().into_string().unwrap();
            name.strip_suffix(".ostl").map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

fn expected(name: &str, ext: &str) -> String {
    fs::read_to_string(repo().join(DIR).join(format!("{name}.{ext}"))).unwrap()
}

#[test]
fn list_elevated_goldens() {
    let names = goldens();
    for required in ["no_model_result", "none_elevated", "rows_without_see"] {
        assert!(
            names.iter().any(|n| n == required),
            "missing golden {required}"
        );
    }
    for name in &names {
        let file = format!("{DIR}/{name}.ostl");
        let out = ostrel(&["check", "--list-elevated", &file]);
        assert_eq!(out.status.code(), Some(0), "{name}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), expected(name, "expected_out"), "{name}");
        assert_eq!(text(&out.stderr), expected(name, "expected_err"), "{name}");

        let plain = ostrel(&["check", &file]);
        assert_eq!(plain.status.code(), Some(0), "{name}");
        assert_eq!(text(&plain.stdout), "", "{name}");
        assert_eq!(
            text(&plain.stderr),
            expected(name, "expected_err"),
            "{name}"
        );
    }
}

#[test]
fn every_elevated_function_and_path_is_listed() {
    let out = ostrel(&[
        "check",
        "--list-elevated",
        &format!("{DIR}/no_model_result.ostl"),
    ]);
    let stdout = text(&out.stdout);
    assert!(stdout.starts_with("elevated functions: 2\n"), "{stdout}");
    assert!(
        stdout.contains("  client path: renameAll -> rename\n"),
        "{stdout}"
    );
    // A plain server fn is not elevated, even when it returns rows.
    assert!(!stdout.contains("teamNames"), "{stdout}");
    assert!(!stdout.contains("returns rows without see"), "{stdout}");
}

#[test]
fn rows_without_see_are_marked_and_warned_once_per_function() {
    let out = ostrel(&[
        "check",
        "--list-elevated",
        &format!("{DIR}/rows_without_see.ostl"),
    ]);
    let stdout = text(&out.stdout);
    assert_eq!(
        stdout.matches(": returns rows without see\n").count(),
        2,
        "{stdout}"
    );
    assert!(
        stdout.contains("  client path: report -> collect -> exportNotes\n"),
        "{stdout}"
    );
    let stderr = text(&out.stderr);
    assert_eq!(stderr.lines().count(), 2, "{stderr}");
    assert!(
        stderr.lines().all(|l| l.contains(": warning[E0314]: ")),
        "{stderr}"
    );
}

#[test]
fn nothing_is_listed_when_the_check_fails() {
    let dir = tempdir();
    let file = dir.join("broken.ostl");
    let src = fs::read_to_string(repo().join(DIR).join("rows_without_see.ostl")).unwrap();
    fs::write(
        &file,
        src.replace("-> Note? as system", "-> Nope? as system"),
    )
    .unwrap();
    let out = ostrel(&["check", "--list-elevated", file.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    let stderr = text(&out.stderr);
    assert!(stderr.contains("error[E0203]"), "{stderr}");
    assert!(!stderr.contains("E0314"), "{stderr}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_script_program_has_no_elevated_functions() {
    let out = ostrel(&["check", "--list-elevated", "examples/v0_1/01_hello.ostl"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "elevated functions: 0\n");
    assert_eq!(text(&out.stderr), "");
}

fn tempdir() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("list_elevated_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}
