//! Process level contracts of the `ostrel` binary (ARCHITECTURE 3.4, SPEC 12.1,
//! 12.6, AC-52): exit codes, usage errors, the stderr line format, paths as
//! given, stdout flushing and edge cases of the input file.
//!
//! Exit code 70 marks an internal defect (ARCHITECTURE 3.4, D84). No test
//! here accepts it: every input ends with 0, 1 or 2.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const USAGE: &str = "usage: ostrel run [--max-steps N] FILE\n       ostrel check FILE\n       ostrel --help | --version\n";
const LEX_CASE: &str = "tests/errors/lex_tab_indent.ostl";
const LEX_EXPECTED: &str = "tests/errors/lex_tab_indent.expected_err";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn command<S: AsRef<OsStr>>(args: &[S]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ostrel"));
    cmd.args(args).current_dir(repo()).stdin(Stdio::null());
    cmd
}

fn ostrel<S: AsRef<OsStr>>(args: &[S]) -> Output {
    command(args).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn read(rel: &str) -> String {
    fs::read_to_string(repo().join(rel)).unwrap()
}

/// A fresh scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("ostrel_cli_contracts")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Asserts that `out` is a usage error: exit 2, nothing on stdout, one
/// `ostrel: ` line followed by the usage text on stderr.
fn assert_usage_error(out: &Output, what: &str) {
    assert_eq!(out.status.code(), Some(2), "{what}");
    assert!(out.stdout.is_empty(), "{what}");
    let err = text(&out.stderr);
    let first = err.lines().next().unwrap_or_default();
    assert!(first.starts_with("ostrel: "), "{what}: {err}");
    assert_eq!(err, format!("{first}\n{USAGE}"), "{what}");
}

#[test]
fn ac_52_usage_errors_exit_with_2() {
    let file = "examples/v0_1/01_hello.ostl";
    let cases: &[&[&str]] = &[
        &[],
        &[""],
        &["run"],
        &["check"],
        &["run", "--"],
        &["RUN", file],
        &["build", file],
        &["run", "--no-such-flag-55", file],
        &["check", "--no-such-flag-55", file],
        &["run", file, "--no-such-flag-55"],
        &["run", "-x", file],
        &["run", file, file],
        &["check", "--max-steps", "5", file],
        &["check", "--max-steps=5", file],
        &["run", "--max-steps"],
        &["run", file, "--max-steps"],
        &["run", "--max-steps", "0", file],
        &["run", "--max-steps", "00", file],
        &["run", "--max-steps", "-1", file],
        &["run", "--max-steps", "+5", file],
        &["run", "--max-steps", " 5", file],
        &["run", "--max-steps", "5x", file],
        &["run", "--max-steps", "", file],
        &["run", "--max-steps=", file],
        &["run", "--max-steps", "18446744073709551616", file],
    ];
    for args in cases {
        assert_usage_error(&ostrel(args), &format!("{args:?}"));
    }
}

#[test]
fn ac_52_usage_error_wins_over_an_unreadable_file() {
    let out = ostrel(&["run", "--no-such-flag-55", "no/such/file.ostl"]);
    assert_usage_error(&out, "unknown flag and missing file");
}

#[cfg(unix)]
#[test]
fn ac_52_non_utf8_command_is_a_usage_error() {
    use std::os::unix::ffi::OsStrExt;
    let out = ostrel(&[OsStr::from_bytes(b"r\xFFn"), OsStr::new(LEX_CASE)]);
    assert_usage_error(&out, "non UTF-8 command");
}

#[test]
fn help_and_version_write_to_stdout_only() {
    for args in [&["--help"][..], &["-h"][..], &["help"][..]] {
        let out = ostrel(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert_eq!(text(&out.stdout), USAGE, "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}");
    }
    let out = ostrel(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    let version = text(&out.stdout);
    assert_eq!(version, format!("ostrel {}\n", env!("CARGO_PKG_VERSION")));
    assert!(out.stderr.is_empty());
}

#[test]
fn max_steps_is_accepted_before_and_after_the_file() {
    let expected = read(LEX_EXPECTED);
    for args in [
        &["run", "--max-steps", "1", LEX_CASE][..],
        &["run", "--max-steps=18446744073709551615", LEX_CASE][..],
        &["run", LEX_CASE, "--max-steps", "7"][..],
        &["run", "--", LEX_CASE][..],
    ] {
        let out = ostrel(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert_eq!(text(&out.stderr), expected, "{args:?}");
    }
}

#[test]
fn names_after_double_dash_and_a_lone_dash_are_files() {
    for (args, shown) in [
        (&["run", "-"][..], "-"),
        (&["check", "-"][..], "-"),
        (&["run", "--", "--max-steps"][..], "--max-steps"),
        (&["check", "--", "-x.ostl"][..], "-x.ostl"),
    ] {
        let out = ostrel(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        let err = text(&out.stderr);
        assert!(
            err.starts_with(&format!("ostrel: cannot read {shown}: ")),
            "{err}"
        );
        assert_eq!(err.lines().count(), 1, "{err}");
    }
}

#[test]
fn file_path_stays_on_one_line_in_stderr() {
    let out = ostrel(&["check", "no/such\ndir/\u{1b}[31m\u{202e}x.ostl"]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(
        err.starts_with("ostrel: cannot read no/such\\u{000A}dir/\\u{001B}[31m\\u{202E}x.ostl: "),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_file_path_is_reported_lossily_on_one_line() {
    use std::os::unix::ffi::OsStrExt;
    let path = OsStr::from_bytes(b"no_such_\xFF.ostl");
    for cmd in ["run", "check"] {
        let out = ostrel(&[OsStr::new(cmd), path]);
        assert_eq!(out.status.code(), Some(1), "{cmd}");
        let err = text(&out.stderr);
        assert!(
            err.starts_with("ostrel: cannot read no_such_\u{fffd}.ostl: "),
            "{err}"
        );
        assert_eq!(err.lines().count(), 1, "{err}");
    }
}

#[test]
fn diagnostics_name_the_path_exactly_as_given() {
    let expected = read(LEX_EXPECTED);
    let dotted = format!("./{LEX_CASE}");
    let detour = format!("tests/../{LEX_CASE}");
    for given in [dotted.as_str(), detour.as_str()] {
        let out = ostrel(&["check", given]);
        assert_eq!(out.status.code(), Some(1), "{given}");
        let want = expected.replace(LEX_CASE, given);
        assert_eq!(text(&out.stderr), want, "{given}");
    }
    let absolute = repo().join(LEX_CASE);
    let out = ostrel(&[OsStr::new("check"), absolute.as_os_str()]);
    assert_eq!(out.status.code(), Some(1));
    let want = expected.replace(LEX_CASE, &absolute.to_string_lossy());
    assert_eq!(text(&out.stderr), want);
}

#[test]
fn file_above_the_source_limit_is_refused_before_reading() {
    let dir = scratch("too_large");
    let path = dir.join("huge.ostl");
    // Sparse: one byte above MAX_FILE_BYTES (u32::MAX) without using the disk.
    File::create(&path)
        .unwrap()
        .set_len(u64::from(u32::MAX) + 1)
        .unwrap();
    for cmd in ["check", "run"] {
        let out = ostrel(&[OsStr::new(cmd), path.as_os_str()]);
        assert_eq!(out.status.code(), Some(1), "{cmd}");
        assert!(out.stdout.is_empty(), "{cmd}");
        let err = text(&out.stderr);
        assert!(err.ends_with(": file is larger than 4 GiB\n"), "{err}");
    }
}

#[test]
fn ac_04_invalid_utf8_is_one_e0014_diagnostic_with_exit_1() {
    let dir = scratch("invalid_utf8");
    for (name, bytes, at) in [
        ("lead.ostl", &b"\xFF"[..], "1:1"),
        ("after_line.ostl", &b"fn main()\n  \xC3"[..], "2:3"),
        ("overlong.ostl", &b"fn main\xC0\x80()\n"[..], "1:8"),
    ] {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        for cmd in ["check", "run"] {
            let out = ostrel(&[OsStr::new(cmd), path.as_os_str()]);
            assert_eq!(out.status.code(), Some(1), "{cmd} {name}");
            assert!(out.stdout.is_empty(), "{cmd} {name}");
            let err = text(&out.stderr);
            // SPEC 12.6: invalid UTF-8 is E0014 at the lead byte of the first ill formed
            // sequence, from check and run, exit 1.
            let prefix = format!("{}:{at}: error[E0014]: ", path.to_string_lossy());
            assert!(err.starts_with(&prefix), "{cmd} {name}: {err}");
            assert_eq!(err.lines().count(), 1, "{cmd} {name}: {err}");
        }
    }
}

#[test]
fn ac_04_empty_file_is_e0400_at_1_1() {
    let dir = scratch("empty");
    let path = dir.join("empty.ostl");
    fs::write(&path, b"").unwrap();
    for cmd in ["check", "run"] {
        let out = ostrel(&[OsStr::new(cmd), path.as_os_str()]);
        assert!(out.stdout.is_empty(), "{cmd}");
        let err = text(&out.stderr);
        // SPEC 12.6: an empty file is E0400 at 1:1 from check and run, exit 1.
        assert_eq!(out.status.code(), Some(1), "{cmd}: {err}");
        let prefix = format!("{}:1:1: error[E0400]: ", path.to_string_lossy());
        assert!(err.starts_with(&prefix), "{cmd}: {err}");
        assert_eq!(err.lines().count(), 1, "{cmd}: {err}");
    }
}

#[test]
fn files_without_a_program_never_succeed() {
    let dir = scratch("no_program");
    for (name, bytes, code) in [
        ("bom.ostl", &b"\xEF\xBB\xBF"[..], Some("E0400")),
        ("newline.ostl", &b"\n"[..], Some("E0400")),
        ("crlf.ostl", &b"\r\n\r\n"[..], Some("E0400")),
        ("spaces.ostl", &b"   "[..], Some("E0400")),
        ("comment.ostl", &b"// nothing here\n"[..], Some("E0400")),
        ("nul.ostl", &b"\0"[..], None),
    ] {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        for cmd in ["check", "run"] {
            let out = ostrel(&[OsStr::new(cmd), path.as_os_str()]);
            let err = text(&out.stderr);
            assert_eq!(out.status.code(), Some(1), "{cmd} {name}: {err}");
            assert!(out.stdout.is_empty(), "{cmd} {name}");
            assert_eq!(err.lines().count(), 1, "{cmd} {name}: {err}");
            let prefix = format!("{}:", path.to_string_lossy());
            assert!(err.starts_with(&prefix), "{cmd} {name}: {err}");
            let tag = code.map_or_else(|| ": error[E".to_owned(), |c| format!(": error[{c}]: "));
            assert!(err.contains(&tag), "{cmd} {name}: {err}");
        }
    }
}

#[test]
fn every_error_golden_through_the_binary_is_exact() {
    let mut cases: Vec<String> = fs::read_dir(repo().join("tests/errors"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".ostl"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty());
    let mut wrong = Vec::new();
    for name in &cases {
        let rel = format!("tests/errors/{name}");
        let expected = read(&rel.replace(".ostl", ".expected_err"));
        for cmd in ["check", "run"] {
            let out = ostrel(&[cmd, rel.as_str()]);
            let err = text(&out.stderr);
            if out.status.code() != Some(1) || !out.stdout.is_empty() || err != expected {
                wrong.push(format!(
                    "{cmd} {rel}: exit {:?}\n  expected: {expected:?}\n  found:    {err:?}",
                    out.status.code()
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn every_example_through_the_binary_is_exact() {
    let mut cases: Vec<String> = fs::read_dir(repo().join("examples/v0_1"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".ostl"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty());
    for name in &cases {
        let rel = format!("examples/v0_1/{name}");
        let out = ostrel(&["run", rel.as_str()]);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{rel}: {err}");
        assert_eq!(
            text(&out.stdout),
            read(&rel.replace(".ostl", ".expected")),
            "{rel}"
        );
        assert!(err.is_empty(), "{rel}: {err}");
        let out = ostrel(&["check", rel.as_str()]);
        assert_eq!(out.status.code(), Some(0), "check {rel}");
        assert!(
            out.stdout.is_empty() && out.stderr.is_empty(),
            "check {rel}"
        );
    }
}

#[test]
fn output_before_a_runtime_error_is_flushed_before_stderr() {
    let dir = scratch("flush_order");
    let src = dir.join("div.ostl");
    fs::write(&src, "fn main()\n  print(\"before\")\n  print(7 / 0)\n").unwrap();
    // Both streams into one file: the order of the bytes is the order of the writes.
    let both = dir.join("both.txt");
    let sink = File::create(&both).unwrap();
    let status = command(&[OsStr::new("run"), src.as_os_str()])
        .stdout(sink.try_clone().unwrap())
        .stderr(sink)
        .status()
        .unwrap();
    let combined = fs::read_to_string(&both).unwrap();
    assert_eq!(status.code(), Some(1), "{combined}");
    let prefix = format!("before\n{}:3:", src.to_string_lossy());
    assert!(combined.starts_with(&prefix), "{combined}");
    assert!(
        combined.contains(": runtime error[DivisionByZero]: "),
        "{combined}"
    );
    assert_eq!(combined.lines().count(), 2, "{combined}");
}

#[cfg(target_os = "linux")]
#[test]
fn lost_program_output_is_a_failure_never_a_success() {
    let full = File::options().write(true).open("/dev/full").unwrap();
    let out = command(&["run", "examples/v0_1/01_hello.ostl"])
        .stdout(full)
        .output()
        .unwrap();
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.starts_with("ostrel: cannot write to stdout: "), "{err}");
    // No program output: a full stdout changes nothing for a diagnostic.
    let full = File::options().write(true).open("/dev/full").unwrap();
    let out = command(&["check", LEX_CASE]).stdout(full).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stderr), read(LEX_EXPECTED));
}

#[test]
fn closed_stdout_pipe_never_kills_the_process() {
    let mut child = command(&["run", "examples/v0_1/13_fizzbuzz.ostl"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    // Exit 0 if all output fit into the pipe before it closed; never a signal or a panic.
    let code = out.status.code();
    assert!(
        matches!(code, Some(0 | 1)),
        "exit {code:?}, stderr {}",
        text(&out.stderr)
    );
}

#[test]
fn parallel_runs_give_identical_results() {
    let children: Vec<_> = (0..16)
        .map(|i| {
            let cmd = if i % 2 == 0 { "check" } else { "run" };
            command(&[cmd, LEX_CASE])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let expected = read(LEX_EXPECTED);
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
        assert_eq!(text(&out.stderr), expected);
    }
}
