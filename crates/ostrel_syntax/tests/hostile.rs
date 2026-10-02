//! Hostile input against the lexer and the parser (AC-04, WP T1-6).
//!
//! Every committed hostile case, error golden and v0.1 example is lexed and
//! parsed as is and as seeded mutations. For each input the token stream and
//! every diagnostic must be well formed: no panic, exactly one `EOF` at the
//! end, every span inside the source and on character boundaries, and the
//! same result on a second run. The large and binary cases of
//! `tests/hostile/gen` run through the CLI in the gate; this file covers the
//! crate itself, with a fixed seed so every failure is reproducible.
//!
//! The parse limits are also checked at their exact default values (the
//! hostile corpus keeps margins around them, D54).

use std::fs;
use std::path::{Path, PathBuf};

use ostrel_core::{Diagnostic, FileId};
use ostrel_syntax::ast::Module;
use ostrel_syntax::lex::{Token, TokenKind, lex};
use ostrel_syntax::parse::{self, Exprs, MAX_DEPTH, ParseLimits, codes};
use ostrel_syntax::{Limits, MAX_HEIGHT, MAX_NODES};

type R = Result<(), String>;

const F: FileId = FileId::from_raw(0);

/// Codes of the three limit diagnostics.
const NESTING_TOO_DEEP: u16 = codes::NESTING_TOO_DEEP.number();
const TREE_TOO_HIGH: u16 = codes::TREE_TOO_HIGH.number();
const TOO_MANY_NODES: u16 = codes::TOO_MANY_NODES.number();

/// Mutants per seed file.
const MUTANTS: u64 = 24;
/// Most edits applied to one mutant.
const MAX_EDITS: u64 = 8;

/// Text pieces the mutator inserts: brackets, quotes, escapes, line ends,
/// indentation, invisible and bidi characters, keywords, operators and
/// edge values.
const PIECES: &[&str] = &[
    "(",
    ")",
    "{",
    "}",
    "{{",
    "\\}",
    "\"",
    "\\",
    "\\u{",
    "\\u{10FFFF}",
    "\\u{110000}",
    "\n",
    "\r",
    "\r\n",
    "\t",
    "  ",
    "    ",
    "\u{202E}",
    "\u{2066}",
    "\u{FEFF}",
    "\u{200B}",
    "\0",
    "fn ",
    "fn main()\n",
    "let ",
    "if ",
    "else\n",
    "return ",
    "data ",
    "not ",
    " and ",
    " or ",
    "+",
    "-",
    "*",
    "/",
    "%",
    "<",
    "==",
    "+=",
    ":=",
    "->",
    ":",
    ",",
    "=",
    ";",
    "//",
    "9007199254740991",
    "9007199254740992",
    "-9007199254740992",
    "0",
    "x",
    "print",
    "true",
    "é",
    "\u{10FFFF}",
];

/// The source files of the corpus, the error goldens and the v0.1 examples.
fn seeds() -> Result<Vec<(String, String)>, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut seeds = Vec::new();
    for dir in ["tests/hostile/cases", "tests/errors", "examples/v0_1"] {
        let mut paths: Vec<PathBuf> = fs::read_dir(root.join(dir))
            .map_err(|e| format!("{dir}: {e}"))?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "ostl"))
            .collect();
        paths.sort();
        if paths.is_empty() {
            return Err(format!("{dir}: no .ostl files"));
        }
        for path in paths {
            let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            // Invalid UTF-8 never reaches the lexer (D84, E0014 in the CLI).
            if let Ok(text) = String::from_utf8(bytes) {
                seeds.push((path.display().to_string(), text));
            }
        }
    }
    Ok(seeds)
}

/// Deterministic xorshift generator, so a failure names its seed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A value in `0..n`, 0 for `n == 0`.
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }

    /// A character boundary of `s`.
    fn boundary(&mut self, s: &str) -> usize {
        let len = u64::try_from(s.len()).unwrap_or(u64::MAX);
        let mut at = usize::try_from(self.below(len.saturating_add(1))).unwrap_or(0);
        while !s.is_char_boundary(at) {
            at -= 1;
        }
        at
    }
}

/// Applies one random edit to `s`: insert a piece, delete or duplicate a
/// range, or repeat a piece many times.
fn mutate(s: &mut String, rng: &mut Rng) {
    let a = rng.boundary(s);
    let b = rng.boundary(s);
    let (lo, hi) = (a.min(b), a.max(b));
    let piece = PIECES
        .get(usize::try_from(rng.below(PIECES.len() as u64)).unwrap_or(0))
        .copied()
        .unwrap_or("(");
    match rng.below(5) {
        0 | 1 => s.insert_str(a, piece),
        2 => s.replace_range(lo..hi, ""),
        3 => {
            let copy = s.get(lo..hi).unwrap_or("").to_owned();
            s.insert_str(hi, &copy);
        }
        _ => {
            let times = usize::try_from(rng.below(400)).unwrap_or(0);
            s.insert_str(a, &piece.repeat(times));
        }
    }
}

