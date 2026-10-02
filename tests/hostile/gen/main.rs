//! Seeded hostile input generator for the Ostrel compiler (AC-04, QA-1).
//!
//! Std only, no dependencies. It is built with plain `rustc` by `tests/hostile/run.sh`
//! so it does not need a Cargo workspace entry.
//!
//! Usage: `hostile-gen <out-dir> [--count N] [--seed S]`
//!
//! Writes two kinds of files into `<out-dir>`:
//! * named cases: the large and binary inputs AC-04 requires (1 MiB random bytes,
//!   10 MiB single line, nesting depth 100 000, chains of 5 000 000 operators or calls,
//!   invalid UTF-8, NUL bytes) that cannot or
//!   should not be committed;
//! * seeded cases: `N` small random inputs (random bytes, token soup, mutations of valid
//!   programs), reproducible from the base seed.
//!
//! The same arguments always produce byte identical files. A `MANIFEST` file lists every
//! case with its size and FNV-1a 64 hash, so two runs can be compared.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const MIB: usize = 1024 * 1024;
const NEST: usize = 100_000;
/// Operator, `and`, `or` and call chain length (REVIEW_RED_A3 #1).
const CHAIN: usize = 5_000_000;
const DEFAULT_COUNT: u64 = 200;
const DEFAULT_SEED: u64 = 0x05_7e_e1_00_00_00_00_04;

/// SplitMix64: small, fast, deterministic on every platform.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform value in `0..n`; `n` must be greater than zero.
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        let i = self.below(items.len() as u64) as usize;
        items[i]
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len + 8);
        while out.len() < len {
            out.extend_from_slice(&self.next().to_le_bytes());
        }
        out.truncate(len);
        out
    }
}

fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

struct Out {
    dir: PathBuf,
    manifest: Vec<String>,
}

impl Out {
    fn write(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        fs::write(self.dir.join(name), data)?;
        self.manifest
            .push(format!("{name} {} {:016x}", data.len(), fnv1a(data)));
        Ok(())
    }
}

/// Wraps an expression in a valid `main`, so the case reaches the parser body.
fn in_main(expr: &str) -> Vec<u8> {
    format!("fn main()\n  print({expr})\n").into_bytes()
}

fn repeat(s: &str, n: usize) -> String {
    s.repeat(n)
}

