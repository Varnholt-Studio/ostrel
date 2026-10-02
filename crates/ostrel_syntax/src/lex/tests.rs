//! Unit tests of the lexer driver.
//!
//! The driver is tested against [`StubLiterals`], a minimal stand in for the
//! literal scanner of WP T1-2b: integers are digit runs with the `Int` range
//! check, string text knows `\` escapes only as "skip the next character". The
//! escape goldens (E0004, E0005) are checked with the real scanner.

use super::codes;
use super::*;
use ostrel_core::{Code, SourceMap};

struct StubLiterals;

impl LiteralScan for StubLiterals {
    fn int(&self, src: &str, start: usize, file: FileId, diags: &mut Vec<Diagnostic>) -> usize {
        let len = src
            .get(start..)
            .unwrap_or("")
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let end = start + len;
        let fits = src
            .get(start..end)
            .and_then(|t| t.parse::<u64>().ok())
            .is_some_and(|v| v <= 9_007_199_254_740_991);
        if !fits {
            let span = Span::new(file, start as u32, end as u32);
            diags.push(Diagnostic::error(
                codes::INT_OUT_OF_RANGE,
                span,
                codes::MSG_INT_OUT_OF_RANGE,
            ));
        }
        end
    }

    fn string_text(
        &self,
        src: &str,
        start: usize,
        _file: FileId,
        _diags: &mut Vec<Diagnostic>,
    ) -> (usize, TextStop) {
        let mut chars = src.get(start..).unwrap_or("").char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => return (start + i + 1, TextStop::Quote),
                '{' => return (start + i + 1, TextStop::Brace),
                '\n' => return (start + i, TextStop::LineEnd),
                '\r' if src
                    .get(start + i + 1..)
                    .is_some_and(|r| r.starts_with('\n')) =>
                {
                    return (start + i, TextStop::LineEnd);
                }
                '\\' if chars.peek().is_some_and(|&(_, n)| n != '\n' && n != '\r') => {
                    let n = chars.next().map_or(' ', |(_, n)| n);
                    if n == 'u' && chars.peek().is_some_and(|&(_, b)| b == '{') {
                        while chars
                            .next_if(|&(_, b)| !matches!(b, '}' | '"' | '\n'))
                            .is_some()
                        {}
                        chars.next_if(|&(_, b)| b == '}');
                    }
                }
                _ => {}
            }
        }
        (src.len(), TextStop::LineEnd)
    }
}

/// A scanner that breaks the contract in every way it can.
struct BrokenLiterals;

impl LiteralScan for BrokenLiterals {
    fn int(&self, _: &str, start: usize, _: FileId, _: &mut Vec<Diagnostic>) -> usize {
        start
    }

    fn string_text(
        &self,
        src: &str,
        start: usize,
        _: FileId,
        _: &mut Vec<Diagnostic>,
    ) -> (usize, TextStop) {
        if start.is_multiple_of(2) {
            (src.len() + 10, TextStop::Quote)
        } else {
            (start.saturating_sub(1), TextStop::Brace)
        }
    }
}

const F: FileId = FileId::from_raw(0);

fn lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    lex_with(src, F, &StubLiterals)
}

fn kinds(src: &str) -> Vec<TokenKind> {
    let (tokens, diags) = lex(src);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    tokens.iter().map(|t| t.kind).collect()
}

fn texts<'a>(src: &'a str, tokens: &[Token]) -> Vec<&'a str> {
    tokens
        .iter()
        .map(|t| {
            src.get(t.span.start as usize..t.span.end as usize)
                .unwrap_or("<bad span>")
        })
        .collect()
}

/// Code and start offset of the first diagnostic.
fn first(diags: &[Diagnostic]) -> Option<(Code, u32)> {
    diags.first().map(|d| (d.code, d.span.start))
}

/// Renders the diagnostics of `src` like the CLI does for `path`.
fn rendered(path: &str, src: &str) -> String {
    let mut map = SourceMap::new();
    let Ok(file) = map.add(path, src) else {
        return String::from("<file not added>");
    };
    let (_, diags) = lex_with(src, file, &StubLiterals);
    diags
        .iter()
        .map(|d| d.render(&map) + "\n")
        .collect::<String>()
}

use TokenKind::*;

const fn kw(k: Keyword) -> TokenKind {
    Kw(k)
}

