//! Unit tests of the formatter rules. Every case also checks that formatting
//! the output again changes nothing.

use ostrel_core::FileId;

use crate::{Formatted, Refusal, format, format_source};

const FILE: FileId = FileId::from_raw(0);

fn run(src: &str) -> Result<Formatted, String> {
    let once = format_source(src, FILE).map_err(|r| format!("{r:?}"))?;
    let twice = format_source(&once.text, FILE).map_err(|r| format!("second pass: {r:?}"))?;
    if twice != once {
        return Err(format!(
            "not idempotent:\n{}\n--- then ---\n{}",
            once.text, twice.text
        ));
    }
    Ok(once)
}

fn fmt(src: &str) -> Result<String, String> {
    run(src).map(|f| f.text)
}

fn same(src: &str, want: &str) -> Result<(), String> {
    let got = fmt(src)?;
    if got == want {
        Ok(())
    } else {
        Err(format!("got:\n{got}\nwant:\n{want}"))
    }
}

/// A script program around `body`, which is indented by one level.
fn main_with(body: &str) -> String {
    format!("fn main()\n{body}")
}

#[test]
fn canonical_text_is_unchanged() -> Result<(), String> {
    let src = "fn f(n: Int) -> Int\n  return n * 2\n\nfn main()\n  let x = f(-1)\n  print(\"x is {x}\")\n";
    same(src, src)
}

#[test]
fn spaces_between_tokens_are_canonical() -> Result<(), String> {
    same(
        "fn  f( n :Int )->Int\n  return n*2+ f (n-1)\n\nfn main( )\n  print( f(1 ,2) )\n",
        "fn f(n: Int) -> Int\n  return n * 2 + f(n - 1)\n\nfn main()\n  print(f(1, 2))\n",
    )
}

#[test]
fn unary_minus_binds_and_binary_minus_is_spaced() -> Result<(), String> {
    same(
        &main_with("  let a = - 1\n  let b = a -  - a\n  let c = f(- a, -(a))\n  return -a\n"),
        &main_with("  let a = -1\n  let b = a - -a\n  let c = f(-a, -(a))\n  return -a\n"),
    )
}

#[test]
fn minus_after_value_keyword_subtracts() -> Result<(), String> {
    // Only the spacing matters here, not whether the program type checks.
    same(
        &main_with("  let a = now -b\n  let c = x[1] -1\n"),
        &main_with("  let a = now - b\n  let c = x[1] - 1\n"),
    )
}

#[test]
fn ranges_calls_and_indexes_have_no_inner_spaces() -> Result<(), String> {
    same(
        &main_with("  for i in 1 .. 10\n    print(xs [i])\n"),
        &main_with("  for i in 1..10\n    print(xs[i])\n"),
    )
}

#[test]
fn records_get_inner_spaces_and_sets_do_not() -> Result<(), String> {
    let src = "app A\n  auth paseto open\n\ndata Room\n  name: Text\n  members: Set[User] = { me }\n  make if members == {me}\n\nview Main\n  entry hint: \"x\"\n    on submit make Room {name: it}\n";
    let want = "app A\n  auth paseto open\n\ndata Room\n  name: Text\n  members: Set[User] = {me}\n  make if members == {me}\n\nview Main\n  entry hint: \"x\"\n    on submit make Room { name: it }\n";
    same(src, want)
}

#[test]
fn interpolation_has_no_spaces_at_its_braces() -> Result<(), String> {
    same(
        &main_with("  print(\"a { x  +  1 } b {y}\")\n"),
        &main_with("  print(\"a {x + 1} b {y}\")\n"),
    )
}

#[test]
fn blank_lines_collapse_and_are_trimmed_at_both_ends() -> Result<(), String> {
    same(
        "\n\n\nfn a()\n  print(1)\n\n\n\nfn main()\n\n\n  a()\n\n\n",
        "fn a()\n  print(1)\n\nfn main()\n\n  a()\n",
    )
}

