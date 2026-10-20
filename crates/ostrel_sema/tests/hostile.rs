//! Hostile input hardening of the checker under `ostrel run` (gap G-1, SPEC 9, D41, AC-04).
//!
//! Every source here reaches `ostrel_sema::check` the way the CLI pipeline does: UTF 8
//! decoding, lexer, parser, and the checker only when lexer and parser reported no error
//! (SPEC 12.1). The inputs are the committed hostile corpus (`tests/hostile/cases/`),
//! seeded mutations of it and of the v0.1 examples, and generated programs that are valid
//! for the parser but large or deep for the checker.
//!
//! For every input the checker must:
//! * return instead of panicking,
//! * give the same tables and diagnostics on a second run (D74, deterministic output),
//! * place every diagnostic inside the checked file, on character boundaries,
//! * report an error whenever it leaves the entry point unresolved, so lowering is never
//!   reached without `fn main()`.
//!
//! The checker runs on a thread with the stack size of the CLI pipeline thread (64 MiB,
//! INT-4), because that is the stack it gets under `ostrel run`.

use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::thread;

use ostrel_core::{Diagnostic, FileId};
use ostrel_sema::{Checked, check};
use ostrel_syntax::ast::Module;

/// Stack of the CLI pipeline thread (`crates/ostrel_cli/src/main.rs`).
const PIPELINE_STACK: usize = 64 * 1024 * 1024;

/// Mutations per seed program in [`mutated_programs_never_break_the_checker`].
const MUTATIONS_PER_SEED: u64 = 24;

/// Base seed of the mutations. Changing it changes the inputs, never the expectations.
const SEED: u64 = 0x6f73_7472_656c_0044;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The committed `.ostl` files of `dir`, sorted by name so failures are reproducible.
fn ostl_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ostl"))
        .map(|path| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let bytes =
                fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            (name, bytes)
        })
        .collect();
    files.sort();
    files
}

fn corpus() -> Vec<(String, Vec<u8>)> {
    let cases = ostl_files(&repo_root().join("tests/hostile/cases"));
    assert!(
        cases.len() >= 50,
        "the committed hostile corpus has only {} cases",
        cases.len()
    );
    cases
}

/// What the pipeline stages before the checker did with one input.
enum Front {
    /// The lexer or parser reported an error: the checker does not run (SPEC 12.1).
    Rejected,
    /// The checker runs on this tree.
    Parsed(Module),
}

fn front(src: &str, file: FileId) -> Front {
    let (tokens, mut diags) = ostrel_syntax::lex::lex(src, file);
    let (module, parsed) = ostrel_syntax::parse::parse(src, file, &tokens, &diags);
    diags.extend(parsed);
    if diags.iter().any(Diagnostic::is_error) {
        Front::Rejected
    } else {
        Front::Parsed(module)
    }
}

/// Outcome of one input, for the counts the tests assert on.
#[derive(Debug, Default)]
struct Tally {
    not_utf8: usize,
    rejected: usize,
    accepted: usize,
    diagnosed: usize,
}

/// Runs one input through the stages up to the checker and asserts the checker contract.
/// Returns a description of the first broken rule, or `None`.
fn probe(name: &str, bytes: &[u8], tally: &mut Tally) -> Option<String> {
    let Ok(src) = std::str::from_utf8(bytes) else {
        tally.not_utf8 += 1;
        return None;
    };
    let file = FileId::from_raw(0);
    let module = match front(src, file) {
        Front::Rejected => {
            tally.rejected += 1;
            return None;
        }
        Front::Parsed(module) => module,
    };
    let first = panic::catch_unwind(AssertUnwindSafe(|| check(&module, file, src)));
    let Ok((checked, diags)) = first else {
        return Some(format!("{name}: the checker panicked"));
    };
    if let Some(fault) = diagnostics_in_file(name, src, file, &diags) {
        return Some(fault);
    }
    let errors = diags.iter().any(Diagnostic::is_error);
    if !errors && checked.main.is_none() {
        return Some(format!(
            "{name}: accepted without an entry point, lowering would fail with exit 70"
        ));
    }
    let second = panic::catch_unwind(AssertUnwindSafe(|| check(&module, file, src)));
    let Ok(again) = second else {
        return Some(format!("{name}: the checker panicked on the second run"));
    };
    if !same(&(checked, diags), &again) {
        return Some(format!("{name}: two runs of the checker disagree"));
    }
    if errors {
        tally.diagnosed += 1;
    } else {
        tally.accepted += 1;
    }
    None
}

fn same(a: &(Checked, Vec<Diagnostic>), b: &(Checked, Vec<Diagnostic>)) -> bool {
    a.0 == b.0 && a.1 == b.1
}

