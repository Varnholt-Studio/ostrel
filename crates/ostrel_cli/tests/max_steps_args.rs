//! Edge values of `ostrel run --max-steps` (SPEC 12.4, AC-04, AC-52,
//! ARCHITECTURE 3.4).
//!
//! What the spec fixes and these tests check:
//!
//! * `StepLimit` is raised for more steps than the value of `--max-steps`
//!   (AC-52), with exit code 1, the output printed before the error kept, and
//!   the one fixed message of ARCHITECTURE 3.4 on stderr.
//! * No text depends on `--max-steps` (SPEC 12.4, D83); for a limit reached
//!   inside one top level call of `main`, neither does the position. When
//!   `main` itself is the running frame, the position depends on the limit, so
//!   no test here pins it (SPEC 12.4).
//! * Exit codes are 0, 1 or 2 only; 70 and 101 are always defects (D84).
//!
//! * Accepted values (SPEC 12.4): one or more ASCII digits with a value from 1
//!   to `u64::MAX`, leading zeros allowed. Every other value (zero, negative,
//!   signed, non numeric, above `u64::MAX`) and a missing value is a usage
//!   error with exit code 2, and the program is not run. Given more than once,
//!   every occurrence is checked: an invalid value in any occurrence is a usage
//!   error, also next to a valid one. Only when all are valid does the last
//!   occurrence count.
//! * A usage error writes exactly one `ostrel: <reason>` line, then the fixed
//!   usage text (SPEC 12.4, the form checked in `contracts.rs`).
//!
//! No test depends on the exact step boundary or on step weights, which are
//! internal to the VM (SPEC 12.4): a limit of 1 is below what any program
//! needs, and every other limit used is far below or far above the steps
//! needed.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const USAGE: &str = "usage: ostrel run [--max-steps N] FILE\n       ostrel check [--list-elevated] FILE\n       ostrel --help | --version\n";
const STEP_LIMIT_MESSAGE: &str =
    "runtime error[StepLimit]: program exceeded its step limit; raise it with `--max-steps`";

/// `fib(20)` needs far more than 100 000 steps and far less than the default
/// limit; `main` prints `start` before the call.
const FIB_CASE: &str = "tests/runtime/step_limit_small.ostl";
const FIB_EXPECTED_ERR: &str = "tests/runtime/step_limit_small.expected_err";

/// A program whose only statement prints. Used where the limit is far above
/// its needs, or where only the exit code and the error kind are checked.
const PRINT_ONLY: &str = "fn main()\n  print(\"a\")\n";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn ostrel<S: AsRef<OsStr>>(args: &[S]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ostrel"))
        .args(args)
        .current_dir(repo())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo().join(rel)).unwrap()
}

/// Writes `source` to a fresh scratch file for one test and returns its path.
fn source_file(name: &str, source: &str) -> String {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("ostrel_cli_max_steps")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.ostl");
    fs::write(&file, source).unwrap();
    file.to_str().unwrap().to_string()
}

/// Asserts a usage error: exit 2, the program did not run (stdout empty), one
/// `ostrel: ` line naming the flag, then the usage text.
fn assert_usage_error(out: &Output, what: &str) {
    assert_eq!(out.status.code(), Some(2), "{what}");
    assert!(out.stdout.is_empty(), "{what}: the program must not run");
    let err = text(&out.stderr);
    let first = err.lines().next().unwrap_or_default();
    assert!(first.starts_with("ostrel: "), "{what}: {err}");
    assert!(first.contains("--max-steps"), "{what}: {err}");
    assert!(!err.contains("runtime error"), "{what}: {err}");
    assert_eq!(err, format!("{first}\n{USAGE}"), "{what}");
}

/// Asserts a `StepLimit` failure without pinning the position or the output
/// before the error: with `main` as the running frame both depend on the limit
/// and on step weights (SPEC 12.4). Exit 1, and stderr is exactly one line
/// `FILE:LINE:COLUMN: runtime error[StepLimit]: <fixed message>`.
fn assert_step_limit(out: &Output, file: &str, what: &str) {
    assert_eq!(out.status.code(), Some(1), "{what}");
    let err = text(&out.stderr);
    assert_eq!(err.lines().count(), 1, "{what}: {err}");
    let suffix = format!(": {STEP_LIMIT_MESSAGE}\n");
    assert!(err.ends_with(&suffix), "{what}: {err}");
    let position = err
        .strip_prefix(&format!("{file}:"))
        .and_then(|rest| rest.strip_suffix(&suffix))
        .unwrap_or_else(|| panic!("{what}: {err}"));
    let parts: Vec<&str> = position.split(':').collect();
    assert_eq!(parts.len(), 2, "{what}: {err}");
    for part in parts {
        assert!(
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()),
            "{what}: {err}"
        );
    }
}