#[test]
fn line_ends_bom_and_trailing_blanks_are_normalized() -> Result<(), String> {
    same(
        "\u{feff}fn main()   \r\n  print(1)  // note   \r\n  print(2)",
        "fn main()\n  print(1) // note\n  print(2)\n",
    )
}

#[test]
fn continuation_lines_are_one_level_deeper_than_their_bracket() -> Result<(), String> {
    same(
        &main_with("  print(f(1,\n            2,\n     3\n          ))\n"),
        &main_with("  print(f(1,\n    2,\n    3\n  ))\n"),
    )
}

#[test]
fn nested_continuation_lines_follow_the_line_that_opened_the_bracket() -> Result<(), String> {
    same(
        &main_with("  print(f(\n   g(\n         1\n  ),\n   2\n  ))\n"),
        &main_with("  print(f(\n    g(\n      1\n    ),\n    2\n  ))\n"),
    )
}

#[test]
fn trailing_comment_gets_one_space() -> Result<(), String> {
    same(
        &main_with("  print(1)// one\n  print(2)      // two\n"),
        &main_with("  print(1) // one\n  print(2) // two\n"),
    )
}

#[test]
fn comment_lines_keep_their_level_between_the_code_around_them() -> Result<(), String> {
    // `// end of a` belongs to the body of `a`, `// about main` to the next
    // item; a comment deeper than both is moved to the deeper one.
    same(
        "fn a()\n  print(1)\n  // end of a\n// about main\n        // too deep\nfn main()\n  a()\n",
        "fn a()\n  print(1)\n  // end of a\n// about main\n  // too deep\nfn main()\n  a()\n",
    )
}

#[test]
fn comment_before_a_block_may_take_the_level_of_the_block() -> Result<(), String> {
    let src = "fn main()\n// head\n  // body\n  print(1)\n";
    same(src, src)
}

#[test]
fn comment_inside_a_bracket_is_an_item() -> Result<(), String> {
    same(
        &main_with("  print(f(\n// first\n      1\n  ))\n"),
        &main_with("  print(f(\n    // first\n    1\n  ))\n"),
    )
}

#[test]
fn style_body_keeps_its_css_and_comments() -> Result<(), String> {
    let src = "fn main()\n  print(1)\n\nstyle\n  .mine {\n    background: url(//cdn.example/x.png);\n    color: red;/* hue\n         stays */\n  }\n  /* between */\n  .b { x: y }\n";
    same(src, src)
}

#[test]
fn style_comment_line_never_leaves_the_body() -> Result<(), String> {
    // A comment line one column deep is still in the body (it is deeper than
    // `style`); written at column 0 it would end the body and become code.
    same(
        "fn main()\n  print(1)\n\nstyle\n  .a { b: c }\n /* last */\n",
        "fn main()\n  print(1)\n\nstyle\n  .a { b: c }\n  /* last */\n",
    )
}

#[test]
fn long_lines_are_reported_and_kept() -> Result<(), String> {
    let long = format!("  print(\"{}\")\n", "x".repeat(100));
    let src = main_with(&long);
    let out = run(&src)?;
    if out.text != src || out.long_lines != vec![2] {
        return Err(format!("{out:?}"));
    }
    Ok(())
}

#[test]
fn width_counts_characters_not_bytes() -> Result<(), String> {
    // 91 columns, 181 bytes.
    let src = main_with(&format!("  print(\"{}\")\n", "\u{e4}".repeat(80)));
    let out = run(&src)?;
    if !out.long_lines.is_empty() {
        return Err(format!("{:?}", out.long_lines));
    }
    Ok(())
}

#[test]
fn invalid_source_is_refused_with_the_check_diagnostics() -> Result<(), String> {
    for src in [
        "fn main(\n",
        "fn main()\n      print(1)\n",
        "fn main()\n\tprint(1)\n",
    ] {
        match format_source(src, FILE) {
            Err(Refusal::Invalid(diags)) if !diags.is_empty() => {}
            other => return Err(format!("{src:?}: {other:?}")),
        }
    }
    Ok(())
}