fn named_cases(out: &mut Out, seed: u64) -> io::Result<()> {
    let mut rng = Rng::new(seed ^ 0x6e61_6d65);

    // AC-04: 1 MiB random bytes.
    out.write("random_1mib.ostl", &rng.bytes(MIB))?;

    // AC-04: a 10 MiB single line, in three shapes: comment, bare identifier, string literal.
    let mut comment = b"// ".to_vec();
    comment.resize(10 * MIB, b'a');
    out.write("long_line_10mib_comment.ostl", &comment)?;
    out.write("long_line_10mib_ident.ostl", &vec![b'a'; 10 * MIB])?;
    let mut text = b"fn main()\n  print(\"".to_vec();
    let start = text.len();
    text.resize(start + 10 * MIB, b'x');
    text.extend_from_slice(b"\")\n");
    out.write("long_line_10mib_string.ostl", &text)?;
    // A 10 MiB line of one expression: `1+1+1+...` (D43, iterative operator chains).
    let mut chain = b"fn main()\n  print(1".to_vec();
    while chain.len() < 10 * MIB {
        chain.extend_from_slice(b"+1");
    }
    chain.extend_from_slice(b")\n");
    out.write("long_line_10mib_add_chain.ostl", &chain)?;

    // AC-04: nesting depth 100 000. SPEC 5.0: must end with a diagnostic (depth limit).
    let p = format!("{}1{}", repeat("(", NEST), repeat(")", NEST));
    out.write("nest_paren_100k.ostl", &in_main(&p))?;
    let p = format!("{}1", repeat("(", NEST));
    out.write("nest_paren_100k_unclosed.ostl", &in_main(&p))?;
    let p = format!("{}1{}", repeat("{", NEST), repeat("}", NEST));
    out.write("nest_brace_100k.ostl", &in_main(&p))?;
    let p = format!("{}1{}", repeat("[", NEST), repeat("]", NEST));
    out.write("nest_bracket_100k.ostl", &in_main(&p))?;
    let p = format!("{}1{}", repeat("f(", NEST), repeat(")", NEST));
    out.write("nest_call_100k.ostl", &in_main(&p))?;
    out.write(
        "nest_unary_minus_100k.ostl",
        &in_main(&format!("{}1", repeat("-", NEST))),
    )?;
    out.write(
        "nest_unary_minus_spaced_100k.ostl",
        &in_main(&format!("{}1", repeat("- ", NEST))),
    )?;
    out.write(
        "nest_not_100k.ostl",
        &in_main(&format!("{}true", repeat("not ", NEST))),
    )?;
    // G1: braces inside an interpolation are counted up to 32; 100 000 must diagnose.
    let p = format!("\"{}x{}\"", repeat("{", NEST), repeat("}", NEST));
    out.write("nest_interp_brace_100k.ostl", &in_main(&p))?;
    let p = format!("\"{}", repeat("{(", NEST));
    out.write("nest_interp_open_100k.ostl", &in_main(&p))?;
    // Deep block indentation: 2 000 levels of `if true` (about 4 MB), far over depth 256.
    let mut deep = String::from("fn main()\n");
    for level in 1..=2000 {
        deep.push_str(&repeat("  ", level));
        deep.push_str("if true\n");
    }
    deep.push_str(&repeat("  ", 2001));
    deep.push_str("print(1)\n");
    out.write("nest_indent_2000.ostl", deep.as_bytes())?;
    // Long flat chains, no nesting (D43): must not overflow the native stack either.
    let p = format!("1{}", repeat(" + 1", NEST));
    out.write("chain_add_100k.ostl", &in_main(&p))?;
    let p = format!("true{}", repeat(" and true", NEST));
    out.write("chain_and_100k.ostl", &in_main(&p))?;
    let p = format!("1{}", repeat(" == 1", NEST));
    out.write("chain_compare_100k.ostl", &in_main(&p))?;
    // RED-A3 #1: chains in the millions for `check` and `run`. Each is one flat line; a
    // recursive pass or a recursive `Drop` over such an AST overflows the native stack.
    let mut and_chain = b"fn main()\n  print(true".to_vec();
    for _ in 0..CHAIN {
        and_chain.extend_from_slice(b" and true");
    }
    and_chain.extend_from_slice(b")\n");
    out.write("chain_and_5m.ostl", &and_chain)?;
    let mut or_chain = b"fn main()\n  print(false".to_vec();
    for _ in 0..CHAIN {
        or_chain.extend_from_slice(b" or false");
    }
    or_chain.extend_from_slice(b")\n");
    out.write("chain_or_5m.ostl", &or_chain)?;
    // Postfix call chain `f(1)(1)(1)...`, not nesting: the postfix loop builds one level
    // per call without recursing in the parser.
    let mut calls = b"fn f(n: Int) -> Int\n  return n\n\nfn main()\n  print(f(1)".to_vec();
    for _ in 0..CHAIN {
        calls.extend_from_slice(b"(1)");
    }
    calls.extend_from_slice(b")\n");
    out.write("chain_call_5m.ostl", &calls)?;
    let mut lets = String::from("fn main()\n  let x0 = 0\n");
    for i in 1..NEST {
        lets.push_str(&format!("  let x{i} = x{} + 1\n", i - 1));
    }
    lets.push_str(&format!("  print(x{})\n", NEST - 1));
    out.write("many_lets_100k.ostl", lets.as_bytes())?;
    let mut fns = String::new();
    for i in 0..NEST {
        fns.push_str(&format!("fn f{i}() -> Int\n  return {i}\n"));
    }
    fns.push_str("fn main()\n  print(f99999())\n");
    out.write("many_fns_100k.ostl", fns.as_bytes())?;
    let mut ident = b"fn main()\n  let ".to_vec();
    ident.resize(ident.len() + MIB, b'a');
    ident.extend_from_slice(b" = 1\n");
    out.write("long_ident_1mib.ostl", &ident)?;
    let digits = format!("fn main()\n  print({})\n", repeat("9", MIB));
    out.write("long_int_literal_1mib.ostl", digits.as_bytes())?;

    // AC-04: invalid UTF-8 and NUL bytes, plus related encodings.
    out.write("invalid_utf8.ostl", b"fn main()\n  print(\"\xff\xfe\")\n")?;
    out.write(
        "invalid_utf8_ident.ostl",
        b"fn ma\xc3\x28in()\n  print(1)\n",
    )?;
    out.write(
        "invalid_utf8_overlong.ostl",
        b"fn main()\n  print(\"\xc0\xaf\")\n",
    )?;
    out.write(
        "invalid_utf8_surrogate.ostl",
        b"fn main()\n  print(\"\xed\xa0\x80\")\n",
    )?;
    out.write(
        "invalid_utf8_truncated_eof.ostl",
        b"fn main()\n  print(\"\xe2\x82",
    )?;
    out.write(
        "invalid_utf8_in_comment.ostl",
        b"// \xff\nfn main()\n  print(1)\n",
    )?;
    out.write("nul_bytes.ostl", b"fn main()\n  pr\0int(1)\n\0\0\0")?;
    out.write("nul_in_string.ostl", b"fn main()\n  print(\"a\0b\")\n")?;
    out.write("nul_in_comment.ostl", b"// a\0b\nfn main()\n  print(1)\n")?;
    out.write("nul_only_64kib.ostl", &vec![0u8; 64 * 1024])?;
    out.write("bom_start.ostl", b"\xef\xbb\xbffn main()\n  print(1)\n")?;
    out.write("bom_middle.ostl", b"fn main()\n  print(\xef\xbb\xbf1)\n")?;
    out.write("crlf_lines.ostl", b"fn main()\r\n  print(1)\r\n")?;
    out.write("cr_only_lines.ostl", b"fn main()\r  print(1)\r")?;
    out.write("form_feed_indent.ostl", b"fn main()\n\x0c print(1)\n")?;
    out.write("vertical_tab.ostl", b"fn main()\n  print(\x0b1)\n")?;
    // Unicode that is valid UTF-8 but often mishandled.
    out.write(
        "bidi_override_in_string.ostl",
        "fn main()\n  print(\"a\u{202e}b\u{2066}c\")\n".as_bytes(),
    )?;
    out.write(
        "bidi_override_in_comment.ostl",
        "// \u{202e} } \u{2066}\nfn main()\n  print(1)\n".as_bytes(),
    )?;
    out.write(
        "zero_width_in_ident.ostl",
        "fn ma\u{200d}in()\n  print(1)\n".as_bytes(),
    )?;
    out.write(
        "non_ascii_ident.ostl",
        "fn main()\n  let \u{e4}x = 1\n  print(\u{e4}x)\n".as_bytes(),
    )?;
    out.write(
        "nbsp_indent.ostl",
        "fn main()\n\u{a0}\u{a0}print(1)\n".as_bytes(),
    )?;
    out.write(
        "line_separator.ostl",
        "fn main()\u{2028}  print(1)\u{2029}".as_bytes(),
    )?;
    Ok(())
}

