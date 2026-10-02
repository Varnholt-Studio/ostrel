//! `ostrel-fuzz`: seeded, std only fuzzer for the `ostrel` CLI (SPEC AC-36).

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use ostrel_fuzz::campaign::{Campaign, InputSource, Stop, load_corpus, render};
use ostrel_fuzz::cli::{self, Command, USAGE};
use ostrel_fuzz::exec::{Target, write_input};
use ostrel_fuzz::generate::GenConfig;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ostrel-fuzz: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match execute(cmd) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ostrel-fuzz: {e}");
            ExitCode::from(2)
        }
    }
}

fn source(
    seed: u64,
    corpus: &[std::path::PathBuf],
    max_len: usize,
) -> std::io::Result<InputSource> {
    Ok(InputSource {
        seed,
        corpus: load_corpus(corpus)?,
        generator: GenConfig::default(),
        max_len,
    })
}

fn corpus_args(corpus: &[std::path::PathBuf]) -> String {
    corpus
        .iter()
        .map(|d| format!(" --corpus {}", d.display()))
        .collect()
}

fn execute(cmd: Command) -> std::io::Result<ExitCode> {
    match cmd {
        Command::Help => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Gen {
            seed,
            count,
            out,
            corpus,
            max_len,
        } => {
            let src = source(seed, &corpus, max_len)?;
            for i in 0..count {
                write_input(&out.join(format!("s{seed}-i{i}.ostl")), &src.input(i))?;
            }
            println!("wrote {count} inputs to {}", out.display());
            Ok(ExitCode::SUCCESS)
        }
        Command::Repro {
            seed,
            iteration,
            out,
            corpus,
            max_len,
        } => {
            let src = source(seed, &corpus, max_len)?;
            write_input(&out, &src.input(iteration))?;
            println!("wrote {}", out.display());
            Ok(ExitCode::SUCCESS)
        }
        Command::Run(r) => {
            if !Path::new(&r.bin).is_file() {
                return Err(std::io::Error::other(format!(
                    "binary {} not found",
                    r.bin.display()
                )));
            }
            let stop = match (r.duration, r.iterations) {
                (Some(d), _) => Stop::After(d),
                (None, Some(n)) => Stop::Iterations(n),
                (None, None) => Stop::After(Duration::from_secs(60)),
            };
            let campaign = Campaign {
                name: r.target.clone(),
                target: Target {
                    program: r.bin.clone(),
                    args: vec![r.target.clone()],
                    timeout: r.timeout,
                    memory_kib: (r.memory_mib > 0).then(|| r.memory_mib * 1024),
                },
                source: source(r.seed, &r.corpus, r.max_len)?,
                stop,
                out_dir: r.out.clone(),
                work_dir: std::env::temp_dir().join(format!("ostrel-fuzz-{}", std::process::id())),
                keep_per_label: r.keep,
                corpus_args: corpus_args(&r.corpus),
            };
            let report = campaign.run()?;
            let _ = std::fs::remove_dir(&campaign.work_dir);
            print!("{}", render(&campaign, &report));
            Ok(if report.findings == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
    }
}
