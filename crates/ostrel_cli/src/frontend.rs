//! The one place where the CLI calls the parser (WP T1-3, T1-3b).
//!
//! [`parse`] is a single call to the frozen parser entry
//! `ostrel_syntax::parse::parse` (ARCHITECTURE 3).

use ostrel_core::{Diagnostic, FileId};
use ostrel_syntax::ast::Module;
use ostrel_syntax::lex::Token;

/// Parses the tokens of one file.
///
/// `lexed` holds the diagnostics the lexer reported for `tokens`, so the
/// parser can keep to one diagnostic per fault (SPEC 12.1).
pub fn parse(
    src: &str,
    file: FileId,
    tokens: &[Token],
    lexed: &[Diagnostic],
) -> (Module, Vec<Diagnostic>) {
    ostrel_syntax::parse::parse(src, file, tokens, lexed)
}

#[cfg(test)]
mod tests {
    //! Every script of `examples/v0_1` runs through the real parser and the
    //! whole pipeline, as under `ostrel run`, with byte exact output.

    use std::fs;
    use std::path::{Path, PathBuf};

    use ostrel_vm::{Host, Limits};

    use crate::pipeline::{self, Exit, Mode};

    /// The number of committed v0.1 examples (`examples/v0_1/README.md`).
    const EXAMPLES: usize = 13;

    #[derive(Default)]
    struct Capture(String);

    impl Host for Capture {
        fn print(&mut self, text: &str) {
            self.0.push_str(text);
            self.0.push('\n');
        }
    }

    fn dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/v0_1")
    }

    fn examples() -> Result<Vec<String>, String> {
        let entries = fs::read_dir(dir()).map_err(|e| e.to_string())?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            if let Ok(name) = entry.file_name().into_string()
                && let Some(stem) = name.strip_suffix(".ostl")
            {
                names.push(stem.to_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    #[test]
    fn every_v0_1_example_runs_with_its_expected_output() -> Result<(), String> {
        let names = examples()?;
        if names.len() != EXAMPLES {
            return Err(format!("expected {EXAMPLES} examples, found {names:?}"));
        }
        let mut failures = Vec::new();
        for stem in &names {
            let path = format!("examples/v0_1/{stem}.ostl");
            let bytes = fs::read(dir().join(format!("{stem}.ostl"))).map_err(|e| e.to_string())?;
            let expected = fs::read_to_string(dir().join(format!("{stem}.expected")))
                .map_err(|e| format!("{stem}.expected: {e}"))?;
            let mut out = Capture::default();
            let mut err = String::new();
            let mode = Mode::Run(Limits::default());
            let exit = pipeline::execute(mode, &path, bytes, &mut out, &mut err);
            if exit != Exit::Success || out.0 != expected || !err.is_empty() {
                failures.push(format!("{path}: {exit:?}, stderr {err:?}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("\n"))
        }
    }

    #[test]
    fn check_accepts_every_v0_1_example() -> Result<(), String> {
        for stem in examples()? {
            let path = format!("examples/v0_1/{stem}.ostl");
            let bytes = fs::read(dir().join(format!("{stem}.ostl"))).map_err(|e| e.to_string())?;
            let mut out = Capture::default();
            let mut err = String::new();
            let exit = pipeline::execute(Mode::Check, &path, bytes, &mut out, &mut err);
            if exit != Exit::Success || !out.0.is_empty() || !err.is_empty() {
                return Err(format!("{path}: {exit:?}, stderr {err:?}"));
            }
        }
        Ok(())
    }
}
