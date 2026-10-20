//! Hostile input hardening of the lowering under `ostrel run` (gap G-1, SPEC 9, D41, AC-04).
//!
//! `ostrel run` lowers every program the checker accepts. A [`LowerError`] there is the
//! internal error exit 70, which no corpus case may produce (SPEC 12.6, D84). So for every
//! input that passes lexer, parser and checker, as in the CLI pipeline, this file asserts:
//! * `ostrel_ir::lower` returns a program instead of an error or a panic,
//! * the program is well formed by the rules the VM relies on (every index in range,
//!   operand types as documented on `InstKind`, call arity and types, a `Bool` condition
//!   on every branch, return values matching the result type, `Int` constants within the
//!   bounds of D24, spans inside the source), checked here independently of `ostrel_vm`,
//! * a second lowering gives the identical program.
//!
//! Inputs: the committed hostile corpus (`tests/hostile/cases/`), seeded mutations of it
//! and of the v0.1 examples, and generated programs that are large or deep. Lowering runs
//! on a thread with the stack of the CLI pipeline thread (64 MiB, INT-4).

use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::thread;

use ostrel_core::{Diagnostic, FileId};
use ostrel_ir::{
    BinOp, Block, BlockId, Const, Function, INT_MAX, INT_MIN, InstKind, Local, LowerError, Program,
    Span, Terminator, Ty, UnOp, lower,
};
use ostrel_sema::{Checked, check};
use ostrel_syntax::ast::Module;

/// Stack of the CLI pipeline thread (`crates/ostrel_cli/src/main.rs`).
const PIPELINE_STACK: usize = 64 * 1024 * 1024;

/// Mutations per seed program in [`mutated_programs_lower_to_valid_ir`].
const MUTATIONS_PER_SEED: u64 = 24;

/// Base seed of the mutations. Changing it changes the inputs, never the expectations.
const SEED: u64 = 0x6f73_7472_656c_1044;

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

/// The tree of `src` if lexer and parser report no error; the checker and the lowering
/// run only then (SPEC 12.1).
fn parsed(src: &str, file: FileId) -> Option<Module> {
    let (tokens, mut diags) = ostrel_syntax::lex::lex(src, file);
    let (module, more) = ostrel_syntax::parse::parse(src, file, &tokens, &diags);
    diags.extend(more);
    if diags.iter().any(Diagnostic::is_error) {
        None
    } else {
        Some(module)
    }
}

/// How far the pipeline got with one input.
#[derive(Debug, Default)]
struct Tally {
    before_checker: usize,
    checker_errors: usize,
    lowered: usize,
}

/// Runs one input through the pipeline up to the lowering and returns the first broken
/// rule, if any.
fn probe(name: &str, bytes: &[u8], tally: &mut Tally) -> Option<String> {
    let file = FileId::from_raw(0);
    let Some((src, module)) = std::str::from_utf8(bytes)
        .ok()
        .and_then(|src| parsed(src, file).map(|m| (src, m)))
    else {
        tally.before_checker += 1;
        return None;
    };
    let checker = panic::catch_unwind(AssertUnwindSafe(|| check(&module, file, src)));
    let Ok((checked, diags)) = checker else {
        return Some(format!("{name}: the checker panicked"));
    };
    if diags.iter().any(Diagnostic::is_error) {
        tally.checker_errors += 1;
        return None;
    }
    let lowered = || panic::catch_unwind(AssertUnwindSafe(|| lower(&module, &checked, file)));
    let program = match lowered() {
        Err(_) => return Some(format!("{name}: the lowering panicked")),
        Ok(Err(e)) => {
            return Some(format!(
                "{name}: accepted by the checker, lowering failed with {e:?} (exit 70)"
            ));
        }
        Ok(Ok(program)) => program,
    };
    if let Err(fault) = validate(&program, &module, src) {
        return Some(format!("{name}: malformed IR: {fault}"));
    }
    match lowered() {
        Ok(Ok(again)) if again == program => {}
        _ => return Some(format!("{name}: two lowerings disagree")),
    }
    tally.lowered += 1;
    None
}

/// Runs `cases` on a thread with the stack of the CLI pipeline and fails with every broken
/// rule at once.
fn probe_all(cases: Vec<(String, Vec<u8>)>) -> Tally {
    let outcome = thread::Builder::new()
        .name("hostile-ir".to_string())
        .stack_size(PIPELINE_STACK)
        .spawn(move || {
            let mut tally = Tally::default();
            let faults: Vec<String> = cases
                .iter()
                .filter_map(|(name, bytes)| probe(name, bytes, &mut tally))
                .collect();
            (tally, faults)
        })
        .unwrap_or_else(|e| panic!("cannot start the lowering thread: {e}"))
        .join();
    let Ok((tally, faults)) = outcome else {
        panic!("the lowering thread ended abnormally");
    };
    assert!(faults.is_empty(), "lowering faults:\n{}", faults.join("\n"));
    tally
}

