//! The formatter over every Ostrel file of the repository (AC-48 round trip):
//! every file that parses formats without a defect, and formatting the output
//! again changes nothing. The goldens of `tests/fmt/` pin exact output.

use std::fs;
use std::path::{Path, PathBuf};

use ostrel_core::FileId;
use ostrel_fmt::{Refusal, format_source};

const FILE: FileId = FileId::from_raw(0);

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every file under `dir` with the extension `ext`, sorted.
fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !matches!(name, "target" | "node_modules" | ".git") {
                files(&path, ext, out)?;
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

/// The `ostl` blocks of a Markdown file, as `docs/` checks them one by one.
fn blocks(md: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current: Option<String> = None;
    for line in md.lines() {
        match current.as_mut() {
            None if line.trim_start() == "```ostl" => current = Some(String::new()),
            None => {}
            Some(_) if line.trim_start().starts_with("```") => {
                found.extend(current.take());
            }
            Some(block) => {
                block.push_str(line);
                block.push('\n');
            }
        }
    }
    found
}

/// Formats `src` twice. `Ok(None)` if it does not parse.
fn round_trip(name: &str, src: &str) -> Result<Option<String>, String> {
    let once = match format_source(src, FILE) {
        Ok(f) => f,
        Err(Refusal::Invalid(_)) => return Ok(None),
        Err(Refusal::Defect(why)) => return Err(format!("{name}: defect: {why}")),
    };
    match format_source(&once.text, FILE) {
        Ok(twice) if twice == once => Ok(Some(once.text)),
        Ok(_) => Err(format!("{name}: formatting twice differs from once")),
        Err(r) => Err(format!("{name}: output refused: {r:?}")),
    }
}

#[test]
fn every_parsing_file_of_the_repository_round_trips() -> Result<(), String> {
    let root = root();
    let mut paths = Vec::new();
    for dir in ["examples", "tests", "crates", "docs"] {
        let dir = root.join(dir);
        if dir.is_dir() {
            files(&dir, "ostl", &mut paths)?;
        }
    }
    let mut sources: Vec<(String, String)> = Vec::new();
    for path in &paths {
        // Hostile cases include invalid UTF-8 on purpose.
        let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Ok(src) = String::from_utf8(bytes) {
            sources.push((path.display().to_string(), src));
        }
    }
    let mut mds = Vec::new();
    let docs = root.join("docs");
    if docs.is_dir() {
        files(&docs, "md", &mut mds)?;
    }
    for md in &mds {
        let text = fs::read_to_string(md).map_err(|e| format!("{}: {e}", md.display()))?;
        for (i, block) in blocks(&text).into_iter().enumerate() {
            sources.push((format!("{} block {}", md.display(), i + 1), block));
        }
    }
    let mut formatted = 0;
    let mut failures = Vec::new();
    for (name, src) in &sources {
        match round_trip(name, src) {
            Ok(Some(_)) => formatted += 1,
            Ok(None) => {}
            Err(e) => failures.push(e),
        }
    }
    if !failures.is_empty() {
        return Err(failures.join("\n"));
    }
    // The examples alone are more than 30 parsing files; a walk that finds
    // far fewer has lost its directories.
    if formatted < 30 {
        return Err(format!(
            "only {formatted} of {} files formatted",
            sources.len()
        ));
    }
    Ok(())
}

#[test]
fn goldens_of_tests_fmt_give_their_formatted_text() -> Result<(), String> {
    let dir = root().join("tests/fmt");
    let mut cases = Vec::new();
    files(&dir, "ostl", &mut cases)?;
    if cases.is_empty() {
        return Err("tests/fmt has no cases".into());
    }
    let mut failures = Vec::new();
    for case in &cases {
        let name = case.display().to_string();
        let src = fs::read_to_string(case).map_err(|e| format!("{name}: {e}"))?;
        let want_path = case.with_extension("formatted");
        let want =
            fs::read_to_string(&want_path).map_err(|e| format!("{}: {e}", want_path.display()))?;
        match round_trip(&name, &src)? {
            Some(got) if got == want => {}
            Some(got) => failures.push(format!("{name}: got\n{got}\nwant\n{want}")),
            None => failures.push(format!("{name}: does not parse")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

#[test]
fn every_golden_has_a_case() -> Result<(), String> {
    let dir = root().join("tests/fmt");
    let mut wants = Vec::new();
    files(&dir, "formatted", &mut wants)?;
    let orphans: Vec<String> = wants
        .iter()
        .filter(|w| !w.with_extension("ostl").is_file())
        .map(|w| w.display().to_string())
        .collect();
    if orphans.is_empty() {
        Ok(())
    } else {
        Err(format!("expected output without a case: {orphans:?}"))
    }
}
