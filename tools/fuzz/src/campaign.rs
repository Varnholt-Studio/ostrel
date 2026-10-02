//! One fuzz campaign: derive inputs from a seed, run the target, keep findings.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::exec::{Outcome, Target, write_input};
use crate::generate::{self, GenConfig};
use crate::mutate::mutate;
use crate::rng::Rng;

/// Largest corpus file used as a mutation seed; larger files are skipped.
pub const MAX_SEED_FILE: u64 = 1 << 20;

/// When a campaign stops.
#[derive(Debug, Clone, Copy)]
pub enum Stop {
    /// After this much wall clock time.
    After(Duration),
    /// After this many inputs.
    Iterations(u64),
}

/// Inputs that do not depend on the target: seed, corpus and generator settings.
/// `(seed, iteration, corpus)` determines every input byte for byte.
#[derive(Debug, Clone)]
pub struct InputSource {
    /// Campaign seed.
    pub seed: u64,
    /// Mutation seeds, sorted by path.
    pub corpus: Vec<(PathBuf, Vec<u8>)>,
    /// Generator settings.
    pub generator: GenConfig,
    /// Upper bound for mutated inputs in bytes (generated twists may be larger).
    pub max_len: usize,
}

impl InputSource {
    /// Builds input number `iteration`.
    pub fn input(&self, iteration: u64) -> Vec<u8> {
        let mut rng = Rng::for_input(self.seed, iteration);
        if !self.corpus.is_empty() && rng.chance(50) {
            let base = rng
                .pick(&self.corpus)
                .map(|(_, b)| b.clone())
                .unwrap_or_default();
            let other = rng.pick(&self.corpus).map(|(_, b)| b.as_slice());
            let mut bytes = base;
            mutate(&mut rng, &mut bytes, other, 8, self.max_len);
            bytes
        } else {
            let mut bytes = generate::program(&mut rng, &self.generator);
            if rng.chance(25) {
                let limit = self.max_len.max(bytes.len());
                mutate(&mut rng, &mut bytes, None, 4, limit);
            }
            bytes
        }
    }
}

/// Reads every corpus file below `dirs` (recursively, sorted by path). Notes,
/// expectations and scripts are skipped, as are files above [`MAX_SEED_FILE`].
pub fn load_corpus(dirs: &[PathBuf]) -> io::Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut files = Vec::new();
    for d in dirs {
        collect(d, &mut files)?;
    }
    files.sort();
    files.dedup();
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        out.push((f.clone(), fs::read(&f)?));
    }
    Ok(out)
}

fn collect(path: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let meta = fs::metadata(path)?;
    if meta.is_file() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let skip = [".md", ".txt", ".expected", ".expected_err", ".sh", ".rs"]
            .iter()
            .any(|ext| name.ends_with(ext));
        if !skip && !name.starts_with('.') && meta.len() <= MAX_SEED_FILE {
            files.push(path.to_path_buf());
        }
        return Ok(());
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            collect(&entry?.path(), files)?;
        }
    }
    Ok(())
}

/// Settings of a campaign.
#[derive(Debug, Clone)]
pub struct Campaign {
    /// Target name used in reports and file names (`check`, `fmt`, `run`).
    pub name: String,
    /// The program under test.
    pub target: Target,
    /// Input derivation.
    pub source: InputSource,
    /// Stop condition.
    pub stop: Stop,
    /// Directory for findings.
    pub out_dir: PathBuf,
    /// Directory for the scratch input file.
    pub work_dir: PathBuf,
    /// Findings written per outcome label; further ones are only counted.
    pub keep_per_label: usize,
    /// Text of the corpus arguments, repeated in the reproduction command.
    pub corpus_args: String,
}