#[test]
fn token_stays_small() {
    assert!(std::mem::size_of::<Token>() <= 24);
    assert_eq!(std::mem::size_of::<Token>(), 16);
}

#[test]
fn keyword_table_matches_syntax_6() {
    let list = "app auth home data enum check merge fixed server serial per var let fn call as \
                system return if else for in where sort desc limit last see make edit drop \
                view style extern on and or not me now none signed true false catch";
    let words: Vec<&str> = list.split_whitespace().collect();
    assert_eq!(words.len(), 45);
    assert_eq!(Keyword::ALL.len(), 45);
    for (word, kw) in words.iter().zip(Keyword::ALL) {
        assert_eq!(kw.as_str(), *word);
        assert_eq!(Keyword::from_text(word), Some(*kw));
    }
    for word in [
        "is", "set", "from", "use", "new", "it", "js", "client", "print", "Let",
    ] {
        assert_eq!(Keyword::from_text(word), None, "{word}");
    }
}

#[test]
fn layout_of_the_v0_1_example() {
    let src = "fn fib(n: Int) -> Int\n  if n < 2\n    return n\n  return fib(n - 1) + fib(n - 2)\n\nfn main()\n  print(\"hi\")\n";
    let expected = vec![
        kw(Keyword::Fn),
        Ident,
        LParen,
        Ident,
        Colon,
        Ident,
        RParen,
        Arrow,
        Ident,
        Nl,
        Indent,
        kw(Keyword::If),
        Ident,
        Lt,
        Int,
        Nl,
        Indent,
        kw(Keyword::Return),
        Ident,
        Nl,
        Dedent,
        kw(Keyword::Return),
        Ident,
        LParen,
        Ident,
        Minus,
        Int,
        RParen,
        Plus,
        Ident,
        LParen,
        Ident,
        Minus,
        Int,
        RParen,
        Nl,
        Dedent,
        kw(Keyword::Fn),
        Ident,
        LParen,
        RParen,
        Nl,
        Indent,
        Ident,
        LParen,
        Str,
        RParen,
        Nl,
        Dedent,
        Eof,
    ];
    assert_eq!(kinds(src), expected);
}

#[test]
fn blank_and_comment_lines_have_no_layout() {
    let src = "fn main()\n\n    \n      // deep comment\n// top comment\n  print(1) // trailing\n";
    assert_eq!(
        kinds(src),
        vec![
            kw(Keyword::Fn),
            Ident,
            LParen,
            RParen,
            Nl,
            Comment,
            Comment,
            Indent,
            Ident,
            LParen,
            Int,
            RParen,
            Comment,
            Nl,
            Dedent,
            Eof,
        ]
    );
}

#[test]
fn comment_token_ends_before_the_line_end() {
    let src = "// a / b\r\nx\n";
    let (tokens, diags) = lex(src);
    assert!(diags.is_empty());
    assert_eq!(texts(src, &tokens), vec!["// a / b", "x", "\n", ""]);
}

#[test]
fn last_line_without_line_end_is_closed() {
    let src = "fn main()\n  print(1)";
    let (tokens, _) = lex(src);
    let tail: Vec<TokenKind> = tokens.iter().rev().take(3).map(|t| t.kind).collect();
    assert_eq!(tail, vec![Eof, Dedent, Nl]);
    let nl = tokens
        .iter()
        .rev()
        .nth(2)
        .map(|t| (t.span.start, t.span.end));
    assert_eq!(nl, Some((src.len() as u32, src.len() as u32)));
}

#[test]
fn empty_input_is_only_eof() {
    assert_eq!(kinds(""), vec![Eof]);
    assert_eq!(kinds("\n\n// c\n"), vec![Comment, Eof]);
    assert_eq!(kinds("\u{FEFF}x"), vec![Ident, Nl, Eof]);
}

#[test]
fn crlf_is_a_line_end() {
    assert_eq!(
        kinds("fn main()\r\n  x\r\n"),
        vec![
            kw(Keyword::Fn),
            Ident,
            LParen,
            RParen,
            Nl,
            Indent,
            Ident,
            Nl,
            Dedent,
            Eof
        ]
    );
}