/// Lexes and parses `src` with `limits`.
fn run(src: &str, limits: ParseLimits) -> (Vec<Token>, Vec<Diagnostic>, Module, Vec<Diagnostic>) {
    let (tokens, lexed) = lex(src, F);
    let (module, parsed) = parse::parse_with(src, F, &tokens, &lexed, limits, &Exprs);
    (tokens, lexed, module, parsed)
}

/// Lexes and parses `src` through the frozen entry point (D85).
fn parse_default(src: &str) -> (Module, Vec<Diagnostic>) {
    let (tokens, lexed) = lex(src, F);
    let (module, mut diags) = parse::parse(src, F, &tokens, &lexed);
    let mut all = lexed;
    all.append(&mut diags);
    (module, all)
}

fn codes_of(diags: &[Diagnostic]) -> Vec<u16> {
    diags.iter().map(|d| d.code.number()).collect()
}

/// Checks the invariants of one input; `name` identifies it in a failure.
fn check(name: &str, src: &str) -> R {
    let (tokens, lexed, module, parsed) = run(src, ParseLimits::DEFAULT);
    let len = u32::try_from(src.len()).map_err(|e| e.to_string())?;
    let on_boundary = |at: u32| src.is_char_boundary(at as usize);

    let eofs = tokens.iter().filter(|t| t.kind == TokenKind::Eof).count();
    if eofs != 1 || tokens.last().map(|t| t.kind) != Some(TokenKind::Eof) {
        return Err(format!(
            "{name}: token stream must end with exactly one EOF"
        ));
    }
    let mut prev = 0;
    for tok in &tokens {
        let s = tok.span;
        if s.start > s.end || s.end > len || !on_boundary(s.start) || !on_boundary(s.end) {
            return Err(format!("{name}: bad token span {tok:?}"));
        }
        if s.start < prev {
            return Err(format!("{name}: tokens out of order at {tok:?}"));
        }
        prev = s.start;
    }
    for diag in lexed.iter().chain(&parsed) {
        let s = diag.span;
        if s.start > s.end || s.end > len || !on_boundary(s.start) || !on_boundary(s.end) {
            return Err(format!("{name}: bad diagnostic span {diag:?}"));
        }
        if diag.message.trim().is_empty() {
            return Err(format!("{name}: empty message for {}", diag.code));
        }
    }
    let limits = Limits::DEFAULT;
    if module.node_count() > limits.max_nodes || module.height() > limits.max_height {
        return Err(format!("{name}: tree over its limits"));
    }

    let (tokens2, lexed2, module2, parsed2) = run(src, ParseLimits::DEFAULT);
    if tokens2 != tokens
        || lexed2 != lexed
        || parsed2 != parsed
        || module2.node_count() != module.node_count()
    {
        return Err(format!("{name}: second run differs"));
    }
    Ok(())
}

#[test]
fn ac_04_committed_cases_are_well_formed() -> R {
    let seeds = seeds()?;
    for (name, src) in &seeds {
        check(name, src)?;
    }
    Ok(())
}

#[test]
fn ac_04_seeded_mutants_are_well_formed() -> R {
    let seeds = seeds()?;
    let mut count = 0u64;
    for (index, (name, src)) in seeds.iter().enumerate() {
        for mutant in 0..MUTANTS {
            let seed = (index as u64) << 32 | mutant;
            let mut rng = Rng::new(seed);
            let mut text = src.clone();
            for _ in 0..=rng.below(MAX_EDITS) {
                mutate(&mut text, &mut rng);
            }
            check(&format!("{name} mutant seed {seed:#x}"), &text)?;
            count += 1;
        }
    }
    if count < 2_000 {
        return Err(format!("only {count} mutants, the seed set shrank"));
    }
    Ok(())
}

#[test]
fn ac_04_every_prefix_of_an_example_is_well_formed() -> R {
    // Cutting a file anywhere gives unterminated strings, interpolations,
    // brackets and blocks at end of file.
    let src = "fn twice(n: Int) -> Int\n  return n * 2 // twice\n\nfn main()\n  \
               let s = \"a{twice(1)}\\u{1F600}é\"\n  if s == \"x\"\n    print((1 + 2) * 3)\n  \
               else\n    print(not true and false)\n";
    let mut cut = 0;
    while cut <= src.len() {
        if let Some(prefix) = src.get(..cut) {
            check(&format!("prefix {cut}"), prefix)?;
        }
        cut += 1;
    }
    Ok(())
}

/// `fn main()` with `let x = 1 + 1 + ...` of `operands` operands.
fn add_chain(operands: usize) -> String {
    format!("fn main()\n  let x = {}\n", vec!["1"; operands].join(" + "))
}