#[test]
fn ac_52_max_steps_one_stops_the_first_print() {
    let file = source_file("one", PRINT_ONLY);
    for args in [
        &["run", "--max-steps", "1", &file][..],
        &["run", "--max-steps=1", &file][..],
        &["run", &file, "--max-steps", "1"][..],
    ] {
        assert_step_limit(&ostrel(args), &file, &format!("{args:?}"));
    }
}

#[test]
fn ac_52_max_steps_value_changes_neither_message_nor_position() {
    let expected_err = read(FIB_EXPECTED_ERR);
    for steps in ["50", "1000", "100000"] {
        for args in [
            vec![
                "run".to_string(),
                "--max-steps".to_string(),
                steps.to_string(),
                FIB_CASE.to_string(),
            ],
            vec![
                "run".to_string(),
                format!("--max-steps={steps}"),
                FIB_CASE.to_string(),
            ],
            vec![
                "run".to_string(),
                FIB_CASE.to_string(),
                "--max-steps".to_string(),
                steps.to_string(),
            ],
        ] {
            let out = ostrel(&args);
            assert_eq!(out.status.code(), Some(1), "{args:?}");
            assert_eq!(text(&out.stdout), "start\n", "{args:?}");
            assert_eq!(text(&out.stderr), expected_err, "{args:?}");
        }
    }
}

