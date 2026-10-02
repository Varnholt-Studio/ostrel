//! Command line of `ostrel-fuzz` (std only argument parsing).

use std::path::PathBuf;
use std::time::Duration;

/// Usage text, printed on usage errors and for `help`.
pub const USAGE: &str = "\
usage:
  ostrel-fuzz run --bin PATH --target check|fmt|run [--seed N]
                  [--duration 30m | --iterations N] [--corpus DIR]...
                  [--out DIR] [--timeout-ms N] [--memory-mib N] [--max-len BYTES]
                  [--keep N]
  ostrel-fuzz gen --seed N --count N --out DIR [--corpus DIR]... [--max-len BYTES]
  ostrel-fuzz repro --seed N --iteration N --out FILE [--corpus DIR]... [--max-len BYTES]
  ostrel-fuzz help

exit codes: 0 clean, 1 findings, 2 usage or I/O error";

/// Default per input time limit of `check` and `fmt` (SPEC AC-36, 12.4).
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(1);
/// Default per input time limit of `run` (v0.3 row of `tests/hostile/README`, SPEC 12.4).
pub const RUN_TIMEOUT: Duration = Duration::from_secs(5);
/// Default memory limit in MiB (SPEC AC-36).
pub const MEMORY_MIB: u64 = 512;
/// Default upper bound for mutated inputs.
pub const MAX_LEN: usize = 64 * 1024;

/// A parsed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Run a campaign against the CLI.
    Run(RunArgs),
    /// Write generated inputs to a directory.
    Gen {
        /// Campaign seed.
        seed: u64,
        /// Number of inputs.
        count: u64,
        /// Output directory.
        out: PathBuf,
        /// Mutation seed directories.
        corpus: Vec<PathBuf>,
        /// Upper bound for mutated inputs.
        max_len: usize,
    },
    /// Rebuild one input from seed and iteration.
    Repro {
        /// Campaign seed.
        seed: u64,
        /// Iteration number.
        iteration: u64,
        /// Output file.
        out: PathBuf,
        /// Mutation seed directories.
        corpus: Vec<PathBuf>,
        /// Upper bound for mutated inputs.
        max_len: usize,
    },
    /// Print the usage text.
    Help,
}

/// Arguments of `run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    /// Path of the `ostrel` binary.
    pub bin: PathBuf,
    /// CLI subcommand under test.
    pub target: String,
    /// Campaign seed.
    pub seed: u64,
    /// Wall clock budget, if the campaign is time boxed.
    pub duration: Option<Duration>,
    /// Number of inputs, if the campaign is count boxed.
    pub iterations: Option<u64>,
    /// Mutation seed directories.
    pub corpus: Vec<PathBuf>,
    /// Directory for findings.
    pub out: PathBuf,
    /// Per input time limit.
    pub timeout: Duration,
    /// Memory limit in MiB; 0 disables it.
    pub memory_mib: u64,
    /// Upper bound for mutated inputs.
    pub max_len: usize,
    /// Findings written per outcome label.
    pub keep: usize,
}

/// Parses `args` (without the program name).
pub fn parse(args: &[String]) -> Result<Command, String> {
    let Some((cmd, rest)) = args.split_first() else {
        return Err("missing command".into());
    };
    let mut p = Parser { rest, pos: 0 };
    match cmd.as_str() {
        "help" | "--help" | "-h" => Ok(Command::Help),
        "run" => p.run(),
        "gen" => p.gen_cmd(),
        "repro" => p.repro(),
        other => Err(format!("unknown command `{other}`")),
    }
}

struct Parser<'a> {
    rest: &'a [String],
    pos: usize,
}

#[derive(Default)]
struct Flags {
    bin: Option<PathBuf>,
    target: Option<String>,
    seed: Option<u64>,
    duration: Option<Duration>,
    iterations: Option<u64>,
    iteration: Option<u64>,
    count: Option<u64>,
    corpus: Vec<PathBuf>,
    out: Option<PathBuf>,
    timeout_ms: Option<u64>,
    memory_mib: Option<u64>,
    max_len: Option<usize>,
    keep: Option<usize>,
}