/// Checks the rules the VM relies on, without using the VM.
fn validate(program: &Program, module: &Module, src: &str) -> Result<(), String> {
    if program.functions.len() != module.items().len() {
        return Err(format!(
            "{} functions for {} items",
            program.functions.len(),
            module.items().len()
        ));
    }
    let main = program
        .function(program.main)
        .ok_or_else(|| format!("main {:?} out of range", program.main))?;
    if main.params != 0 || main.result != Ty::Unit {
        return Err(format!(
            "main has {} params and result {:?}",
            main.params, main.result
        ));
    }
    for (index, f) in program.functions.iter().enumerate() {
        validate_fn(program, f, src).map_err(|e| format!("function {index} `{}`: {e}", f.name))?;
    }
    Ok(())
}

fn span_ok(span: Span, src: &str) -> Result<(), String> {
    let (start, end) = (span.start as usize, span.end as usize);
    if span.file != 0 || start > end || end > src.len() {
        return Err(format!(
            "span {span:?} outside the source of {} bytes",
            src.len()
        ));
    }
    Ok(())
}

fn validate_fn(program: &Program, f: &Function, src: &str) -> Result<(), String> {
    span_ok(f.span, src)?;
    if f.params as usize > f.locals.len() {
        return Err(format!("{} params but {} locals", f.params, f.locals.len()));
    }
    if f.locals.contains(&Ty::Unit) {
        return Err("a local of type Unit".to_string());
    }
    if f.blocks.is_empty() {
        return Err("no entry block".to_string());
    }
    let ty = |l: Local| {
        f.local_ty(l)
            .ok_or_else(|| format!("local {l:?} out of range"))
    };
    let expect = |l: Local, want: Ty| -> Result<(), String> {
        let got = ty(l)?;
        if got == want {
            Ok(())
        } else {
            Err(format!("local {l:?} is {got:?}, expected {want:?}"))
        }
    };
    for (b, Block { insts, term }) in f.blocks.iter().enumerate() {
        for inst in insts {
            span_ok(inst.span, src)?;
            let at = |e: String| format!("block {b}, {:?}: {e}", inst.kind);
            match &inst.kind {
                InstKind::Const { dst, value } => {
                    expect(*dst, value.ty()).map_err(at)?;
                    if let Const::Int(v) = value
                        && !(INT_MIN..=INT_MAX).contains(v)
                    {
                        return Err(at(format!("constant {v} outside the Int range")));
                    }
                }
                InstKind::Copy { dst, src } => {
                    let t = ty(*src).map_err(at)?;
                    expect(*dst, t).map_err(at)?;
                }
                InstKind::Unary { dst, op, arg } => {
                    let t = match op {
                        UnOp::Neg => Ty::Int,
                        UnOp::Not => Ty::Bool,
                    };
                    expect(*arg, t).map_err(at)?;
                    expect(*dst, t).map_err(at)?;
                }
                InstKind::Binary { dst, op, lhs, rhs } => {
                    expect(*dst, op.result_ty()).map_err(at)?;
                    if matches!(op, BinOp::Eq | BinOp::Ne) {
                        let t = ty(*lhs).map_err(at)?;
                        expect(*rhs, t).map_err(at)?;
                    } else {
                        expect(*lhs, Ty::Int).map_err(at)?;
                        expect(*rhs, Ty::Int).map_err(at)?;
                    }
                }
                InstKind::ToText { dst, src } => {
                    expect(*dst, Ty::Text).map_err(at)?;
                    ty(*src).map_err(at)?;
                }
                InstKind::Concat { dst, parts } => {
                    expect(*dst, Ty::Text).map_err(at)?;
                    for p in parts {
                        expect(*p, Ty::Text).map_err(at)?;
                    }
                }
                InstKind::Call { dst, func, args } => {
                    let callee = program
                        .function(*func)
                        .ok_or_else(|| at(format!("callee {func:?} out of range")))?;
                    if args.len() != callee.params as usize {
                        return Err(at(format!(
                            "{} arguments for {} parameters",
                            args.len(),
                            callee.params
                        )));
                    }
                    for (i, a) in (0u32..).zip(args) {
                        let want = callee
                            .local_ty(Local(i))
                            .ok_or_else(|| at(format!("callee parameter {i} has no local")))?;
                        expect(*a, want).map_err(at)?;
                    }
                    match (dst, callee.result) {
                        (None, Ty::Unit) => {}
                        (Some(d), r) if r != Ty::Unit => expect(*d, r).map_err(at)?,
                        (d, r) => {
                            return Err(at(format!("result {d:?} for callee result {r:?}")));
                        }
                    }
                }
                InstKind::Print { arg } => {
                    ty(*arg).map_err(at)?;
                }
            }
        }
        let target = |id: BlockId| {
            f.block(id)
                .map(|_| ())
                .ok_or_else(|| format!("block {b}: target {id:?} out of range"))
        };
        match term {
            Terminator::Jump(t) => target(*t)?,
            Terminator::Branch {
                cond,
                then,
                otherwise,
            } => {
                expect(*cond, Ty::Bool).map_err(|e| format!("block {b}: {e}"))?;
                target(*then)?;
                target(*otherwise)?;
            }
            Terminator::Return(value) => match (value, f.result) {
                (None, Ty::Unit) => {}
                (Some(v), r) if r != Ty::Unit => {
                    expect(*v, r).map_err(|e| format!("block {b}: {e}"))?;
                }
                (v, r) => return Err(format!("block {b}: return {v:?} with result {r:?}")),
            },
        }
    }
    Ok(())
}

