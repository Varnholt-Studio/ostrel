//! Runs the system under test on one input with a wall clock and a memory limit.
//!
//! Each input runs in a fresh process, so a hang can be killed and a crash cannot
//! take the fuzzer down. The memory limit is applied with the POSIX shell builtin
//! `ulimit -v` before the target is exec'd (std only, no libc binding).

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How one run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Exit code 0 or 1: accepted or rejected with a diagnostic (SPEC AC-04).
    Clean(i32),
    /// Any other exit code, including 2 (usage error) and 101 (Rust panic).
    BadExit(i32),
    /// Terminated by a signal (abort on allocation failure, stack overflow, kill).
    Signal(i32),
    /// Ran longer than the time limit and was killed.
    Timeout,
}

impl Outcome {
    /// True when the outcome is a finding (anything except exit 0 or 1).
    pub fn is_finding(&self) -> bool {
        !matches!(self, Outcome::Clean(_))
    }

    /// Short stable label for reports and file names.
    pub fn label(&self) -> String {
        match self {
            Outcome::Clean(c) => format!("exit{c}"),
            Outcome::BadExit(101) => "panic".to_string(),
            Outcome::BadExit(c) => format!("exit{c}"),
            Outcome::Signal(s) => format!("signal{s}"),
            Outcome::Timeout => "timeout".to_string(),
        }
    }
}

/// Program and limits of one fuzz target.
#[derive(Debug, Clone)]
pub struct Target {
    /// Program to run.
    pub program: PathBuf,
    /// Arguments before the input path (for example `["check"]`).
    pub args: Vec<String>,
    /// Wall clock limit per input.
    pub timeout: Duration,
    /// Address space limit in KiB (`ulimit -v`); `None` disables the limit.
    pub memory_kib: Option<u64>,
}

/// Result of one run.
#[derive(Debug, Clone)]
pub struct RunResult {
    /// How the run ended.
    pub outcome: Outcome,
    /// Wall clock time until exit or kill.
    pub elapsed: Duration,
    /// The first bytes of stderr, for the finding report.
    pub stderr_head: Vec<u8>,
}

/// Bytes of stderr kept for a report.
const STDERR_KEEP: usize = 4096;

impl Target {
    /// Runs the target on `input` (a file path). stdout is discarded; stderr is read
    /// in the background so a chatty target cannot block on a full pipe.
    pub fn run(&self, input: &Path) -> io::Result<RunResult> {
        let mut cmd = match self.memory_kib {
            Some(kib) => {
                let mut c = Command::new("/bin/sh");
                c.arg("-c")
                    .arg(format!("ulimit -v {kib} && exec \"$0\" \"$@\""))
                    .arg(&self.program);
                c
            }
            None => Command::new(&self.program),
        };
        cmd.args(&self.args)
            .arg(input)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let start = Instant::now();
        let mut child = cmd.spawn()?;
        let reader = child.stderr.take().map(|mut err| {
            thread::spawn(move || {
                let mut head = Vec::new();
                let mut buf = [0u8; 8192];
                loop {
                    match err.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let room = STDERR_KEEP.saturating_sub(head.len());
                            head.extend_from_slice(buf.get(..n.min(room)).unwrap_or(&[]));
                        }
                    }
                }
                head
            })
        });

        let mut pause = Duration::from_micros(200);
        let status: Option<ExitStatus> = loop {
            if let Some(s) = child.try_wait()? {
                break Some(s);
            }
            if start.elapsed() >= self.timeout {
                // The child may exit between try_wait and kill; both are fine.
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_millis(10));
        };
        let elapsed = start.elapsed();
        let stderr_head = reader.and_then(|h| h.join().ok()).unwrap_or_default();
        let outcome = match status {
            None => Outcome::Timeout,
            Some(s) => classify(s),
        };
        Ok(RunResult {
            outcome,
            elapsed,
            stderr_head,
        })
    }
}

fn classify(status: ExitStatus) -> Outcome {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(c @ (0 | 1)), _) => Outcome::Clean(c),
        (Some(c), _) => Outcome::BadExit(c),
        (None, Some(s)) => Outcome::Signal(s),
        (None, None) => Outcome::BadExit(-1),
    }
}

/// Writes `bytes` to `path`, creating parent directories.
pub fn write_input(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::{Outcome, Target};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn sh(script: &str, timeout_ms: u64, memory_kib: Option<u64>) -> Target {
        Target {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), script.to_string()],
            timeout: Duration::from_millis(timeout_ms),
            memory_kib,
        }
    }

    fn input() -> &'static Path {
        Path::new("/dev/null")
    }

    #[test]
    fn exit_0_and_1_are_clean() {
        assert_eq!(
            sh("exit 0", 5000, None).run(input()).unwrap().outcome,
            Outcome::Clean(0)
        );
        assert_eq!(
            sh("exit 1", 5000, None).run(input()).unwrap().outcome,
            Outcome::Clean(1)
        );
    }

    #[test]
    fn usage_error_and_panic_are_findings() {
        let two = sh("exit 2", 5000, None).run(input()).unwrap().outcome;
        assert_eq!(two, Outcome::BadExit(2));
        assert!(two.is_finding());
        let panic = sh("exit 101", 5000, None).run(input()).unwrap().outcome;
        assert_eq!(panic.label(), "panic");
    }

    #[test]
    fn signal_is_a_finding() {
        let r = sh("kill -SEGV $$", 5000, None).run(input()).unwrap();
        assert_eq!(r.outcome, Outcome::Signal(11));
    }

    #[test]
    fn hang_is_killed_at_the_time_limit() {
        let r = sh("sleep 5", 300, None).run(input()).unwrap();
        assert_eq!(r.outcome, Outcome::Timeout);
        assert!(r.elapsed < Duration::from_secs(3), "took {:?}", r.elapsed);
    }

    #[test]
    fn memory_limit_reaches_the_target() {
        let r = sh("ulimit -v >&2; exit 0", 5000, Some(524_288))
            .run(input())
            .unwrap();
        assert_eq!(r.outcome, Outcome::Clean(0));
        assert_eq!(String::from_utf8_lossy(&r.stderr_head).trim(), "524288");
    }

    #[test]
    fn input_path_is_the_last_argument() {
        // With `sh -c SCRIPT NAME`, the input path becomes $0 of the script.
        let r = sh("echo \"$0\" >&2", 5000, None).run(input()).unwrap();
        assert_eq!(String::from_utf8_lossy(&r.stderr_head).trim(), "/dev/null");
    }

    #[test]
    fn stderr_is_capped() {
        let r = sh(
            "i=0; while [ $i -lt 2000 ]; do echo 0123456789 >&2; i=$((i+1)); done",
            5000,
            None,
        )
        .run(input())
        .unwrap();
        assert_eq!(r.stderr_head.len(), super::STDERR_KEEP);
    }
}