fn diagnostics_in_file(
    name: &str,
    src: &str,
    file: FileId,
    diags: &[Diagnostic],
) -> Option<String> {
    for d in diags {
        let (start, end) = (d.span.start as usize, d.span.end as usize);
        if d.span.file != file {
            return Some(format!("{name}: diagnostic {d:?} names another file"));
        }
        if start > end || end > src.len() {
            return Some(format!("{name}: diagnostic {d:?} lies outside the source"));
        }
        if !src.is_char_boundary(start) || !src.is_char_boundary(end) {
            return Some(format!(
                "{name}: diagnostic {d:?} splits a character of the source"
            ));
        }
        if d.message.is_empty() || d.message.contains('\n') {
            return Some(format!(
                "{name}: diagnostic {d:?} has an empty or multi line message"
            ));
        }
    }
    None
}

/// Runs `cases` on a thread with the stack of the CLI pipeline and fails with every broken
/// rule at once.
fn probe_all(cases: Vec<(String, Vec<u8>)>) -> Tally {
    let outcome = thread::Builder::new()
        .name("hostile-sema".to_string())
        .stack_size(PIPELINE_STACK)
        .spawn(move || {
            let mut tally = Tally::default();
            let faults: Vec<String> = cases
                .iter()
                .filter_map(|(name, bytes)| probe(name, bytes, &mut tally))
                .collect();
            (tally, faults)
        })
        .unwrap_or_else(|e| panic!("cannot start the checker thread: {e}"))
        .join();
    let Ok((tally, faults)) = outcome else {
        panic!("the checker thread ended abnormally");
    };
    assert!(faults.is_empty(), "checker faults:\n{}", faults.join("\n"));
    tally
}

#[test]
fn committed_corpus_never_breaks_the_checker() {
    let tally = probe_all(corpus());
    // The corpus must keep reaching the checker; a corpus that only the lexer and parser
    // see would make this test empty.
    assert!(
        tally.accepted >= 10,
        "only {} corpus cases pass the checker: {tally:?}",
        tally.accepted
    );
    assert!(
        tally.diagnosed >= 5,
        "only {} corpus cases get a checker diagnostic: {tally:?}",
        tally.diagnosed
    );
}

/// SplitMix64, the generator of `tests/hostile/gen`: small and the same on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Fragments that name the checker's rules: types, the entry point, `print`, the `Int`
/// bounds of D24, names outside the v0.1 slice and shapes of calls and interpolation.
const FRAGMENTS: &[&str] = &[
    "main",
    "print",
    "print(",
    "fn main()\n",
    "fn f(a: Int, a: Int) -> Int\n  a\n",
    "fn main()\n  main()\n",
    "fn print(x: Int)\n  x\n",
    "9007199254740991",
    "-9007199254740991",
    "9007199254740992",
    "Int",
    "Text",
    "Bool",
    "Float",
    "Map",
    "List",
    "Nope",
    "true",
    "false",
    " == ",
    " + ",
    " and ",
    " or ",
    "not ",
    "(",
    ")",
    ", ",
    ": ",
    " -> Int",
    " -> Text",
    " -> Unit",
    "\"{x}\"",
    "\"{print(1)}\"",
    "\"{main()}\"",
    "let x = ",
    "let x: Int = ",
    "let x: Text = 1\n",
    "return ",
    "return\n",
    "if ",
    "else\n",
    "\n  ",
    "\n    ",
    "\n",
    "x",
    "f(1)",
    "f(1, 2, 3)",
];

/// Applies one seeded mutation to `src`: replace, insert, delete or duplicate a slice at a
/// character boundary, so the result stays UTF 8 and reaches the lexer.
fn mutate(src: &str, rng: &mut Rng) -> String {
    let bounds: Vec<usize> = src
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(src.len()))
        .collect();
    let mut a = bounds[rng.below(bounds.len())];
    let mut b = bounds[rng.below(bounds.len())];
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    let fragment = FRAGMENTS[rng.below(FRAGMENTS.len())];
    match rng.below(4) {
        0 => format!("{}{fragment}{}", &src[..a], &src[b..]),
        1 => format!("{}{fragment}{}", &src[..a], &src[a..]),
        2 => format!("{}{}", &src[..a], &src[b..]),
        _ => format!("{}{}{}", &src[..b], &src[a..b], &src[b..]),
    }
}

/// Seed programs: the UTF 8 corpus cases and the v0.1 examples.
fn seed_programs() -> Vec<(String, String)> {
    let mut seeds: Vec<(String, String)> = corpus()
        .into_iter()
        .chain(ostl_files(&repo_root().join("examples/v0_1")))
        .filter_map(|(name, bytes)| String::from_utf8(bytes).ok().map(|s| (name, s)))
        .filter(|(_, s)| s.len() <= 64 * 1024)
        .collect();
    seeds.sort();
    seeds
}