#[test]
fn ac_04_height_limit_is_exact_at_its_default() -> R {
    // The largest chain that parses has exactly the default height; one
    // more operand is one diagnostic (D54).
    let (mut ok, mut bad) = (1usize, 4 * MAX_HEIGHT as usize);
    if !parse_default(&add_chain(bad))
        .1
        .iter()
        .any(|d| d.code.number() == TREE_TOO_HIGH)
    {
        return Err("upper bound of the search parses".into());
    }
    while bad - ok > 1 {
        let mid = ok + (bad - ok) / 2;
        if parse_default(&add_chain(mid)).1.is_empty() {
            ok = mid;
        } else {
            bad = mid;
        }
    }
    let (module, diags) = parse_default(&add_chain(ok));
    if !diags.is_empty() || module.height() != MAX_HEIGHT {
        return Err(format!(
            "{ok} operands: height {}, {diags:?}",
            module.height()
        ));
    }
    let (_, diags) = parse_default(&add_chain(ok + 1));
    if codes_of(&diags) != [TREE_TOO_HIGH] {
        return Err(format!("{} operands: {diags:?}", ok + 1));
    }
    Ok(())
}

/// Functions `fn fK()` with one `print` of a chain of 100 operands, then one
/// function whose `let` value is `nots` times `not` before `true`.
fn wide(functions: usize, nots: usize) -> String {
    let body = vec!["1"; 100].join(" + ");
    let mut src = String::new();
    for k in 0..functions {
        src.push_str(&format!("fn f{k}()\n  print({body})\n"));
    }
    src.push_str(&format!(
        "fn main()\n  let x = {}true\n",
        "not ".repeat(nots)
    ));
    src
}

#[test]
fn ac_04_node_limit_is_exact_at_its_default() -> R {
    // Each `not` adds one node, so the tail fills the last few nodes exactly.
    let lifted = ParseLimits {
        max_depth: MAX_DEPTH,
        tree: Limits {
            max_nodes: u32::MAX,
            max_height: MAX_HEIGHT,
        },
    };
    let nodes = |src: &str| -> Result<u32, String> {
        let (_, lexed, module, parsed) = run(src, lifted);
        if lexed.is_empty() && parsed.is_empty() {
            Ok(module.node_count())
        } else {
            Err(format!("{lexed:?} {parsed:?}"))
        }
    };
    let base = nodes(&wide(0, 0))?;
    let per_fn = nodes(&wide(1, 0))? - base;
    if nodes(&wide(0, 1))? != base + 1 {
        return Err("a `not` must add exactly one node".into());
    }
    let functions = ((MAX_NODES - base) / per_fn) as usize;
    let nots = (MAX_NODES - nodes(&wide(functions, 0))?) as usize;
    if nots >= MAX_HEIGHT as usize / 2 {
        return Err(format!("tail of {nots} nodes is too high"));
    }

    let (module, diags) = parse_default(&wide(functions, nots));
    if !diags.is_empty() || module.node_count() != MAX_NODES {
        return Err(format!("{} nodes, {diags:?}", module.node_count()));
    }
    let (_, diags) = parse_default(&wide(functions, nots + 1));
    if codes_of(&diags) != [TOO_MANY_NODES] {
        return Err(format!("one node over: {diags:?}"));
    }
    Ok(())
}

#[test]
fn ac_04_nesting_limit_is_exact_at_its_default() -> R {
    // The function body is level 1; brackets and calls fill the rest.
    let open = MAX_DEPTH as usize - 1;
    for (head, name) in [("(", "parentheses"), ("f(", "calls")] {
        let ok = format!(
            "fn main()\n  let x = {}1{}\n",
            head.repeat(open),
            ")".repeat(open)
        );
        let (_, diags) = parse_default(&ok);
        if !diags.is_empty() {
            return Err(format!("{name} at the limit: {diags:?}"));
        }
        let over = format!(
            "fn main()\n  let x = {}1{}\n",
            head.repeat(open + 1),
            ")".repeat(open + 1)
        );
        let (_, diags) = parse_default(&over);
        if codes_of(&diags) != [NESTING_TOO_DEEP] {
            return Err(format!("{name} over the limit: {diags:?}"));
        }
    }
    Ok(())
}

#[test]
fn ac_04_comment_lines_cost_no_extra_diagnostics() -> R {
    // A chain over the height limit with comments around it still gives one
    // diagnostic: the token view skips trivia without copying the stream.
    let src = format!("// head\n{}// tail\n", add_chain(3 * MAX_HEIGHT as usize));
    let (_, diags) = parse_default(&src);
    if codes_of(&diags) != [TREE_TOO_HIGH] {
        return Err(format!("{diags:?}"));
    }
    check("commented chain", &src)
}
