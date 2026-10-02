//! The `ostrel` binary: exit codes, streams and the pipeline thread.

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

#[test]
fn usage_errors_exit_with_2_and_print_nothing_to_stdout() {
    let hello = "examples/v0_1/01_hello.ostl";
    for args in [
        &[][..],
        &["run"][..],
        &["check"][..],
        &["run", "--no-such-flag-55", hello][..],
        &["check", "--max-steps", "3", hello][..],
        &["frobnicate", hello][..],
    ] {
        let out = ostrel(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(text(&out.stderr).contains("usage: ostrel run"), "{args:?}");
    }
}

#[test]
fn help_and_version_exit_with_0() {
    let out = ostrel(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stdout).starts_with("usage: ostrel run"));
    let out = ostrel(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stdout).starts_with("ostrel "));
}

#[test]
fn unreadable_file_exits_with_1() {
    for cmd in ["run", "check"] {
        let out = ostrel(&[cmd, "examples/v0_1/no_such_file.ostl"]);
        assert_eq!(out.status.code(), Some(1), "{cmd}");
        let err = text(&out.stderr);
        assert!(
            err.starts_with("ostrel: cannot read examples/v0_1/no_such_file.ostl: "),
            "{err}"
        );
        let out = ostrel(&[cmd, "examples"]);
        assert_eq!(out.status.code(), Some(1), "{cmd} on a directory");
    }
}

#[test]
fn lexer_golden_through_the_binary_keeps_the_path_as_given() {
    let case = "tests/errors/lex_tab_indent.ostl";
    let expected = std::fs::read(repo().join("tests/errors/lex_tab_indent.expected_err")).unwrap();
    for cmd in ["check", "run"] {
        let out = ostrel(&[cmd, case]);
        assert_eq!(out.status.code(), Some(1), "{cmd}");
        assert!(out.stdout.is_empty(), "{cmd}");
        assert_eq!(text(&out.stderr), text(&expected), "{cmd}");
    }
}

#[test]
fn hello_world_exits_0_with_its_expected_output_or_names_the_missing_parser() {
    let out = ostrel(&["run", "examples/v0_1/01_hello.ostl"]);
    match out.status.code() {
        Some(0) => {
            let expected = std::fs::read(repo().join("examples/v0_1/01_hello.expected")).unwrap();
            assert_eq!(out.stdout, expected);
            assert!(out.stderr.is_empty());
        }
        Some(70) => assert_eq!(
            text(&out.stderr),
            "ostrel: internal error: the parser is not part of this build\n"
        ),
        other => panic!("exit {other:?}, stderr {}", text(&out.stderr)),
    }
}