#[test]
fn dedent_by_several_levels_and_multi_level_indent() {
    let src = "a\n  b\n    c\nd\n      e\n";
    assert_eq!(
        kinds(src),
        vec![
            Ident, Nl, Indent, Ident, Nl, Indent, Ident, Nl, Dedent, Dedent, Ident, Nl, Indent,
            Indent, Indent, Ident, Nl, Dedent, Dedent, Dedent, Eof,
        ]
    );
}

#[test]
fn open_paren_continues_the_line() {
    let src = "fn main()\n  print(add(1,\n2,\n      3))\n  x\n";
    assert_eq!(
        kinds(src),
        vec![
            kw(Keyword::Fn),
            Ident,
            LParen,
            RParen,
            Nl,
            Indent,
            Ident,
            LParen,
            Ident,
            LParen,
            Int,
            Comma,
            Int,
            Comma,
            Int,
            RParen,
            RParen,
            Nl,
            Ident,
            Nl,
            Dedent,
            Eof,
        ]
    );
}

#[test]
fn brackets_and_braces_do_not_continue_a_line_in_v0_1() {
    assert!(continues_line('('));
    assert!(!continues_line('['));
    assert!(!continues_line('{'));
    assert_eq!(kinds("a [\nb\n"), vec![Ident, LBracket, Nl, Ident, Nl, Eof]);
}

#[test]
fn stray_closing_paren_does_not_break_layout() {
    assert_eq!(kinds("a)\nb\n"), vec![Ident, RParen, Nl, Ident, Nl, Eof]);
}

#[test]
fn punctuation_and_operators() {
    let src = "( ) [ ] { } , . .. : -> ? ?? | ~ + - * / % = == != < <= > >=";
    assert_eq!(
        kinds(src),
        vec![
            LParen,
            RParen,
            LBracket,
            RBracket,
            LBrace,
            RBrace,
            Comma,
            Dot,
            DotDot,
            Colon,
            Arrow,
            Question,
            QuestionQuestion,
            Pipe,
            Tilde,
            Plus,
            Minus,
            Star,
            Slash,
            Percent,
            Eq,
            EqEq,
            BangEq,
            Lt,
            Le,
            Gt,
            Ge,
            Nl,
            Eof,
        ]
    );
}

#[test]
fn false_friends_lex_as_separate_tokens() {
    // `+=` and `:=` are parser diagnostics (E0020); the lexer stays neutral.
    assert_eq!(kinds("x += 1"), vec![Ident, Plus, Eq, Int, Nl, Eof]);
    assert_eq!(kinds("x := 1"), vec![Ident, Colon, Eq, Int, Nl, Eof]);
    assert_eq!(
        kinds("if x is 1"),
        vec![kw(Keyword::If), Ident, Ident, Int, Nl, Eof]
    );
}

#[test]
fn identifiers_are_ascii_with_digits_and_underscore() {
    let src = "a1_b B2 letx fnx x_";
    let (tokens, diags) = lex(src);
    assert!(diags.is_empty());
    assert_eq!(
        texts(src, &tokens).get(..5),
        Some(&["a1_b", "B2", "letx", "fnx", "x_"][..])
    );
    assert!(tokens.iter().take(5).all(|t| t.kind == Ident));
}

#[test]
fn interpolated_string_pieces() {
    let src = "\"a {x} b {f(y)} c\" \"{x}{y}\" \"\\{not\\} {z}\"";
    let (tokens, diags) = lex(src);
    assert!(diags.is_empty());
    let got: Vec<(TokenKind, &str)> = tokens
        .iter()
        .map(|t| t.kind)
        .zip(texts(src, &tokens))
        .collect();
    assert_eq!(
        got,
        vec![
            (StrHead, "\"a {"),
            (Ident, "x"),
            (StrMid, "} b {"),
            (Ident, "f"),
            (LParen, "("),
            (Ident, "y"),
            (RParen, ")"),
            (StrTail, "} c\""),
            (StrHead, "\"{"),
            (Ident, "x"),
            (StrMid, "}{"),
            (Ident, "y"),
            (StrTail, "}\""),
            (StrHead, "\"\\{not\\} {"),
            (Ident, "z"),
            (StrTail, "}\""),
            (Nl, ""),
            (Eof, ""),
        ]
    );
}

#[test]
fn braces_inside_interpolation_are_counted() {
    let src = "\"{ {a} }\"";
    assert_eq!(
        kinds(src),
        vec![StrHead, LBrace, Ident, RBrace, StrTail, Nl, Eof]
    );
}