/// v0.1 tokens (SYNTAX 4.11) plus a few from the full grammar the parser may meet.
const TOKENS: &[&str] = &[
    "fn",
    "let",
    "if",
    "else",
    "return",
    "and",
    "or",
    "not",
    "true",
    "false",
    "main",
    "print",
    "x",
    "y",
    "n",
    "Int",
    "Text",
    "Bool",
    "0",
    "1",
    "-1",
    "9007199254740991",
    "9007199254740992",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    ",",
    ":",
    "->",
    "=",
    "==",
    "!=",
    "<",
    "<=",
    ">",
    ">=",
    "+",
    "-",
    "*",
    "/",
    "%",
    ".",
    "..",
    "??",
    "~",
    "\"",
    "\"a\"",
    "\"{x}\"",
    "\"\\{\"",
    "\\",
    "//",
    "/*",
    "*/",
    "@",
    "#",
    "$",
    "`",
    "'",
    ";",
    "make",
    "data",
    "view",
    "app",
    "catch",
    "in",
    "for",
    "as",
    "system",
];

const SPACING: &[&str] = &[
    " ", " ", " ", "", "\n", "\n  ", "\n    ", "\n   ", "\t", "\r\n",
];

const SEED_PROGRAMS: &[&str] = &[
    "fn fib(n: Int) -> Int\n  if n < 2\n    return n\n  return fib(n - 1) + fib(n - 2)\n\n\
     fn main()\n  let x = fib(20)\n  print(\"fib(20) = {x}\")\n",
    "fn sign(n: Int) -> Text\n  if n < 0\n    return \"neg\"\n  else\n    if n == 0\n      \
     return \"zero\"\n  return \"pos\"\n\nfn main()\n  print(sign(-7 / 2 % 3))\n  \
     print(not (1 <= 2) or true and false)\n",
    "// comment\nfn greet(name: Text, loud: Bool) -> Text\n  if loud\n    return \"HI \\{ {name} \\}\"\n  \
     return \"hi {name}\\n\\t\\u{1F600}\"\n\nfn main()\n  print(greet(\"a\", true))\n",
];