impl Parser<'_> {
    fn value(&mut self, flag: &str) -> Result<String, String> {
        let v = self
            .rest
            .get(self.pos)
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))?;
        self.pos += 1;
        Ok(v)
    }

    fn flags(&mut self, allowed: &[&str]) -> Result<Flags, String> {
        let mut f = Flags::default();
        while let Some(flag) = self.rest.get(self.pos).cloned() {
            self.pos += 1;
            if !allowed.contains(&flag.as_str()) {
                return Err(format!("unknown flag `{flag}`"));
            }
            let v = self.value(&flag)?;
            match flag.as_str() {
                "--bin" => f.bin = Some(PathBuf::from(v)),
                "--target" => f.target = Some(v),
                "--seed" => f.seed = Some(number(&flag, &v)?),
                "--duration" => f.duration = Some(duration(&v)?),
                "--iterations" => f.iterations = Some(number(&flag, &v)?),
                "--iteration" => f.iteration = Some(number(&flag, &v)?),
                "--count" => f.count = Some(number(&flag, &v)?),
                "--corpus" => f.corpus.push(PathBuf::from(v)),
                "--out" => f.out = Some(PathBuf::from(v)),
                "--timeout-ms" => f.timeout_ms = Some(number(&flag, &v)?),
                "--memory-mib" => f.memory_mib = Some(number(&flag, &v)?),
                "--max-len" => f.max_len = Some(number::<usize>(&flag, &v)?),
                "--keep" => f.keep = Some(number::<usize>(&flag, &v)?),
                _ => return Err(format!("unknown flag `{flag}`")),
            }
        }
        Ok(f)
    }

    fn run(&mut self) -> Result<Command, String> {
        let f = self.flags(&[
            "--bin",
            "--target",
            "--seed",
            "--duration",
            "--iterations",
            "--corpus",
            "--out",
            "--timeout-ms",
            "--memory-mib",
            "--max-len",
            "--keep",
        ])?;
        let target = f.target.ok_or("run needs --target")?;
        let default_timeout = match target.as_str() {
            "check" | "fmt" => CHECK_TIMEOUT,
            "run" => RUN_TIMEOUT,
            other => return Err(format!("unknown target `{other}` (check, fmt or run)")),
        };
        if f.duration.is_some() && f.iterations.is_some() {
            return Err("give either --duration or --iterations, not both".into());
        }
        if f.duration.is_none() && f.iterations.is_none() {
            return Err("run needs --duration or --iterations".into());
        }
        Ok(Command::Run(RunArgs {
            bin: f.bin.ok_or("run needs --bin")?,
            target,
            seed: f.seed.unwrap_or(1),
            duration: f.duration,
            iterations: f.iterations,
            corpus: f.corpus,
            out: f
                .out
                .unwrap_or_else(|| PathBuf::from("tools/fuzz/findings")),
            timeout: f
                .timeout_ms
                .map(Duration::from_millis)
                .unwrap_or(default_timeout),
            memory_mib: f.memory_mib.unwrap_or(MEMORY_MIB),
            max_len: f.max_len.unwrap_or(MAX_LEN),
            keep: f.keep.unwrap_or(20),
        }))
    }

    fn gen_cmd(&mut self) -> Result<Command, String> {
        let f = self.flags(&["--seed", "--count", "--out", "--corpus", "--max-len"])?;
        Ok(Command::Gen {
            seed: f.seed.ok_or("gen needs --seed")?,
            count: f.count.ok_or("gen needs --count")?,
            out: f.out.ok_or("gen needs --out")?,
            corpus: f.corpus,
            max_len: f.max_len.unwrap_or(MAX_LEN),
        })
    }

    fn repro(&mut self) -> Result<Command, String> {
        let f = self.flags(&["--seed", "--iteration", "--out", "--corpus", "--max-len"])?;
        Ok(Command::Repro {
            seed: f.seed.ok_or("repro needs --seed")?,
            iteration: f.iteration.ok_or("repro needs --iteration")?,
            out: f.out.ok_or("repro needs --out")?,
            corpus: f.corpus,
            max_len: f.max_len.unwrap_or(MAX_LEN),
        })
    }
}

fn number<T: std::str::FromStr>(flag: &str, v: &str) -> Result<T, String> {
    v.parse()
        .map_err(|_| format!("{flag}: `{v}` is not a non negative number"))
}

/// Parses `90s`, `30m`, `2h` or a plain number of seconds.
pub fn duration(v: &str) -> Result<Duration, String> {
    let (digits, unit) = match v.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((i, _)) => v.split_at(i),
        None => (v, "s"),
    };
    let n: u64 = digits.parse().map_err(|_| format!("bad duration `{v}`"))?;
    let secs = match unit {
        "s" => n,
        "m" => n.checked_mul(60).ok_or("duration too large")?,
        "h" => n.checked_mul(3600).ok_or("duration too large")?,
        _ => return Err(format!("bad duration `{v}` (use s, m or h)")),
    };
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::{Command, duration, parse};
    use std::time::Duration;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn run_with_defaults() {
        let Command::Run(r) = parse(&args(
            "run --bin target/debug/ostrel --target check --duration 30m",
        ))
        .unwrap() else {
            panic!("not a run command");
        };
        assert_eq!(r.timeout, Duration::from_secs(1));
        assert_eq!(r.duration, Some(Duration::from_secs(1800)));
        assert_eq!(r.memory_mib, 512);
        assert_eq!(r.seed, 1);
    }

    #[test]
    fn run_target_sets_the_time_limit() {
        let Command::Run(r) = parse(&args("run --bin b --target run --iterations 5")).unwrap()
        else {
            panic!("not a run command");
        };
        assert_eq!(r.timeout, Duration::from_secs(5));
        let Command::Run(r) = parse(&args(
            "run --bin b --target fmt --iterations 5 --timeout-ms 250",
        ))
        .unwrap() else {
            panic!("not a run command");
        };
        assert_eq!(r.timeout, Duration::from_millis(250));
    }

    #[test]
    fn usage_errors() {
        for bad in [
            "",
            "frobnicate",
            "run --target check --iterations 1",
            "run --bin b --target lex --iterations 1",
            "run --bin b --target check",
            "run --bin b --target check --iterations 1 --duration 1m",
            "run --bin b --target check --iterations x",
            "run --bin b --target check --iterations 1 --verbose",
            "run --bin b --target check --iterations",
            "gen --seed 1 --out d",
            "repro --seed 1 --out f",
        ] {
            assert!(parse(&args(bad)).is_err(), "accepted `{bad}`");
        }
    }

    #[test]
    fn gen_and_repro() {
        assert_eq!(
            parse(&args(
                "repro --seed 3 --iteration 9 --out x.ostl --corpus a --corpus b"
            ))
            .unwrap(),
            Command::Repro {
                seed: 3,
                iteration: 9,
                out: "x.ostl".into(),
                corpus: vec!["a".into(), "b".into()],
                max_len: super::MAX_LEN,
            }
        );
        assert!(matches!(
            parse(&args("gen --seed 1 --count 4 --out d")),
            Ok(Command::Gen { count: 4, .. })
        ));
        assert_eq!(parse(&args("help")).unwrap(), Command::Help);
    }

    #[test]
    fn durations() {
        assert_eq!(duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(duration("15").unwrap(), Duration::from_secs(15));
        assert!(duration("m").is_err());
        assert!(duration("5d").is_err());
    }
}