fn mutations() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for (index, (name, src)) in seed_programs().into_iter().enumerate() {
        let mut rng = Rng(SEED ^ (index as u64).wrapping_mul(0x1000_0000_01b3));
        for round in 0..MUTATIONS_PER_SEED {
            let mut text = mutate(&src, &mut rng);
            if round % 3 == 2 {
                text = mutate(&text, &mut rng);
            }
            out.push((format!("{name} mutation {round}"), text.into_bytes()));
        }
    }
    out
}

#[test]
fn mutated_programs_never_break_the_checker() {
    let cases = mutations();
    let first: Vec<String> = cases.iter().map(|(n, b)| format!("{n}:{b:?}")).collect();
    let again: Vec<String> = mutations()
        .iter()
        .map(|(n, b)| format!("{n}:{b:?}"))
        .collect();
    assert_eq!(
        first, again,
        "the mutations must be reproducible from the seed"
    );
    let tally = probe_all(cases);
    assert!(
        tally.accepted + tally.diagnosed >= 100,
        "too few mutations reach the checker: {tally:?}"
    );
}

/// `n` functions, each calling the one before it, and a `main` that calls the last.
fn call_chain(n: usize) -> String {
    let mut s = String::from("fn f0(a: Int) -> Int\n  a\n");
    for i in 1..n {
        s.push_str(&format!(
            "\nfn f{i}(a: Int) -> Int\n  return f{}(a) + 1\n",
            i - 1
        ));
    }
    s.push_str(&format!("\nfn main()\n  print(f{}(0))\n", n - 1));
    s
}

/// One `main` with `n` bindings, each using the one before it.
fn let_chain(n: usize) -> String {
    let mut s = String::from("fn main()\n  let x0 = 0\n");
    for i in 1..n {
        s.push_str(&format!("  let x{i} = x{} + 1\n", i - 1));
    }
    s.push_str(&format!("  print(x{})\n", n - 1));
    s
}

/// One name bound `n` times in the same block.
fn rebinding(n: usize) -> String {
    let mut s = String::from("fn main()\n");
    for _ in 0..n {
        s.push_str("  let x = 1\n");
    }
    s.push_str("  print(x)\n");
    s
}

/// A function with `n` parameters and a call with `n` arguments.
fn wide_call(n: usize) -> String {
    let params: Vec<String> = (0..n).map(|i| format!("p{i}: Int")).collect();
    let args: Vec<String> = (0..n).map(|i| i.to_string()).collect();
    format!(
        "fn wide({}) -> Int\n  p0\n\nfn main()\n  print(wide({}))\n",
        params.join(", "),
        args.join(", ")
    )
}

/// `n` calls of names that do not exist: one diagnostic each, no panic, no blow up.
fn unknown_names(n: usize) -> String {
    let mut s = String::from("fn main()\n");
    for i in 0..n {
        s.push_str(&format!("  missing{i}(x{i})\n"));
    }
    s
}

/// `n` functions with the same name.
fn duplicate_fns(n: usize) -> String {
    let mut s = String::new();
    for _ in 0..n {
        s.push_str("fn main()\n  print(1)\n\n");
    }
    s
}

/// `if` blocks nested `n` deep, each returning on both arms.
fn nested_ifs(n: usize) -> String {
    let mut s = String::from("fn pick(a: Int) -> Int\n");
    for depth in 0..n {
        let pad = "  ".repeat(depth + 1);
        s.push_str(&format!("{pad}if a > {depth}\n"));
    }
    s.push_str(&format!("{}return a\n", "  ".repeat(n + 1)));
    for depth in (0..n).rev() {
        let pad = "  ".repeat(depth + 1);
        s.push_str(&format!("{pad}return {depth}\n"));
    }
    s.push_str("\nfn main()\n  print(pick(3))\n");
    s
}

/// Parentheses nested `n` deep around an `Int` operation.
fn nested_parens(n: usize) -> String {
    format!(
        "fn main()\n  print({}1 + 2{})\n",
        "(".repeat(n),
        ")".repeat(n)
    )
}

/// One expression with `n` operands of mixed types: every `+` after the first `Text`
/// operand is E0300, every comparison of different types E0304.
fn mixed_chain(n: usize) -> String {
    let operands: Vec<&str> = (0..n)
        .map(|i| match i % 4 {
            0 => "1",
            1 => "\"a\"",
            2 => "true",
            _ => "x",
        })
        .collect();
    format!(
        "fn main()\n  let x = 1\n  print({})\n",
        operands.join(" + ")
    )
}

