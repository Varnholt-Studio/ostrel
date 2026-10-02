//! Command line arguments of `ostrel` (ARCHITECTURE 3.4, SPEC AC-52).
//!
//! Grammar:
//!
//! ```text
//! ostrel run [--max-steps N] FILE
//! ostrel check FILE
//! ostrel --help | --version
//! ```
//!
//! Flags may stand before or after `FILE`; `--` ends the flags. Anything else
//! is a usage error, which the CLI reports with exit code 2.

use std::ffi::OsString;
use std::fmt;

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `ostrel run`: compile the file and run its `fn main()`.
    Run {
        /// The source file, exactly as given on the command line.
        file: OsString,
        /// Value of `--max-steps`, if given.
        max_steps: Option<u64>,
    },
    /// `ostrel check`: compile the file and report diagnostics only.
    Check {
        /// The source file, exactly as given on the command line.
        file: OsString,
    },
    /// `ostrel --help`.
    Help,
    /// `ostrel --version`.
    Version,
}

/// A command line the CLI does not accept (exit code 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The usage text, printed for `--help` and after a usage error.
pub const USAGE: &str = "usage: ostrel run [--max-steps N] FILE\n       ostrel check FILE\n       ostrel --help | --version\n";

/// Parses the arguments after the program name.
pub fn parse<I>(args: I) -> Result<Command, UsageError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let Some(sub) = args.next() else {
        return Err(UsageError("missing command".to_string()));
    };
    let run = match sub.to_str() {
        Some("run") => true,
        Some("check") => false,
        Some("-h" | "--help" | "help") => return Ok(Command::Help),
        Some("--version") => return Ok(Command::Version),
        _ => {
            return Err(UsageError(format!(
                "unknown command `{}`",
                sub.to_string_lossy()
            )));
        }
    };

    let mut file: Option<OsString> = None;
    let mut max_steps: Option<u64> = None;
    let mut flags_done = false;
    while let Some(arg) = args.next() {
        let text = arg.to_str();
        let is_flag = !flags_done && text.is_some_and(|t| t.starts_with('-') && t != "-");
        if !is_flag {
            if file.is_some() {
                return Err(UsageError(format!(
                    "unexpected argument `{}`; give exactly one file",
                    arg.to_string_lossy()
                )));
            }
            file = Some(arg);
            continue;
        }
        let flag = text.unwrap_or_default();
        match flag {
            "--" => flags_done = true,
            "--max-steps" if run => {
                let value = args
                    .next()
                    .ok_or_else(|| UsageError("`--max-steps` needs a value".to_string()))?;
                max_steps = Some(steps(&value.to_string_lossy())?);
            }
            _ if run && flag.starts_with("--max-steps=") => {
                max_steps = Some(steps(flag.trim_start_matches("--max-steps="))?);
            }
            _ => return Err(UsageError(format!("unknown flag `{flag}`"))),
        }
    }
    let Some(file) = file else {
        return Err(UsageError("missing file argument".to_string()));
    };
    Ok(if run {
        Command::Run { file, max_steps }
    } else {
        Command::Check { file }
    })
}

/// Parses the value of `--max-steps`: a decimal number of at least 1.
fn steps(value: &str) -> Result<u64, UsageError> {
    match value.parse::<u64>() {
        Ok(n) if n >= 1 && value.bytes().all(|b| b.is_ascii_digit()) => Ok(n),
        _ => Err(UsageError(format!(
            "`--max-steps` needs a whole number of at least 1, found `{value}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Command, UsageError> {
        parse(args.iter().map(OsString::from))
    }

    fn run(file: &str, max_steps: Option<u64>) -> Command {
        Command::Run {
            file: file.into(),
            max_steps,
        }
    }

    #[test]
    fn run_and_check_take_one_file() {
        assert_eq!(p(&["run", "a.ostl"]), Ok(run("a.ostl", None)));
        assert_eq!(
            p(&["check", "a.ostl"]),
            Ok(Command::Check {
                file: "a.ostl".into()
            })
        );
    }

    #[test]
    fn max_steps_before_or_after_the_file() {
        assert_eq!(
            p(&["run", "--max-steps", "1000", "a.ostl"]),
            Ok(run("a.ostl", Some(1000)))
        );
        assert_eq!(
            p(&["run", "a.ostl", "--max-steps=7"]),
            Ok(run("a.ostl", Some(7)))
        );
    }

    #[test]
    fn double_dash_ends_the_flags() {
        assert_eq!(p(&["run", "--", "-x.ostl"]), Ok(run("-x.ostl", None)));
        assert_eq!(p(&["run", "-"]), Ok(run("-", None)));
    }

    #[test]
    fn help_and_version() {
        assert_eq!(p(&["--help"]), Ok(Command::Help));
        assert_eq!(p(&["-h"]), Ok(Command::Help));
        assert_eq!(p(&["--version"]), Ok(Command::Version));
    }

    #[test]
    fn usage_errors() {
        for bad in [
            &[][..],
            &["build", "a.ostl"][..],
            &["run"][..],
            &["check"][..],
            &["run", "--no-such-flag-55", "a.ostl"][..],
            &["check", "--max-steps", "5", "a.ostl"][..],
            &["check", "--list-elevated", "a.ostl"][..],
            &["run", "a.ostl", "b.ostl"][..],
            &["run", "--max-steps"][..],
            &["run", "--max-steps", "0", "a.ostl"][..],
            &["run", "--max-steps", "-1", "a.ostl"][..],
            &["run", "--max-steps", "+5", "a.ostl"][..],
            &["run", "--max-steps=", "a.ostl"][..],
            &["run", "--max-steps", "99999999999999999999999", "a.ostl"][..],
        ] {
            assert!(p(bad).is_err(), "{bad:?} must be a usage error");
        }
    }
}
