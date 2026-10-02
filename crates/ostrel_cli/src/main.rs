//! Command line interface (`ostrel`) of the Ostrel programming language.
//!
//! `ostrel run FILE` compiles a script program through the real pipeline and
//! runs its `fn main()`; `ostrel check FILE` stops after the checker
//! (ARCHITECTURE 3.4, G15). Exit codes: 0 success, 1 any diagnostic or runtime
//! error, 2 usage error. Exit code 70 marks a defect of the compiler itself.
//!
//! The pipeline runs on a thread with a 64 MiB stack, because the recursive
//! compiler passes are bounded by the AST height limit, not by the size of the
//! main thread's stack. Program output is buffered and flushed before any
//! error text is written to stderr.

// This crate handles untrusted input: no unwrap, expect, panic or unchecked indexing.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod args;
mod frontend;
mod pipeline;

use std::ffi::OsString;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::process::ExitCode;
use std::thread;

use ostrel_core::MAX_FILE_BYTES;
use ostrel_vm::{Host, Limits};

use args::{Command, USAGE};
use pipeline::{Exit, Mode};

/// Stack size of the pipeline thread (ARCHITECTURE 3.4).
const PIPELINE_STACK: usize = 64 * 1024 * 1024;

/// Exit code reported when the pipeline thread panicked, the same as a panic
/// of the main thread. Always a bug (AC-04).
const PANIC_EXIT: u8 = 101;

fn main() -> ExitCode {
    let command = match args::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(e) => {
            let _ = write!(io::stderr().lock(), "ostrel: {e}\n{USAGE}");
            return ExitCode::from(Exit::Usage.code());
        }
    };
    let code = match command {
        Command::Help => {
            let _ = io::stdout().lock().write_all(USAGE.as_bytes());
            Exit::Success.code()
        }
        Command::Version => {
            let _ = writeln!(io::stdout().lock(), "ostrel {}", env!("CARGO_PKG_VERSION"));
            Exit::Success.code()
        }
        Command::Check { file } => on_pipeline_thread(Mode::Check, file),
        Command::Run { file, max_steps } => {
            let mut limits = Limits::default();
            if let Some(steps) = max_steps {
                limits.max_steps = steps;
            }
            on_pipeline_thread(Mode::Run(limits), file)
        }
    };
    ExitCode::from(code)
}

/// Runs [`compile_file`] on the pipeline thread and returns its exit code.
fn on_pipeline_thread(mode: Mode, file: OsString) -> u8 {
    let spawned = thread::Builder::new()
        .name("ostrel-pipeline".to_string())
        .stack_size(PIPELINE_STACK)
        .spawn(move || compile_file(mode, &file));
    match spawned {
        Ok(handle) => handle.join().unwrap_or(PANIC_EXIT),
        Err(e) => {
            let _ = writeln!(
                io::stderr().lock(),
                "ostrel: internal error: cannot start the pipeline thread: {e}"
            );
            Exit::Internal.code()
        }
    }
}

/// Reads `file`, runs the pipeline, flushes stdout and then writes the error
/// text to stderr.
fn compile_file(mode: Mode, file: &OsString) -> u8 {
    let path = file.to_string_lossy();
    let mut err = String::new();
    let mut host = StdoutHost::new();
    let exit = match read_source(file) {
        Ok(bytes) => pipeline::execute(mode, &path, bytes, &mut host, &mut err),
        Err(e) => {
            err.push_str(&format!(
                "ostrel: cannot read {}: {e}\n",
                pipeline::escape(&path)
            ));
            Exit::Failure
        }
    };
    let flushed = host.finish();
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(err.as_bytes());
    let mut exit = exit;
    if let Err(e) = flushed {
        // Lost program output is a failed run, never a silent success.
        let _ = writeln!(stderr, "ostrel: cannot write to stdout: {e}");
        if exit == Exit::Success {
            exit = Exit::Failure;
        }
    }
    let _ = stderr.flush();
    exit.code()
}

/// Reads a source file, refusing files the source map cannot hold before
/// reading them.
fn read_source(file: &OsString) -> io::Result<Vec<u8>> {
    let meta = fs::metadata(file)?;
    if meta.is_dir() {
        return Err(io::Error::other("is a directory"));
    }
    if usize::try_from(meta.len()).map_or(true, |len| len > MAX_FILE_BYTES) {
        return Err(io::Error::other("file is larger than 4 GiB"));
    }
    fs::read(file)
}

/// Writes program output to stdout through a buffer. The first write error
/// is kept and ends all further output.
struct StdoutHost {
    out: BufWriter<io::Stdout>,
    error: Option<io::Error>,
}

impl StdoutHost {
    fn new() -> Self {
        StdoutHost {
            out: BufWriter::new(io::stdout()),
            error: None,
        }
    }

    /// Flushes the buffer and reports the first write error, if any.
    fn finish(mut self) -> io::Result<()> {
        if let Some(e) = self.error.take() {
            return Err(e);
        }
        self.out.flush()
    }
}

impl Host for StdoutHost {
    fn print(&mut self, text: &str) {
        if self.error.is_some() {
            return;
        }
        let written = self
            .out
            .write_all(text.as_bytes())
            .and_then(|()| self.out.write_all(b"\n"));
        if let Err(e) = written {
            self.error = Some(e);
        }
    }
}

#[cfg(test)]
mod tests;