#[test]
fn committed_corpus_lowers_to_valid_ir() {
    let cases = corpus();
    // The `run_` cases of the corpus are valid programs (AC-04 run half, A2-16); they
    // must all reach the lowering, or this test has stopped testing it.
    let runs = cases.iter().filter(|(n, _)| n.starts_with("run_")).count();
    assert!(runs >= 10, "only {runs} run cases in the corpus");
    let tally = probe_all(cases);
    assert!(
        tally.lowered >= runs,
        "only {} corpus cases were lowered, the corpus has {runs} run cases: {tally:?}",
        tally.lowered
    );
}

#[test]
fn mutated_programs_lower_to_valid_ir() {
    let tally = probe_all(mutations());
    assert!(
        tally.lowered >= 50,
        "too few mutations reach the lowering: {tally:?}"
    );
}

/// `and` and `or` alternating over `n` operands: every operator becomes a branch.
fn short_circuit_chain(n: usize) -> String {
    let mut joined = String::new();
    for i in 0..n {
        if i > 0 {
            joined.push_str(if i % 2 == 0 { " and " } else { " or " });
        }
        joined.push_str(&format!("a > {i}"));
    }
    format!("fn test(a: Int) -> Bool\n  {joined}\n\nfn main()\n  print(test(3))\n")
}

/// One string with `n` interpolations: one `Concat` with many parts.
fn interpolation_parts(n: usize) -> String {
    let parts: String = (0..n).map(|i| format!("{{x}}-{i}|")).collect();
    format!("fn main()\n  let x = 7\n  print(\"{parts}\")\n")
}

#[test]
fn large_and_deep_programs_lower_to_valid_ir() {
    let cases: Vec<(String, String)> = vec![
        ("call_chain_5000".into(), call_chain(5_000)),
        ("let_chain_20000".into(), let_chain(20_000)),
        ("rebinding_20000".into(), rebinding(20_000)),
        ("wide_call_2000".into(), wide_call(2_000)),
        ("nested_ifs_100".into(), nested_ifs(100)),
        ("nested_parens_200".into(), nested_parens(200)),
        (
            "interpolation_chain_2000".into(),
            interpolation_chain(2_000),
        ),
        ("extremes".into(), extremes()),
        (
            "short_circuit_chain_1999".into(),
            short_circuit_chain(1_999),
        ),
        (
            "interpolation_parts_5000".into(),
            interpolation_parts(5_000),
        ),
    ];
    let count = cases.len();
    let tally = probe_all(
        cases
            .into_iter()
            .map(|(n, s)| (n, s.into_bytes()))
            .collect(),
    );
    assert_eq!(
        tally.lowered, count,
        "every generated program must be lowered: {tally:?}"
    );
}

#[test]
fn lowering_with_tables_of_another_tree_fails_cleanly() {
    // A defect upstream (tables of one tree, lowering of another) must surface as a
    // `LowerError`, which the CLI turns into exit 70, or as well formed IR; never as a
    // panic or as IR that indexes outside its own tables.
    let small = "fn main()\n  print(1)\n".to_string();
    let big = call_chain(50);
    let outcome = thread::Builder::new()
        .stack_size(PIPELINE_STACK)
        .spawn(move || -> Result<(), String> {
            let file = FileId::from_raw(0);
            let tree_a = parsed(&small, file).ok_or("the small sample does not parse")?;
            let tree_b = parsed(&big, file).ok_or("the large sample does not parse")?;
            let checked_a = check(&tree_a, file, &small).0;
            let checked_b = check(&tree_b, file, &big).0;
            for (tree, tables, src) in [
                (&tree_a, &checked_b, small.as_str()),
                (&tree_b, &checked_a, big.as_str()),
            ] {
                match panic::catch_unwind(AssertUnwindSafe(|| lower(tree, tables, file))) {
                    Err(_) => return Err("the lowering panicked".to_string()),
                    Ok(Ok(p)) => validate(&p, tree, src)
                        .map_err(|e| format!("mismatched tables gave malformed IR: {e}"))?,
                    Ok(Err(LowerError::NoMain | LowerError::Unchecked(_))) => {}
                }
            }
            let empty = Checked::default();
            match panic::catch_unwind(AssertUnwindSafe(|| lower(&tree_a, &empty, file))) {
                Ok(Err(LowerError::NoMain)) => Ok(()),
                Ok(other) => Err(format!("empty tables gave {other:?}")),
                Err(_) => Err("the lowering panicked on empty tables".to_string()),
            }
        })
        .unwrap_or_else(|e| panic!("cannot start the lowering thread: {e}"))
        .join();
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("{e}"),
        Err(_) => panic!("the lowering thread ended abnormally"),
    }
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