#[test]
fn depth_32_is_accepted() {
    let src = format!("\"{}x{}\"", "{".repeat(32), "}".repeat(32));
    let (tokens, diags) = lex(&src);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(tokens.iter().filter(|t| t.kind == LBrace).count(), 31);
    assert_eq!(tokens.iter().filter(|t| t.kind == RBrace).count(), 31);
}

#[test]
fn depth_33_is_one_error_at_the_33rd_brace() {
    let src = format!("x = \"{}x{}\" + 1", "{".repeat(33), "}".repeat(33));
    let (tokens, diags) = lex(&src);
    assert_eq!(diags.len(), 1);
    assert_eq!(
        first(&diags),
        Some((codes::INTERPOLATION_TOO_DEEP, 4 + 1 + 32))
    );
    // The whole string is one Error token; lexing resumes after it.
    let k: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
    assert_eq!(k, vec![Ident, Eq, Error, Plus, Int, Nl, Eof]);
    assert_eq!(
        texts(&src, &tokens).get(2).map(|t| t.len()),
        Some(2 + 66 + 1)
    );
}

#[test]
fn line_end_inside_interpolation_is_unterminated() {
    let src = "x = \"a {b\ny\n";
    let (tokens, diags) = lex(src);
    assert_eq!(diags.len(), 1);
    assert_eq!(first(&diags), Some((codes::UNTERMINATED_STRING, 4)));
    let k: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
    assert_eq!(k, vec![Ident, Eq, Error, Nl, Ident, Nl, Eof]);
}

#[test]
fn line_end_after_interpolation_is_unterminated_at_the_opening_quote() {
    let src = "s = \"a {b} c\nt\n";
    let (tokens, diags) = lex(src);
    assert_eq!(diags.len(), 1);
    assert_eq!(first(&diags), Some((codes::UNTERMINATED_STRING, 4)));
    assert_eq!(texts(src, &tokens).get(2).copied(), Some("\"a {b} c"));
}

#[test]
fn unterminated_string_ends_an_open_paren() {
    let src = "f(\"a\nb\n";
    let (tokens, diags) = lex(src);
    assert_eq!(diags.len(), 1);
    let k: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
    assert_eq!(k, vec![Ident, LParen, Error, Nl, Ident, Nl, Eof]);
}

#[test]
fn unbalanced_paren_inside_interpolation_does_not_leak() {
    // The `(` inside the string must not join the following lines.
    let src = "a \"{(x}\"\nb\n";
    assert_eq!(
        kinds(src),
        vec![Ident, StrHead, LParen, Ident, StrTail, Nl, Ident, Nl, Eof]
    );
    let src = "a \"{(x\nb\n";
    let (tokens, diags) = lex(src);
    assert_eq!(diags.len(), 1);
    let k: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
    assert_eq!(k, vec![Ident, Error, Nl, Ident, Nl, Eof]);
}

#[test]
fn semicolon_is_reported_once_and_separates_statements() {
    let (tokens, diags) = lex("let x = 1; print(x)\n");
    assert_eq!(diags.len(), 1);
    assert_eq!(first(&diags).map(|d| d.0), Some(codes::SEMICOLON));
    let k: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
    assert_eq!(
        k,
        vec![
            kw(Keyword::Let),
            Ident,
            Eq,
            Int,
            Nl,
            Ident,
            LParen,
            Ident,
            RParen,
            Nl,
            Eof
        ]
    );
    // Inside an interpolation the `;` never becomes a line end.
    let (tokens, diags) = lex("\"{a;b}\"");
    assert_eq!(diags.len(), 1);
    assert!(!tokens.iter().rev().skip(2).any(|t| t.kind == Nl));
}

#[test]
fn unexpected_run_is_one_diagnostic() {
    let src = "a @@b ü¨ß c";
    let (tokens, diags) = lex(src);
    assert_eq!(diags.len(), 2, "{diags:?}");
    let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages,
        ["unexpected character `@`", "unexpected character `ü`"]
    );
    assert_eq!(
        texts(src, &tokens).get(..4),
        Some(&["a", "@@b", "ü¨ß", "c"][..])
    );
    assert_eq!(tokens.get(1).map(|t| t.kind), Some(Error));
    // `!` alone is not an operator.
    let (_, diags) = lex("!x");
    assert_eq!(diags.len(), 1);
    // A lone CR and other control characters are unexpected.
    let (_, diags) = lex("a\rb\u{0}c");
    assert_eq!(diags.len(), 2);
}