#[test]
fn ac_52_max_steps_u64_max_runs_to_completion() {
    let max = u64::MAX.to_string();
    assert_eq!(max, "18446744073709551615");
    let default = ostrel(&["run", "examples/v0_1/07_recursion.ostl"]);
    assert_eq!(default.status.code(), Some(0));
    for args in [
        vec![
            "run".to_string(),
            "--max-steps".to_string(),
            max.clone(),
            "examples/v0_1/07_recursion.ostl".to_string(),
        ],
        vec![
            "run".to_string(),
            format!("--max-steps={max}"),
            "examples/v0_1/07_recursion.ostl".to_string(),
        ],
    ] {
        let out = ostrel(&args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert_eq!(out.stdout, default.stdout, "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}: {}", text(&out.stderr));
    }

    let out = ostrel(&["run", "--max-steps", &max, FIB_CASE]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "start\n6765\nnot reached\n");
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
}

#[test]
fn ac_52_max_steps_zero_is_a_usage_error() {
    let file = source_file("zero", PRINT_ONLY);
    for args in [
        &["run", "--max-steps", "0", &file][..],
        &["run", "--max-steps=0", &file][..],
        &["run", &file, "--max-steps", "0"][..],
        &["run", "--max-steps", "00", &file][..],
    ] {
        assert_usage_error(&ostrel(args), &format!("{args:?}"));
    }
}

#[test]
fn ac_52_max_steps_negative_or_signed_is_a_usage_error() {
    let file = source_file("negative", PRINT_ONLY);
    for value in [
        "-1",
        "-0",
        "+1",
        "-18446744073709551615",
        "-9223372036854775808",
    ] {
        for args in [
            vec![
                "run".to_string(),
                "--max-steps".to_string(),
                value.to_string(),
                file.clone(),
            ],
            vec![
                "run".to_string(),
                format!("--max-steps={value}"),
                file.clone(),
            ],
        ] {
            assert_usage_error(&ostrel(&args), &format!("{args:?}"));
        }
    }
}

#[test]
fn ac_52_max_steps_not_numeric_is_a_usage_error() {
    let file = source_file("not_numeric", PRINT_ONLY);
    for value in [
        "abc", "ten", "1e3", "0x10", "1_000", "1,000", "1.0", "1.5", "5x", "x5", " 5", "5 ",
        "\u{FF15}", "\u{0665}", "",
    ] {
        for args in [
            vec![
                "run".to_string(),
                "--max-steps".to_string(),
                value.to_string(),
                file.clone(),
            ],
            vec![
                "run".to_string(),
                format!("--max-steps={value}"),
                file.clone(),
            ],
        ] {
            assert_usage_error(&ostrel(&args), &format!("{args:?}"));
        }
    }
}

#[test]
fn ac_52_max_steps_above_u64_is_a_usage_error() {
    let file = source_file("above_u64", PRINT_ONLY);
    for value in [
        "18446744073709551616",
        "18446744073709551625",
        "99999999999999999999",
        "340282366920938463463374607431768211456",
    ] {
        for args in [
            vec![
                "run".to_string(),
                "--max-steps".to_string(),
                value.to_string(),
                file.clone(),
            ],
            vec![
                "run".to_string(),
                format!("--max-steps={value}"),
                file.clone(),
            ],
        ] {
            assert_usage_error(&ostrel(&args), &format!("{args:?}"));
        }
    }
}

#[test]
fn ac_52_max_steps_missing_value_is_a_usage_error() {
    let file = source_file("missing", PRINT_ONLY);
    for args in [
        // At the end of the line.
        &["run", &file, "--max-steps"][..],
        &["run", "--max-steps"][..],
        // The next argument is taken as the value, so the file is no number.
        &["run", "--max-steps", &file][..],
        // `--` is not a value.
        &["run", "--max-steps", "--", &file][..],
        // An empty value after `=`.
        &["run", "--max-steps=", &file][..],
    ] {
        assert_usage_error(&ostrel(args), &format!("{args:?}"));
    }
}

#[cfg(unix)]
#[test]
fn ac_52_max_steps_non_utf8_value_is_a_usage_error() {
    use std::os::unix::ffi::OsStrExt;
    let file = source_file("non_utf8", PRINT_ONLY);
    let out = ostrel(&[
        OsStr::new("run"),
        OsStr::new("--max-steps"),
        OsStr::from_bytes(b"1\xFF"),
        OsStr::new(&file),
    ]);
    assert_usage_error(&out, "non UTF-8 value");
}

#[test]
fn ac_52_max_steps_leading_zeros_are_allowed() {
    let file = source_file("leading_zeros", PRINT_ONLY);
    // `0001` is 1: the run stops with `StepLimit`.
    for args in [
        &["run", "--max-steps", "0001", &file][..],
        &["run", "--max-steps=0001", &file][..],
    ] {
        assert_step_limit(&ostrel(args), &file, &format!("{args:?}"));
    }
    // `u64::MAX` written with leading zeros is still in range.
    let padded = format!("000000000000000000000{}", u64::MAX);
    for args in [
        vec![
            "run".to_string(),
            "--max-steps".to_string(),
            padded.clone(),
            file.clone(),
        ],
        vec![
            "run".to_string(),
            format!("--max-steps={padded}"),
            file.clone(),
        ],
    ] {
        let out = ostrel(&args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(text(&out.stdout), "a\n", "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}: {}", text(&out.stderr));
    }
}

#[test]
fn ac_52_max_steps_given_twice_the_last_counts() {
    let file = source_file("repeated", PRINT_ONLY);
    let max = u64::MAX.to_string();
    // Last value `u64::MAX`: the program runs to the end.
    for args in [
        vec![
            "run".to_string(),
            "--max-steps".to_string(),
            "1".to_string(),
            "--max-steps".to_string(),
            max.clone(),
            file.clone(),
        ],
        vec![
            "run".to_string(),
            "--max-steps=1".to_string(),
            format!("--max-steps={max}"),
            file.clone(),
        ],
        vec![
            "run".to_string(),
            "--max-steps=1".to_string(),
            file.clone(),
            "--max-steps".to_string(),
            max.clone(),
        ],
    ] {
        let out = ostrel(&args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(text(&out.stdout), "a\n", "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}: {}", text(&out.stderr));
    }
    // Last value 1: the run stops with `StepLimit`.
    for args in [
        vec![
            "run".to_string(),
            "--max-steps".to_string(),
            max.clone(),
            "--max-steps".to_string(),
            "1".to_string(),
            file.clone(),
        ],
        vec![
            "run".to_string(),
            format!("--max-steps={max}"),
            "--max-steps=1".to_string(),
            file.clone(),
        ],
        vec![
            "run".to_string(),
            "--max-steps".to_string(),
            max.clone(),
            file.clone(),
            "--max-steps=1".to_string(),
        ],
    ] {
        assert_step_limit(&ostrel(&args), &file, &format!("{args:?}"));
    }
}

#[test]
fn ac_52_max_steps_invalid_in_any_occurrence_is_a_usage_error() {
    let file = source_file("repeated_invalid", PRINT_ONLY);
    let max = u64::MAX.to_string();
    // One invalid value per rejected class, before and after a valid one.
    for invalid in ["0", "-1", "+5", "1e3", "", "18446744073709551616"] {
        for valid in ["5", max.as_str()] {
            for args in [
                vec![
                    "run".to_string(),
                    "--max-steps".to_string(),
                    invalid.to_string(),
                    "--max-steps".to_string(),
                    valid.to_string(),
                    file.clone(),
                ],
                vec![
                    "run".to_string(),
                    "--max-steps".to_string(),
                    valid.to_string(),
                    "--max-steps".to_string(),
                    invalid.to_string(),
                    file.clone(),
                ],
                vec![
                    "run".to_string(),
                    format!("--max-steps={invalid}"),
                    file.clone(),
                    format!("--max-steps={valid}"),
                ],
                vec![
                    "run".to_string(),
                    format!("--max-steps={valid}"),
                    file.clone(),
                    format!("--max-steps={invalid}"),
                ],
            ] {
                assert_usage_error(&ostrel(&args), &format!("{args:?}"));
            }
        }
    }
    // A missing value after a valid occurrence.
    let args = ["run", "--max-steps", "5", &file, "--max-steps"];
    assert_usage_error(&ostrel(&args), &format!("{args:?}"));
}