#[test]
fn empty_and_comment_only_files_need_no_main() -> Result<(), String> {
    same("", "")?;
    same("\n\n   \n", "")?;
    same("  // only a comment  \n\n", "// only a comment\n")
}

#[test]
fn tokens_of_another_text_are_a_defect_not_a_panic() -> Result<(), String> {
    let (tokens, _) = ostrel_syntax::lex::lex("fn main()\n  print(1)\n", FILE);
    match format("x", FILE, &tokens) {
        Err(Refusal::Defect(_)) => Ok(()),
        other => Err(format!("{other:?}")),
    }
}

#[test]
fn formatting_is_linear_in_the_number_of_comment_lines() -> Result<(), String> {
    // A run of comment lines shares one look ahead (no quadratic rescans).
    let build = |n: usize| main_with(&format!("{}  print(1)\n", "  // c\n".repeat(n)));
    let small = build(2_000);
    let large = build(64_000);
    let time = |src: &str| -> Result<std::time::Duration, String> {
        let start = std::time::Instant::now();
        fmt(src)?;
        Ok(start.elapsed())
    };
    let mut batch = std::time::Duration::MAX;
    let mut big = std::time::Duration::MAX;
    for _ in 0..3 {
        let start = std::time::Instant::now();
        for _ in 0..32 {
            fmt(&small)?;
        }
        batch = batch.min(start.elapsed());
        big = big.min(time(&large)?);
    }
    let bound = batch.max(std::time::Duration::from_millis(1)) * 4;
    if big < bound {
        Ok(())
    } else {
        Err(format!(
            "{big:?} for one large file, {batch:?} for 32 small ones"
        ))
    }
}

#[test]
fn tokens_that_would_merge_keep_a_space() {
    use crate::space::{Side, space};
    use ostrel_syntax::lex::{Keyword, TokenKind as K};
    let unary_minus = Side::flagged(K::Minus, true);
    let word = [K::Ident, K::Int, K::Kw(Keyword::Me), K::Kw(Keyword::None)];
    let mut pairs = vec![
        (Side::plain(K::Dot), Side::plain(K::Dot)),
        (Side::plain(K::Dot), Side::plain(K::DotDot)),
        (Side::plain(K::DotDot), Side::plain(K::Dot)),
        (Side::plain(K::DotDot), Side::plain(K::DotDot)),
        (Side::plain(K::Question), Side::plain(K::Question)),
        (Side::plain(K::Question), Side::plain(K::QuestionQuestion)),
        (unary_minus, Side::plain(K::Minus)),
        (unary_minus, Side::plain(K::Gt)),
        (unary_minus, Side::plain(K::Ge)),
        (unary_minus, Side::plain(K::Arrow)),
    ];
    for a in word {
        for b in word {
            pairs.push((Side::plain(a), Side::plain(b)));
        }
    }
    for (a, b) in pairs {
        assert!(space(a, b), "no space between {a:?} and {b:?}");
    }
}

#[test]
fn format_keeps_tokens_apart_that_would_merge() {
    // These texts lex but do not parse, so only `format` reaches them; the
    // spacing rules alone would write `a...b`, `b??` and `-->`.
    for (src, want) in [
        ("a.. .b ? ?\n", "a.. .b? ?\n"),
        ("a . .. b\n", "a. ..b\n"),
        ("a - - > b\n", "a - - > b\n"),
        ("a - - >= b\n", "a - - >= b\n"),
        ("a - - -> b\n", "a - - -> b\n"),
    ] {
        let (tokens, diags) = ostrel_syntax::lex::lex(src, FILE);
        assert!(!diags.iter().any(|d| d.is_error()), "{src:?}: {diags:?}");
        let got = format(src, FILE, &tokens).map(|f| f.text);
        assert_eq!(got, Ok(want.to_string()), "{src:?}");
    }
}