#[test]
fn bidi_controls_are_rejected_everywhere_but_escapes() {
    for c in [
        '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}',
        '\u{2068}', '\u{2069}',
    ] {
        assert!(codes::is_bidi_control(c));
        let src = format!("x {c}= 1 // a{c}b{c}\n");
        let (tokens, diags) = lex(&src);
        assert_eq!(diags.len(), 3, "{c:?}");
        assert!(diags.iter().all(|d| d.code == codes::BIDI_CONTROL));
        assert!(diags.iter().all(|d| d.span.len() == 3));
        assert!(diags.iter().all(|d| !d.message.contains(c)));
        let k: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(k, vec![Ident, Error, Eq, Int, Comment, Nl, Eof]);
    }
    for c in ['\u{200B}', '\u{200F}', '\u{061C}', '\u{2060}', '\u{2029}'] {
        assert!(!codes::is_bidi_control(c));
    }
    // The escape form is plain text for the driver.
    assert_eq!(kinds("\"\\u{202E}\""), vec![Str, Nl, Eof]);
}

#[test]
fn escape_braces_do_not_count_in_recovery() {
    // After E0006 the rest of the string is skipped; `\u{41}` must not open a
    // brace there, or the skip would run past the closing quote.
    let src = "x = \"{\"a\"} \\u{41}\" + 1\n";
    let (tokens, diags) = lex(src);
    assert_eq!(diags.len(), 1);
    let k: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
    assert_eq!(k, vec![Ident, Eq, Error, Plus, Int, Nl, Eof]);
}

#[test]
fn tab_after_the_indentation_is_whitespace() {
    assert_eq!(kinds("a\t=\t1"), vec![Ident, Eq, Int, Nl, Eof]);
}

#[test]
fn faulty_indentation_still_opens_one_block() {
    for src in [
        "fn main()\n\tprint(1)\n",
        "fn main()\n   print(1)\n",
        "fn main()\n \tprint(1)\n",
    ] {
        let (tokens, diags) = lex(src);
        assert_eq!(diags.len(), 1, "{src:?}");
        let k: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            k,
            vec![
                kw(Keyword::Fn),
                Ident,
                LParen,
                RParen,
                Nl,
                Indent,
                Ident,
                LParen,
                Int,
                RParen,
                Nl,
                Dedent,
                Eof,
            ],
            "{src:?}"
        );
    }
}

#[test]
fn every_indent_is_closed() {
    let src = "a\n  b\n    c\n      d\n        e";
    let (tokens, _) = lex(src);
    let indents = tokens.iter().filter(|t| t.kind == Indent).count();
    let dedents = tokens.iter().filter(|t| t.kind == Dedent).count();
    assert_eq!((indents, dedents), (4, 4));
    assert_eq!(tokens.last().map(|t| t.kind), Some(Eof));
}

#[test]
fn broken_scanner_cannot_hang_or_panic_the_driver() {
    let src = "x = 12 + \"ab{c}d\" \"e\" 3\n\"ü\n";
    let (tokens, _) = lex_with(src, F, &BrokenLiterals);
    assert_eq!(tokens.last().map(|t| t.kind), Some(Eof));
    for t in &tokens {
        assert!(t.span.start <= t.span.end && t.span.end as usize <= src.len());
        assert!(src.is_char_boundary(t.span.start as usize));
        assert!(src.is_char_boundary(t.span.end as usize));
    }
}