/// Summary of a finished campaign.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Inputs run.
    pub iterations: u64,
    /// Count per outcome label.
    pub outcomes: BTreeMap<String, u64>,
    /// Number of findings (all outcomes except exit 0 and 1).
    pub findings: u64,
    /// Files written for findings.
    pub written: Vec<PathBuf>,
    /// Slowest clean run.
    pub slowest: Duration,
    /// Iteration of the slowest clean run.
    pub slowest_iteration: u64,
    /// Total wall clock time.
    pub elapsed: Duration,
}

impl Campaign {
    /// Runs the campaign until the stop condition holds.
    pub fn run(&self) -> io::Result<Report> {
        fs::create_dir_all(&self.work_dir)?;
        let input_path = self.work_dir.join("input.ostl");
        let start = Instant::now();
        let mut report = Report::default();
        let mut kept: BTreeMap<String, usize> = BTreeMap::new();
        let mut iteration = 0u64;
        loop {
            let done = match self.stop {
                Stop::After(d) => start.elapsed() >= d,
                Stop::Iterations(n) => iteration >= n,
            };
            if done {
                break;
            }
            let bytes = self.source.input(iteration);
            write_input(&input_path, &bytes)?;
            let result = self.target.run(&input_path)?;
            let label = result.outcome.label();
            *report.outcomes.entry(label.clone()).or_default() += 1;
            if result.outcome.is_finding() {
                report.findings += 1;
                let n = kept.entry(label.clone()).or_default();
                if *n < self.keep_per_label {
                    *n += 1;
                    let path = self.save_finding(
                        iteration,
                        &bytes,
                        &result.outcome,
                        result.elapsed,
                        &result.stderr_head,
                    )?;
                    report.written.push(path);
                }
            } else if result.elapsed > report.slowest {
                report.slowest = result.elapsed;
                report.slowest_iteration = iteration;
            }
            iteration += 1;
        }
        report.iterations = iteration;
        report.elapsed = start.elapsed();
        let _ = fs::remove_file(&input_path);
        Ok(report)
    }

    fn save_finding(
        &self,
        iteration: u64,
        bytes: &[u8],
        outcome: &Outcome,
        elapsed: Duration,
        stderr_head: &[u8],
    ) -> io::Result<PathBuf> {
        let stem = format!(
            "{}-{}-s{}-i{}",
            self.name,
            outcome.label(),
            self.source.seed,
            iteration
        );
        let input = self.out_dir.join(format!("{stem}.ostl"));
        write_input(&input, bytes)?;
        let note = format!(
            "target: {} {} <input>\noutcome: {}\nelapsed_ms: {}\nseed: {}\niteration: {}\nbytes: {}\n\
             reproduce: ostrel-fuzz repro --seed {} --iteration {}{} --out {stem}.ostl\n\
             stderr (first {} bytes):\n{}\n",
            self.target.program.display(),
            self.target.args.join(" "),
            outcome.label(),
            elapsed.as_millis(),
            self.source.seed,
            iteration,
            bytes.len(),
            self.source.seed,
            iteration,
            self.corpus_args,
            stderr_head.len(),
            String::from_utf8_lossy(stderr_head),
        );
        fs::write(self.out_dir.join(format!("{stem}.txt")), note)?;
        Ok(input)
    }
}

/// Renders the report as stable, line oriented text.
pub fn render(c: &Campaign, r: &Report) -> String {
    let mut s = String::new();
    s.push_str(&format!("target: {}\n", c.name));
    s.push_str(&format!("seed: {}\n", c.source.seed));
    s.push_str(&format!("corpus_files: {}\n", c.source.corpus.len()));
    s.push_str(&format!("iterations: {}\n", r.iterations));
    s.push_str(&format!("elapsed_s: {}\n", r.elapsed.as_secs()));
    for (label, n) in &r.outcomes {
        s.push_str(&format!("outcome {label}: {n}\n"));
    }
    s.push_str(&format!(
        "slowest_clean_ms: {} (iteration {})\n",
        r.slowest.as_millis(),
        r.slowest_iteration
    ));
    s.push_str(&format!("findings: {}\n", r.findings));
    for p in &r.written {
        s.push_str(&format!("finding: {}\n", p.display()));
    }
    s.push_str(if r.findings == 0 {
        "RESULT: CLEAN\n"
    } else {
        "RESULT: FINDINGS\n"
    });
    s
}