/// `n` functions, each interpolating the result of the one before it. A string literal
/// inside an interpolation is E0006, so the nesting goes through calls.
fn interpolation_chain(n: usize) -> String {
    let mut s = String::from("fn t0(a: Int) -> Text\n  \"{a}\"\n");
    for i in 1..n {
        s.push_str(&format!(
            "\nfn t{i}(a: Int) -> Text\n  \"<{{t{}(a + 1)}}|{{a}}>\"\n",
            i - 1
        ));
    }
    s.push_str(&format!("\nfn main()\n  print(t{}(0))\n", n - 1));
    s
}

/// Very long names (512 KiB each), the `Int` bounds of D24 and their negation.
fn extremes() -> String {
    let long = "n".repeat(512 * 1024);
    let other = "m".repeat(512 * 1024);
    format!(
        "fn {long}(a: Int) -> Int\n  a\n\nfn main()\n  let {other} = 9007199254740991\n  \
         print({long}({other}))\n  print(-9007199254740991)\n  print(-(-9007199254740991))\n"
    )
}

#[test]
fn large_and_deep_programs_never_break_the_checker() {
    let cases: Vec<(String, String)> = vec![
        ("call_chain_5000".into(), call_chain(5_000)),
        ("let_chain_20000".into(), let_chain(20_000)),
        ("rebinding_20000".into(), rebinding(20_000)),
        ("wide_call_2000".into(), wide_call(2_000)),
        ("unknown_names_20000".into(), unknown_names(20_000)),
        ("duplicate_fns_5000".into(), duplicate_fns(5_000)),
        ("nested_ifs_100".into(), nested_ifs(100)),
        ("nested_parens_200".into(), nested_parens(200)),
        ("mixed_chain_1999".into(), mixed_chain(1_999)),
        (
            "interpolation_chain_2000".into(),
            interpolation_chain(2_000),
        ),
        ("extremes".into(), extremes()),
    ];
    let names: Vec<String> = cases.iter().map(|(n, _)| n.clone()).collect();
    let tally = probe_all(
        cases
            .into_iter()
            .map(|(n, s)| (n, s.into_bytes()))
            .collect(),
    );
    // Every generated program is valid for the lexer and parser, so each one must reach
    // the checker; otherwise the case tests the parser and not the checker.
    assert_eq!(
        tally.rejected + tally.not_utf8,
        0,
        "generated programs rejected before the checker: {tally:?} of {names:?}"
    );
}

#[test]
fn checker_verdicts_on_generated_programs() {
    let valid = [
        call_chain(5_000),
        let_chain(20_000),
        wide_call(2_000),
        nested_ifs(100),
        nested_parens(200),
        interpolation_chain(2_000),
        extremes(),
    ];
    let invalid = [
        unknown_names(20_000),
        duplicate_fns(5_000),
        mixed_chain(1_999),
    ];
    let outcome = thread::Builder::new()
        .stack_size(PIPELINE_STACK)
        .spawn(move || {
            let file = FileId::from_raw(0);
            let verdict = |src: &str| match front(src, file) {
                Front::Parsed(module) => {
                    let (checked, diags) = check(&module, file, src);
                    let errors = diags.iter().filter(|d| d.is_error()).count();
                    Some((checked.main.is_some(), errors))
                }
                Front::Rejected => None,
            };
            let valid: Vec<_> = valid.iter().map(|s| verdict(s)).collect();
            let invalid: Vec<_> = invalid.iter().map(|s| verdict(s)).collect();
            (valid, invalid)
        })
        .unwrap_or_else(|e| panic!("cannot start the checker thread: {e}"))
        .join();
    let Ok((valid, invalid)) = outcome else {
        panic!("the checker thread ended abnormally");
    };
    for (index, v) in valid.iter().enumerate() {
        assert_eq!(
            *v,
            Some((true, 0)),
            "valid program {index} was not accepted"
        );
    }
    // 20 000 unknown callees: at least one diagnostic each, and the checker does not stop
    // early in a way that hides later faults.
    let unknown = invalid[0].map(|(_, errors)| errors);
    assert!(
        unknown.is_some_and(|e| e >= 20_000),
        "unknown names: {unknown:?}"
    );
    // 5 000 functions named `main`: one E0201 for each one after the first.
    let duplicates = invalid[1].map(|(_, errors)| errors);
    assert_eq!(duplicates, Some(4_999), "duplicate functions");
    let mixed = invalid[2].map(|(_, errors)| errors);
    assert!(mixed.is_some_and(|e| e >= 1), "mixed chain: {mixed:?}");
}