#[test]
fn hostile_inputs_end_with_eof_and_valid_spans() {
    let mut cases: Vec<String> = vec![
        "\"".into(),
        "\"{".into(),
        "\"{\"".into(),
        "\"}".into(),
        "\\".into(),
        "\"\\".into(),
        "((((((((".into(),
        "))))".into(),
        "\u{FEFF}\u{FEFF}".into(),
        "\r".into(),
        "\r\r\n".into(),
        " \t \t".into(),
        "{{{{}}}}".into(),
        "\"{{{{\"}}}}".into(),
    ];
    cases.push(format!("\"{}", "{".repeat(100_000)));
    cases.push(format!("{}x", " ".repeat(100_001)));
    cases.push("a\n".repeat(10_000) + &"  ".repeat(5_000) + "b");
    cases.push((0u8..=127).map(char::from).collect());
    for src in &cases {
        let (tokens, _) = lex(src);
        assert_eq!(tokens.iter().filter(|t| t.kind == Eof).count(), 1);
        assert_eq!(tokens.last().map(|t| t.kind), Some(Eof));
        let indents = tokens.iter().filter(|t| t.kind == Indent).count();
        let dedents = tokens.iter().filter(|t| t.kind == Dedent).count();
        assert_eq!(indents, dedents);
        for t in &tokens {
            assert!(t.span.end as usize <= src.len());
            assert!(src.is_char_boundary(t.span.start as usize));
            assert!(src.is_char_boundary(t.span.end as usize));
        }
    }
}

#[test]
fn large_input_is_linear() {
    let line = "  let x = add(1, 2) + \"v {y} w\" // c\n";
    let src = format!("fn main()\n{}", line.repeat(200_000));
    let start = std::time::Instant::now();
    let (tokens, diags) = lex(&src);
    assert!(diags.is_empty());
    assert!(tokens.len() > 3_000_000);
    assert!(start.elapsed().as_secs() < 5, "{:?}", start.elapsed());
}

macro_rules! golden {
    ($name:ident) => {
        #[test]
        fn $name() {
            let path = concat!("tests/errors/", stringify!($name), ".ostl");
            let src = include_str!(concat!(
                "../../../../tests/errors/",
                stringify!($name),
                ".ostl"
            ));
            let expected = include_str!(concat!(
                "../../../../tests/errors/",
                stringify!($name),
                ".expected_err"
            ));
            assert_eq!(rendered(path, src), expected);
        }
    };
}

// Error goldens whose diagnostic comes from the driver. The escape goldens
// (E0004, E0005) belong to the literal scanner; the integer golden also runs
// here because the stub checks the range like the real scanner.
golden!(lex_tab_indent);
golden!(lex_odd_indent);
golden!(lex_unterminated_string);
golden!(lex_string_in_interpolation);
golden!(lex_interpolation_too_deep);
golden!(lex_unexpected_char);
golden!(lex_non_ascii_name);
golden!(lex_column_counts_scalars);
golden!(lex_semicolon);
golden!(lex_int_literal_too_large);

macro_rules! clean {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../../../../tests/errors/", $name, ".ostl"))),)*]
    };
}

/// Every parser and checker golden lexes without a diagnostic, so the lexer
/// adds no cascade in front of them.
#[test]
fn other_goldens_have_no_lexer_diagnostic() {
    let cases: &[(&str, &str)] = clean!(
        "parse_compound_assign",
        "parse_walrus_assign",
        "parse_is_operator",
        "parse_brace_block",
        "parse_colon_block_head",
        "parse_else_if_one_line",
        "parse_unclosed_paren",
        "parse_keyword_as_name",
        "parse_comparison_chain",
        "type_text_plus",
        "type_arity",
        "type_condition_not_bool",
        "type_implicit_return_in_branch",
        "name_unknown",
        "name_duplicate_fn",
        "name_duplicate_param",
        "name_unknown_type",
        "check_float_type_v0_1",
        "entry_missing_main",
        "entry_main_params",
        "entry_main_result",
    );
    for (name, src) in cases {
        let (tokens, diags) = lex(src);
        assert!(diags.is_empty(), "{name}: {diags:?}");
        assert_eq!(tokens.last().map(|t| t.kind), Some(Eof), "{name}");
    }
}

#[test]
fn diagnostic_codes_match_the_catalog() {
    let expected: &[(Code, u16)] = &[
        (codes::TAB_INDENT, 1),
        (codes::ODD_INDENT, 2),
        (codes::UNTERMINATED_STRING, 3),
        (codes::BAD_ESCAPE, 4),
        (codes::NOT_SCALAR, 5),
        (codes::STRING_IN_INTERPOLATION, 6),
        (codes::INTERPOLATION_TOO_DEEP, 7),
        (codes::UNEXPECTED_CHAR, 8),
        (codes::SEMICOLON, 9),
        (codes::INT_OUT_OF_RANGE, 10),
        (codes::BIDI_CONTROL, 11),
    ];
    for (code, number) in expected {
        assert_eq!(code.number(), *number);
    }
}