#[cfg(test)]
mod tests {
    use super::{Campaign, InputSource, Stop, load_corpus, render};
    use crate::exec::Target;
    use crate::generate::GenConfig;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("ostrel-fuzz-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn source(seed: u64) -> InputSource {
        InputSource {
            seed,
            corpus: Vec::new(),
            generator: GenConfig::default(),
            max_len: 1 << 16,
        }
    }

    /// A fake compiler that "panics" on every input containing `else`.
    fn fake_target() -> Target {
        Target {
            program: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".into(),
                "grep -q else \"$0\" && exit 101; exit 1".into(),
            ],
            timeout: Duration::from_secs(5),
            memory_kib: Some(524_288),
        }
    }

    #[test]
    fn campaign_finds_saves_and_reproduces_crashes() {
        let dir = scratch("campaign");
        let c = Campaign {
            name: "check".into(),
            target: fake_target(),
            source: source(17),
            stop: Stop::Iterations(60),
            out_dir: dir.join("findings"),
            work_dir: dir.join("work"),
            keep_per_label: 3,
            corpus_args: String::new(),
        };
        let r = c.run().unwrap();
        assert_eq!(r.iterations, 60);
        assert!(r.findings > 0, "the generator never produced `else`");
        assert_eq!(r.written.len(), 3);
        assert_eq!(r.outcomes.values().sum::<u64>(), 60);
        assert!(render(&c, &r).ends_with("RESULT: FINDINGS\n"));
        for path in &r.written {
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            let iteration: u64 = stem.rsplit("-i").next().unwrap().parse().unwrap();
            assert_eq!(fs::read(path).unwrap(), c.source.input(iteration));
            let note = fs::read_to_string(path.with_extension("txt")).unwrap();
            assert!(note.contains("outcome: panic"));
            assert!(note.contains(&format!("--iteration {iteration}")));
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clean_target_reports_clean() {
        let dir = scratch("clean");
        let c = Campaign {
            name: "fmt".into(),
            target: Target {
                program: PathBuf::from("/bin/sh"),
                args: vec!["-c".into(), "exit 0".into()],
                timeout: Duration::from_secs(5),
                memory_kib: None,
            },
            source: source(1),
            stop: Stop::Iterations(10),
            out_dir: dir.join("findings"),
            work_dir: dir.join("work"),
            keep_per_label: 3,
            corpus_args: String::new(),
        };
        let r = c.run().unwrap();
        assert_eq!(r.findings, 0);
        assert!(render(&c, &r).ends_with("RESULT: CLEAN\n"));
        assert!(!dir.join("findings").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn corpus_is_sorted_filtered_and_used() {
        let dir = scratch("corpus");
        fs::write(dir.join("b.ostl"), b"fn main()\n  print(2)\n").unwrap();
        fs::write(dir.join("a.ostl"), b"fn main()\n  print(1)\n").unwrap();
        fs::write(dir.join("README.md"), b"notes").unwrap();
        fs::write(dir.join("a.expected"), b"1\n").unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("bytes.bin"), [0xffu8, 0x00]).unwrap();
        let corpus = load_corpus(std::slice::from_ref(&dir)).unwrap();
        let names: Vec<String> = corpus
            .iter()
            .map(|(p, _)| p.strip_prefix(&dir).unwrap().display().to_string())
            .collect();
        assert_eq!(names, ["a.ostl", "b.ostl", "sub/bytes.bin"]);
        let mut s = source(4);
        s.corpus = corpus;
        let a: Vec<Vec<u8>> = (0..50).map(|i| s.input(i)).collect();
        let b: Vec<Vec<u8>> = (0..50).map(|i| s.input(i)).collect();
        assert_eq!(a, b);
        fs::remove_dir_all(&dir).unwrap();
    }
}