fn token_soup(rng: &mut Rng) -> Vec<u8> {
    let n = 1 + rng.below(400) as usize;
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(rng.pick(TOKENS));
        s.push_str(rng.pick(SPACING));
    }
    s.into_bytes()
}

fn mutate(rng: &mut Rng) -> Vec<u8> {
    let mut data = rng.pick(SEED_PROGRAMS).as_bytes().to_vec();
    let edits = 1 + rng.below(8);
    for _ in 0..edits {
        if data.is_empty() {
            data.push(b'(');
            continue;
        }
        let at = rng.below(data.len() as u64) as usize;
        match rng.below(7) {
            0 => data[at] ^= 1 << rng.below(8),
            1 => {
                data.remove(at);
            }
            2 => data.insert(at, (rng.below(256)) as u8),
            3 => data.truncate(at),
            4 => {
                let tok = rng.pick(TOKENS).as_bytes().to_vec();
                data.splice(at..at, tok);
            }
            5 => {
                let end = (at + 1 + rng.below(40) as usize).min(data.len());
                let piece = data[at..end].to_vec();
                data.splice(at..at, piece);
            }
            _ => {
                let b = rng.pick(&[b'{', b'}', b'(', b')', b'"', b'\\', b'\t', b'\n', b' ', 0]);
                data.insert(at, b);
            }
        }
    }
    data
}

fn seeded_cases(out: &mut Out, seed: u64, count: u64) -> io::Result<()> {
    for i in 0..count {
        let case_seed = Rng::new(seed.wrapping_add(i)).next();
        let mut rng = Rng::new(case_seed);
        let (kind, data) = match i % 3 {
            0 => {
                let len = rng.below(4097) as usize;
                ("bytes", rng.bytes(len))
            }
            1 => ("soup", token_soup(&mut rng)),
            _ => ("mutant", mutate(&mut rng)),
        };
        out.write(&format!("seed_{kind}_{case_seed:016x}.ostl"), &data)?;
    }
    Ok(())
}

fn parse_u64(s: &str) -> Option<u64> {
    match s.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

fn usage() -> ExitCode {
    let _ = writeln!(
        io::stderr(),
        "usage: hostile-gen <out-dir> [--count N] [--seed S]"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut dir = None;
    let mut count = DEFAULT_COUNT;
    let mut seed = DEFAULT_SEED;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--count" | "--seed" => {
                let Some(v) = args.get(i + 1).and_then(|v| parse_u64(v)) else {
                    return usage();
                };
                if args[i] == "--count" {
                    count = v;
                } else {
                    seed = v;
                }
                i += 2;
            }
            a if a.starts_with('-') => return usage(),
            a => {
                if dir.is_some() {
                    return usage();
                }
                dir = Some(PathBuf::from(a));
                i += 1;
            }
        }
    }
    let Some(dir) = dir else {
        return usage();
    };
    match run(&dir, seed, count) {
        Ok(n) => {
            println!(
                "hostile-gen: {n} cases in {} (seed {seed:#x})",
                dir.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = writeln!(io::stderr(), "hostile-gen: {}: {e}", dir.display());
            ExitCode::from(1)
        }
    }
}

fn run(dir: &Path, seed: u64, count: u64) -> io::Result<usize> {
    fs::create_dir_all(dir)?;
    let mut out = Out {
        dir: dir.to_path_buf(),
        manifest: Vec::new(),
    };
    named_cases(&mut out, seed)?;
    seeded_cases(&mut out, seed, count)?;
    let mut manifest = out.manifest.join("\n");
    manifest.push('\n');
    fs::write(dir.join("MANIFEST"), manifest)?;
    Ok(out.manifest.len())
}
