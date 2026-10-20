//! `OutputLimit` through the binary (SPEC 12.4, AC-52, D113): `print` writes at most
//! 268435456 bytes in one run. There is no golden, because its expected file would
//! hold 256 MiB, so this test checks stdout by size and content.

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Prints one text of 16 MiB (the largest `Text`) until the output limit refuses a
/// line. Each line is 16777217 bytes with its newline, so 15 lines fit (251658255
/// bytes) and the 16th would exceed 268435456 bytes.
const PROGRAM: &str = "\
fn double(t: Text, rounds: Int) -> Text
  if rounds == 0
    return t
  return double(\"{t}{t}\", rounds - 1)

fn spam(t: Text, n: Int) -> Int
  print(t)
  if n == 0
    return 0
  return spam(t, n - 1)

fn main()
  let big = double(\"a\", 24)
  print(spam(big, 9000))
";

const LINE: usize = 16 * 1024 * 1024;
const LINES: usize = 15;

/// Removes the files of the test also when an assertion fails.
struct Scratch(Vec<PathBuf>);

impl Drop for Scratch {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

#[test]
fn ac_52_output_limit() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let source = dir.join("ac_52_output_limit.ostl");
    let stdout = dir.join("ac_52_output_limit.stdout");
    let _scratch = Scratch(vec![source.clone(), stdout.clone()]);
    fs::write(&source, PROGRAM).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_ostrel"))
        .arg("run")
        .arg(&source)
        .stdout(Stdio::from(File::create(&stdout).unwrap()))
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(1));
    let expected_err = format!(
        "{}:7:3: runtime error[OutputLimit]: program output would exceed 268435456 bytes\n",
        source.display()
    );
    assert_eq!(String::from_utf8(out.stderr).unwrap(), expected_err);

    // Exactly the completed prints: 15 whole lines, no part of the refused one.
    let size = fs::metadata(&stdout).unwrap().len();
    assert_eq!(size, (LINES * (LINE + 1)) as u64);
    assert!(size <= 268_435_456);
    let mut reader = BufReader::new(File::open(&stdout).unwrap());
    let mut line = vec![0u8; LINE + 1];
    for index in 0..LINES {
        reader.read_exact(&mut line).unwrap();
        let (text, newline) = line.split_at(LINE);
        assert!(text.iter().all(|b| *b == b'a'), "line {index}");
        assert_eq!(newline, b"\n", "line {index}");
    }
    assert_eq!(reader.read(&mut line).unwrap(), 0);
}
