//! Unit tests of the statement parser.
//!
//! Expressions are read by [`TestExprs`], a small recursive stand in for the
//! expression parser of WP T1-3b. It knows the v0.1 operators, calls,
//! parentheses and strings, which is enough to drive declarations, blocks,
//! statements, recovery and limits. Tokens come from the real lexer driver
//! with [`StubLiterals`], a minimal literal scanner.

use super::*;
use crate::ast::{BinaryOp, Item, MAX_HEIGHT, MAX_NODES, StrPart, UnaryOp};
use crate::lex::{LiteralScan, TextStop, lex_with};
use ostrel_core::SourceMap;

const F: FileId = FileId::from_raw(0);

/// Literal scanner for tests: integers are digit runs, string text knows
/// `\` only as "skip the next character".
struct StubLiterals;

impl LiteralScan for StubLiterals {
    fn int(&self, src: &str, start: usize, _: FileId, _: &mut Vec<Diagnostic>) -> usize {
        let digits = src
            .get(start..)
            .unwrap_or("")
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        start + digits.max(1)
    }

    fn string_text(
        &self,
        src: &str,
        start: usize,
        _: FileId,
        _: &mut Vec<Diagnostic>,
    ) -> (usize, TextStop) {
        let mut chars = src.get(start..).unwrap_or("").char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => return (start + i + 1, TextStop::Quote),
                '{' => return (start + i + 1, TextStop::Brace),
                '\n' | '\r' => return (start + i, TextStop::LineEnd),
                // Skips the escaped character, never a line end; `\u{...}` is
                // skipped whole, so its braces never open an interpolation.
                '\\' => {
                    let escaped = chars.next_if(|&(_, n)| n != '\n' && n != '\r');
                    if escaped.is_some_and(|(_, n)| n == 'u')
                        && chars.next_if(|&(_, n)| n == '{').is_some()
                    {
                        while chars
                            .next_if(|&(_, n)| !matches!(n, '}' | '"' | '\n' | '\r'))
                            .is_some()
                        {}
                        chars.next_if(|&(_, n)| n == '}');
                    }
                }
                _ => {}
            }
        }
        (src.len(), TextStop::LineEnd)
    }
}

/// Recursive expression parser for tests (not the D43 parser).
struct TestExprs;

impl ExprParse for TestExprs {
    fn expr(&self, p: &mut Parser<'_>) -> Result<ExprId, Stop> {
        or_expr(p)
    }
}

fn binary(p: &mut Parser<'_>, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Result<ExprId, Stop> {
    let range = range_of(p, lhs).cover(range_of(p, rhs));
    p.add_expr(ExprKind::Binary { op, lhs, rhs }, range)
}

fn range_of(p: &Parser<'_>, id: ExprId) -> TextRange {
    p.module()
        .expr(id)
        .map_or(TextRange::default(), |e| e.range)
}

fn or_expr(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let mut lhs = and_expr(p)?;
    while p.peek() == TokenKind::Kw(Keyword::Or) {
        p.bump();
        let rhs = and_expr(p)?;
        lhs = binary(p, BinaryOp::Or, lhs, rhs)?;
    }
    Ok(lhs)
}

fn and_expr(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let mut lhs = not_expr(p)?;
    while p.peek() == TokenKind::Kw(Keyword::And) {
        p.bump();
        let rhs = not_expr(p)?;
        lhs = binary(p, BinaryOp::And, lhs, rhs)?;
    }
    Ok(lhs)
}

fn not_expr(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    if p.peek() == TokenKind::Kw(Keyword::Not) {
        let tok = p.bump();
        let operand = not_expr(p)?;
        let range = text_range(tok.span.start, p.prev_end());
        return p.add_expr(
            ExprKind::Unary {
                op: UnaryOp::Not,
                operand,
            },
            range,
        );
    }
    cmp_expr(p)
}

fn cmp_op(kind: TokenKind) -> Option<BinaryOp> {
    Some(match kind {
        TokenKind::EqEq => BinaryOp::Eq,
        TokenKind::BangEq => BinaryOp::Ne,
        TokenKind::Lt => BinaryOp::Lt,
        TokenKind::Le => BinaryOp::Le,
        TokenKind::Gt => BinaryOp::Gt,
        TokenKind::Ge => BinaryOp::Ge,
        _ => return None,
    })
}

fn cmp_expr(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let lhs = add_expr(p)?;
    let Some(op) = cmp_op(p.peek()) else {
        return Ok(lhs);
    };
    p.bump();
    let rhs = add_expr(p)?;
    if cmp_op(p.peek()).is_some() {
        let at = p.current();
        return Err(p.report(codes::COMPARISON_CHAIN, at, codes::MSG_COMPARISON_CHAIN));
    }
    binary(p, op, lhs, rhs)
}

fn add_expr(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let mut lhs = mul_expr(p)?;
    loop {
        let op = match p.peek() {
            TokenKind::Plus => BinaryOp::Add,
            TokenKind::Minus => BinaryOp::Sub,
            _ => return Ok(lhs),
        };
        p.bump();
        let rhs = mul_expr(p)?;
        lhs = binary(p, op, lhs, rhs)?;
    }
}

fn mul_expr(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let mut lhs = unary(p)?;
    loop {
        let op = match p.peek() {
            TokenKind::Star => BinaryOp::Mul,
            TokenKind::Slash => BinaryOp::Div,
            TokenKind::Percent => BinaryOp::Rem,
            _ => return Ok(lhs),
        };
        p.bump();
        let rhs = unary(p)?;
        lhs = binary(p, op, lhs, rhs)?;
    }
}

fn unary(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    if p.peek() == TokenKind::Minus {
        let tok = p.bump();
        let operand = postfix(p)?;
        let range = text_range(tok.span.start, p.prev_end());
        return p.add_expr(
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            },
            range,
        );
    }
    postfix(p)
}

fn postfix(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let start = p.current().span.start;
    let callee = primary(p)?;
    if p.peek() != TokenKind::LParen {
        return Ok(callee);
    }
    let open = p.bump();
    p.enter(open)?;
    let mut args = Vec::new();
    if p.peek() != TokenKind::RParen {
        loop {
            args.push(or_expr(p)?);
            if p.peek() != TokenKind::Comma {
                break;
            }
            p.bump();
        }
    }
    if p.peek() != TokenKind::RParen {
        return Err(p.expected("`,` or `)`"));
    }
    p.bump();
    p.leave();
    let range = text_range(start, p.prev_end());
    p.add_expr(
        ExprKind::Call {
            callee,
            args: args.into_boxed_slice(),
        },
        range,
    )
}

fn primary(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let tok = p.current();
    let range = text_range(tok.span.start, tok.span.end);
    let kind = match tok.kind {
        TokenKind::Int => ExprKind::Int(p.text(tok).parse().unwrap_or(0)),
        TokenKind::Kw(Keyword::True) => ExprKind::Bool(true),
        TokenKind::Kw(Keyword::False) => ExprKind::Bool(false),
        TokenKind::Ident => ExprKind::Name(Ident::new(p.text(tok), range)),
        TokenKind::Str => {
            let text = p.text(tok);
            let inner = text.get(1..text.len().saturating_sub(1)).unwrap_or("");
            let parts: Vec<StrPart> = if inner.is_empty() {
                Vec::new()
            } else {
                vec![StrPart::Text(inner.into())]
            };
            ExprKind::Str(parts.into_boxed_slice())
        }
        TokenKind::StrHead => return interpolated(p),
        TokenKind::LParen => {
            p.bump();
            p.enter(tok)?;
            let inner = or_expr(p)?;
            if p.peek() != TokenKind::RParen {
                return Err(p.expected("`)`"));
            }
            p.bump();
            p.leave();
            let range = text_range(tok.span.start, p.prev_end());
            return p.add_expr(ExprKind::Paren(inner), range);
        }
        _ => return Err(p.expected("an expression")),
    };
    p.bump();
    p.add_expr(kind, range)
}

fn interpolated(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let head = p.bump();
    let mut parts = Vec::new();
    let text = p.text(head);
    let piece = text.get(1..text.len().saturating_sub(1)).unwrap_or("");
    if !piece.is_empty() {
        parts.push(StrPart::Text(piece.into()));
    }
    loop {
        parts.push(StrPart::Interp(or_expr(p)?));
        let tok = p.current();
        let text = p.text(tok);
        let piece = text.get(1..text.len().saturating_sub(1)).unwrap_or("");
        match tok.kind {
            TokenKind::StrMid | TokenKind::StrTail => {
                p.bump();
                if !piece.is_empty() {
                    parts.push(StrPart::Text(piece.into()));
                }
                if tok.kind == TokenKind::StrTail {
                    break;
                }
            }
            _ => return Err(p.expected("`}`")),
        }
    }
    let range = text_range(head.span.start, p.prev_end());
    p.add_expr(ExprKind::Str(parts.into_boxed_slice()), range)
}

// ----- helpers -----

/// Lexes and parses `src`; returns the module, the lexer and the parser
/// diagnostics.
fn run(src: &str, limits: ParseLimits) -> (Module, Vec<Diagnostic>, Vec<Diagnostic>) {
    let (tokens, lex_diags) = lex_with(src, F, &StubLiterals);
    let (module, diags) = parse_with(src, F, &tokens, &lex_diags, limits, &TestExprs);
    (module, lex_diags, diags)
}

/// Parses `src` that must lex cleanly; returns the parser diagnostics.
fn parse(src: &str) -> (Module, Vec<Diagnostic>) {
    let (module, lex_diags, diags) = run(src, ParseLimits::DEFAULT);
    assert!(lex_diags.is_empty(), "lexer diagnostics: {lex_diags:?}");
    (module, diags)
}

/// Code numbers and start offsets of the parser diagnostics.
fn codes_at(src: &str) -> Vec<(u16, u32)> {
    let (_, diags) = parse(src);
    diags
        .iter()
        .map(|d| (d.code.number(), d.span.start))
        .collect()
}

/// Byte offset of the `n`th occurrence (from 0) of `needle` in `src`.
fn at(src: &str, needle: &str, n: usize) -> u32 {
    let offset = src
        .match_indices(needle)
        .nth(n)
        .map_or(usize::MAX, |(i, _)| i);
    u32::try_from(offset).unwrap_or(u32::MAX)
}

/// Renders lexer and parser diagnostics like the CLI does for `path`.
fn rendered(path: &str, src: &str) -> String {
    let mut map = SourceMap::new();
    let Ok(file) = map.add(path, src) else {
        return String::from("<file not added>");
    };
    let (tokens, lex_diags) = lex_with(src, file, &StubLiterals);
    let (_, diags) = parse_with(
        src,
        file,
        &tokens,
        &lex_diags,
        ParseLimits::DEFAULT,
        &TestExprs,
    );
    lex_diags
        .iter()
        .chain(diags.iter())
        .map(|d| d.render(&map) + "\n")
        .collect()
}

fn fn_items(m: &Module) -> Vec<&FnDecl> {
    m.items()
        .iter()
        .map(|item| match item {
            Item::Fn(decl) => &**decl,
        })
        .collect()
}

fn block_stmts(m: &Module, id: BlockId) -> Vec<&StmtKind> {
    m.block(id)
        .map(|b| {
            b.stmts
                .iter()
                .filter_map(|s| m.stmt(*s))
                .map(|s| &s.kind)
                .collect()
        })
        .unwrap_or_default()
}

type R = Result<(), String>;

/// Turns a missing value into a test failure without panicking.
trait Must<T> {
    fn must(self) -> Result<T, String>;
}

impl<T> Must<T> for Option<T> {
    fn must(self) -> Result<T, String> {
        self.ok_or_else(|| String::from("missing value"))
    }
}

const MAIN: &str = "\nfn main()\n  print(1)\n";

// ----- structure -----

#[test]
fn parses_the_v0_1_example() -> R {
    let src = "fn fib(n: Int) -> Int\n  if n < 2\n    return n\n  return fib(n - 1) + fib(n - 2)\n\nfn main()\n  let x = fib(20)\n  print(\"fib(20) = {x}\")\n";
    let (m, diags) = parse(src);
    assert!(diags.is_empty(), "{diags:?}");
    let fns = fn_items(&m);
    assert_eq!(fns.len(), 2);
    let fib = fns.first().must()?;
    assert_eq!(&*fib.name.text, "fib");
    assert_eq!(fib.params.len(), 1);
    assert_eq!(&*fib.params.first().must()?.name.text, "n");
    assert_eq!(&*fib.params.first().must()?.ty.name.text, "Int");
    assert_eq!(fib.result.as_ref().map(|t| &*t.name.text), Some("Int"));
    let body = block_stmts(&m, fib.body);
    assert_eq!(body.len(), 2);
    assert!(matches!(
        body.first().must()?,
        StmtKind::If {
            else_block: None,
            ..
        }
    ));
    assert!(matches!(body.get(1).must()?, StmtKind::Return { .. }));
    let main = fns.get(1).must()?;
    assert!(main.params.is_empty());
    assert!(main.result.is_none());
    let main_body = block_stmts(&m, main.body);
    assert!(matches!(main_body.first().must()?, StmtKind::Let { name, .. } if &*name.text == "x"));
    assert!(matches!(main_body.get(1).must()?, StmtKind::Expr(_)));
    // module > fn > block > return > add > call > sub > name, as built by
    // hand in the AST tests.
    assert_eq!(m.height(), 8);
    Ok(())
}

#[test]
fn ranges_cover_the_source() -> R {
    let src = "fn main()\n  let total = 1 + 2\n  print(total)\n";
    let (m, diags) = parse(src);
    assert!(diags.is_empty(), "{diags:?}");
    let main = fn_items(&m).first().copied().must()?;
    assert_eq!(main.range.start(), 0);
    assert_eq!(main.name.range, text_range(3, 7));
    let block = m.block(main.body).map(|b| b.range);
    let let_start = at(src, "let", 0);
    let print_end = at(src, "print(total)", 0) + 12;
    assert_eq!(block, Some(text_range(let_start, print_end)));
    assert_eq!(main.range.end(), print_end);
    let Some(first) = m.block(main.body).and_then(|b| b.stmts.first().copied()) else {
        return Err("no statement".into());
    };
    let let_range = m.stmt(first).map(|s| s.range);
    assert_eq!(let_range, Some(text_range(let_start, let_start + 17)));
    Ok(())
}

#[test]
fn if_else_and_nested_blocks() -> R {
    let src = "fn main()\n  if true\n    if false\n      print(1)\n    else\n      print(2)\n  else\n    print(3)\n  print(4)\n";
    let (m, diags) = parse(src);
    assert!(diags.is_empty(), "{diags:?}");
    let body = block_stmts(&m, fn_items(&m).first().copied().must()?.body);
    assert_eq!(body.len(), 2);
    let StmtKind::If {
        then_block,
        else_block: Some(else_block),
        ..
    } = body.first().must()?
    else {
        return Err(format!("expected if with else, got {body:?}"));
    };
    assert!(matches!(
        block_stmts(&m, *then_block).first().must()?,
        StmtKind::If {
            else_block: Some(_),
            ..
        }
    ));
    assert_eq!(block_stmts(&m, *else_block).len(), 1);
    Ok(())
}

#[test]
fn params_results_comments_and_continued_lines() -> R {
    let src = "// header\nfn add(a: Int, b: Int) -> Int // sum\n  return add2(\n    a,\n    b\n  )\n\nfn main()\n  // only a comment\n  print(add(1, 2))\n";
    let (m, diags) = parse(src);
    assert!(diags.is_empty(), "{diags:?}");
    let fns = fn_items(&m);
    let names: Vec<&str> = fns
        .first()
        .must()?
        .params
        .iter()
        .map(|p| &*p.name.text)
        .collect();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(block_stmts(&m, fns.get(1).must()?.body).len(), 1);
    Ok(())
}

#[test]
fn empty_input_and_missing_eof() {
    let (m, diags) = parse("");
    assert!(m.items().is_empty() && diags.is_empty());
    let (m, diags) = parse("\n\n// nothing\n");
    assert!(m.items().is_empty() && diags.is_empty());
    // A token stream without EOF is closed at the end of the source.
    let src = "fn main()\n  print(1)\n";
    let (mut tokens, _) = lex_with(src, F, &StubLiterals);
    tokens.pop();
    let (m, diags) = parse_with(src, F, &tokens, &[], ParseLimits::DEFAULT, &TestExprs);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(m.items().len(), 1);
    let (m, diags) = parse_with(src, F, &[], &[], ParseLimits::DEFAULT, &TestExprs);
    assert!(m.items().is_empty() && diags.is_empty());
}

#[test]
fn last_line_without_line_end() -> R {
    let (m, diags) = parse("fn main()\n  print(1)");
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        block_stmts(&m, fn_items(&m).first().copied().must()?.body).len(),
        1
    );
    Ok(())
}

#[test]
fn reexports_the_limits() {
    assert_eq!(MAX_NODES, 1_000_000);
    assert_eq!(MAX_HEIGHT, 2_048);
    assert_eq!(MAX_DEPTH, 256);
    assert_eq!(ParseLimits::default().tree, Limits::DEFAULT);
}

// ----- error goldens of the parser (tests/errors) -----

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

golden!(parse_compound_assign);
golden!(parse_walrus_assign);
golden!(parse_is_operator);
golden!(parse_brace_block);
golden!(parse_colon_block_head);
golden!(parse_else_if_one_line);
golden!(parse_unclosed_paren);
golden!(parse_keyword_as_name);
// `parse_comparison_chain` (E0027) belongs to the expression parser (T1-3b).

macro_rules! sources {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../../../../tests/errors/", $name, ".ostl"))),)*]
    };
}

/// Goldens of later stages parse without a parser diagnostic, so the
/// checker sees each of them (README rule 8).
#[test]
fn checker_goldens_parse_cleanly() {
    let cases: &[(&str, &str)] = sources!(
        "check_float_type_v0_1",
        "entry_main_params",
        "entry_main_result",
        "entry_missing_main",
        "name_duplicate_fn",
        "name_duplicate_param",
        "name_unknown",
        "name_unknown_type",
        "type_arity",
        "type_condition_not_bool",
        "type_implicit_return_in_branch",
        "type_text_plus",
    );
    for (name, src) in cases {
        let (m, diags) = parse(src);
        assert!(diags.is_empty(), "{name}: {diags:?}");
        assert!(!m.items().is_empty(), "{name}");
    }
}

/// A file the lexer has reported gets no further diagnostic from the parser
/// (one fault, one diagnostic).
#[test]
fn lexer_goldens_add_no_parser_diagnostic() {
    let cases: &[(&str, &str)] = sources!(
        "lex_column_counts_scalars",
        "lex_escape_empty_unicode",
        "lex_escape_not_scalar",
        "lex_escape_surrogate",
        "lex_escape_unicode_too_long",
        "lex_int_literal_too_large",
        "lex_interpolation_too_deep",
        "lex_non_ascii_name",
        "lex_odd_indent",
        "lex_semicolon",
        "lex_string_in_interpolation",
        "lex_tab_indent",
        "lex_unexpected_char",
        "lex_unknown_escape",
        "lex_unterminated_string",
    );
    for (name, src) in cases {
        let (_, _, diags) = run(src, ParseLimits::DEFAULT);
        assert!(diags.is_empty(), "{name}: {diags:?}");
    }
}

#[test]
fn faulty_indentation_later_in_a_block_adds_nothing() {
    for src in [
        "fn main()\n  print(1)\n   print(2)\n  print(3)\n",
        "fn main()\n  print(1)\n\tprint(2)\n  print(3)\n",
    ] {
        let (_, lex_diags, diags) = run(src, ParseLimits::DEFAULT);
        assert_eq!(lex_diags.len(), 1, "{src:?}");
        assert!(diags.is_empty(), "{src:?}: {diags:?}");
    }
}

// ----- diagnostics and recovery -----

#[test]
fn one_diagnostic_per_faulty_statement() -> R {
    let src = "fn main()\n  let if = 1\n  x += 2\n  let y = 3\n  print(y) )\n  print(y)\n";
    assert_eq!(
        codes_at(src),
        vec![
            (26, at(src, "if", 0)),
            (20, at(src, "+=", 0)),
            (28, at(src, ")", 2)),
        ]
    );
    let (m, _) = parse(src);
    // The two good statements and the `let y` survive.
    assert_eq!(
        block_stmts(&m, fn_items(&m).first().copied().must()?.body).len(),
        2
    );
    Ok(())
}

#[test]
fn compound_operators_are_e0020() {
    for op in ["+=", "-=", "*=", "/=", "%=", ":="] {
        let src = format!("fn main()\n  x {op} 1\n");
        assert_eq!(codes_at(&src), vec![(20, 14)], "{op}");
    }
    // `= -1` and `==` are not compound operators.
    let src = "fn main()\n  print(1 == 1)\n  x = -1\n";
    assert_eq!(codes_at(src), vec![(100, at(src, "=", 2))]);
}

#[test]
fn recovery_at_item_boundaries() {
    let src = "fn (a: Int)\n  print(a)\n\nfn f(a Int)\n  print(a)\n\nfn g(a: Int,)\n  print(a)\n\nfn main()\n  print(1)\n";
    let (m, diags) = parse(src);
    let codes: Vec<(u16, u32)> = diags
        .iter()
        .map(|d| (d.code.number(), d.span.start))
        .collect();
    assert_eq!(
        codes,
        vec![
            (28, at(src, "(", 0)),
            (28, at(src, "Int", 1)),
            (28, at(src, ")", 4)),
        ]
    );
    let names: Vec<&str> = fn_items(&m).iter().map(|f| &*f.name.text).collect();
    assert_eq!(names, ["main"]);
}

#[test]
fn faults_inside_a_block_with_a_broken_head_are_still_reported() {
    let src = "fn main() x\n  let = 1\n  print(1)\n";
    assert_eq!(
        codes_at(src),
        vec![(28, at(src, "x", 0)), (28, at(src, "=", 0))]
    );
}

#[test]
fn messages_name_what_was_expected() -> R {
    let (_, diags) = parse("fn main(\n");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags.first().must()?.message, "unclosed `(`");
    let (_, diags) = parse("print(1)\n");
    assert_eq!(
        diags.first().must()?.message,
        "expected a declaration such as `fn main()`, found `print`"
    );
    let (_, diags) = parse("fn main()\n  return\n");
    assert_eq!(
        diags.first().must()?.message,
        "expected a value after `return`, found end of line"
    );
    let (_, diags) = parse("fn main(a: if)\n  print(1)\n");
    assert_eq!(
        diags.first().must()?.message,
        "expected a type name, found `if`"
    );
    let (_, diags) = parse("fn main(if: Int)\n  print(1)\n");
    assert_eq!(diags.first().must()?.code, codes::KEYWORD_AS_NAME);
    let (_, diags) = parse("fn if()\n  print(1)\n");
    assert_eq!(
        diags.first().must()?.message,
        "`if` is a keyword and cannot be used as a name"
    );
    Ok(())
}

#[test]
fn missing_block_is_reported_once() -> R {
    let src = "fn helper()\nfn main()\n  if true\n  print(1)\n";
    assert_eq!(
        codes_at(src),
        vec![(28, at(src, "fn", 1)), (28, at(src, "print", 0))]
    );
    let (_, diags) = parse("fn main()");
    assert_eq!(
        diags.first().must()?.message,
        "expected an indented block as the body of `main`, found end of file"
    );
    Ok(())
}

#[test]
fn unexpected_indentation() -> R {
    let src =
        "  fn main()\n    print(1)\nfn f()\n  print(1)\n    print(2)\n      print(3)\n  print(4)\n";
    assert_eq!(
        codes_at(src),
        vec![(32, 0), (32, at(src, "    print(2)", 0))]
    );
    let (m, _) = parse(src);
    assert_eq!(
        block_stmts(&m, fn_items(&m).first().copied().must()?.body).len(),
        2
    );
    Ok(())
}

#[test]
fn braces_report_once_and_the_closing_brace_is_skipped() -> R {
    let src = "fn main() {\n  if true {\n    print(1)\n  } else {\n    print(2)\n  }\n}\n";
    assert_eq!(
        codes_at(src),
        vec![
            (22, at(src, "{", 0)),
            (22, at(src, "{", 1)),
            (22, at(src, "{", 2))
        ]
    );
    let (m, _) = parse(src);
    let body = block_stmts(&m, fn_items(&m).first().copied().must()?.body);
    assert!(matches!(
        body.first().must()?,
        StmtKind::If {
            else_block: Some(_),
            ..
        }
    ));
    // Only as many `}` lines as reported `{` are skipped.
    let extra = format!("{src}}}\n");
    let found = codes_at(&extra);
    assert_eq!(found.last(), Some(&(28, at(&extra, "}", 3))));
    assert_eq!(found.len(), 4);
    Ok(())
}

#[test]
fn colon_heads_report_once() {
    let src = "fn main():\n  if true:\n    print(1)\n  else:\n    print(2)\n";
    assert_eq!(
        codes_at(src),
        vec![
            (23, at(src, ":", 0)),
            (23, at(src, ":", 1)),
            (23, at(src, ":", 2))
        ]
    );
}

#[test]
fn else_if_chain_builds_nested_ifs() -> R {
    let src =
        "fn main()\n  if true\n    print(1)\n  else if false\n    print(2)\n  else\n    print(3)\n";
    assert_eq!(codes_at(src), vec![(24, at(src, "if", 1))]);
    let (m, _) = parse(src);
    let body = block_stmts(&m, fn_items(&m).first().copied().must()?.body);
    let StmtKind::If {
        else_block: Some(else_block),
        ..
    } = body.first().must()?
    else {
        return Err("no else".into());
    };
    let inner = block_stmts(&m, *else_block);
    assert!(matches!(
        inner.first().must()?,
        StmtKind::If {
            else_block: Some(_),
            ..
        }
    ));
    Ok(())
}

#[test]
fn constructs_outside_the_slice_are_e0100_once() -> R {
    let cases = [
        (
            "data Message\n  text: Text\n",
            "data",
            "`data` declaration is not available in v0.1",
        ),
        (
            "server fn f()\n  print(1)\n",
            "server",
            "`server fn` is not available in v0.1",
        ),
        (
            "client fn f()\n  print(1)\n",
            "client",
            "`client fn` is not available in v0.1",
        ),
        (
            "fn f() as system\n  print(1)\n",
            "as",
            "`as system` is not available in v0.1",
        ),
        (
            "fn f(a: Int?)\n  print(1)\n",
            "Int",
            "an optional type is not available in v0.1",
        ),
        (
            "fn f(a: List[Int])\n  print(1)\n",
            "List",
            "a type with type arguments is not available in v0.1",
        ),
        (
            "fn f()\n  for x in xs\n    print(x)\n",
            "for",
            "`for` is not available in v0.1",
        ),
        (
            "fn f()\n  drop x\n",
            "drop",
            "`drop` is not available in v0.1",
        ),
        (
            "fn f()\n  if let y = x\n    print(y)\n",
            "let",
            "`if let` is not available in v0.1",
        ),
        (
            "fn f()\n  x = 1\n",
            "=",
            "assignment is not available in v0.1",
        ),
    ];
    for (head, word, message) in cases {
        let src = format!("{head}{MAIN}");
        let (m, diags) = parse(&src);
        assert_eq!(diags.len(), 1, "{head:?}: {diags:?}");
        assert_eq!(diags.first().must()?.code, codes::NOT_IN_V0_1, "{head:?}");
        assert_eq!(
            diags.first().must()?.span.start,
            at(&src, word, 0),
            "{head:?}"
        );
        assert_eq!(diags.first().must()?.message, message);
        assert!(
            fn_items(&m).iter().any(|f| &*f.name.text == "main"),
            "{head:?}"
        );
    }
    Ok(())
}

#[test]
fn lexer_error_tokens_get_no_parser_diagnostic() {
    for src in [
        "fn main()\n  let x = \"open\n  print(1)\n",
        "fn main(@)\n  print(1)\n",
        "fn main()\n  @\n    print(1)\n",
        "fn main()\n  if x @ 1\n    print(1)\n",
    ] {
        let (m, lex_diags, diags) = run(src, ParseLimits::DEFAULT);
        assert_eq!(lex_diags.len(), 1, "{src:?}");
        assert!(diags.is_empty(), "{src:?}: {diags:?}");
        assert!(m.node_count() >= 1);
    }
}

// ----- limits -----

/// `fn main()` with `levels` nested `if true` blocks around `let x = 1`.
fn nested_ifs(levels: usize) -> String {
    let mut src = String::from("fn main()\n");
    for level in 0..levels {
        src.push_str(&"  ".repeat(level + 1));
        src.push_str("if true\n");
    }
    src.push_str(&"  ".repeat(levels + 1));
    src.push_str("let x = 1\n");
    src
}

fn with_depth(max_depth: u32) -> ParseLimits {
    ParseLimits {
        max_depth,
        ..ParseLimits::DEFAULT
    }
}

#[test]
fn nesting_limit_is_exact_for_blocks() -> R {
    // The body is level 1, so 9 nested ifs reach level 10.
    let (_, _, diags) = run(&nested_ifs(9), with_depth(10));
    assert!(diags.is_empty(), "{diags:?}");
    let src = nested_ifs(10);
    let (_, _, diags) = run(&src, with_depth(10));
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags.first().must()?.code, codes::NESTING_TOO_DEEP);
    assert_eq!(
        diags.first().must()?.message,
        "brackets and blocks nest deeper than 10 levels"
    );
    // At the indentation of the eleventh level.
    assert_eq!(diags.first().must()?.span.start, at(&src, "let", 0) - 22);
    Ok(())
}

#[test]
fn nesting_limit_counts_brackets() -> R {
    let src = |n: usize| format!("fn main()\n  print({}1{})\n", "(".repeat(n), ")".repeat(n));
    // Body 1, call 2, then n parentheses.
    let (_, _, diags) = run(&src(8), with_depth(10));
    assert!(diags.is_empty(), "{diags:?}");
    let (_, _, diags) = run(&src(9), with_depth(10));
    assert_eq!(diags.len(), 1);
    assert_eq!(diags.first().must()?.code, codes::NESTING_TOO_DEEP);
    let (_, _, diags) = run("fn f(a: Int)\n  print(a)\n", with_depth(1));
    assert_eq!(diags.len(), 1);
    assert_eq!(diags.first().must()?.code, codes::NESTING_TOO_DEEP);
    Ok(())
}

#[test]
fn default_nesting_limit_is_256() -> R {
    let (_, _, diags) = run(&nested_ifs(255), ParseLimits::DEFAULT);
    assert!(diags.is_empty(), "{diags:?}");
    let (_, _, diags) = run(&nested_ifs(256), ParseLimits::DEFAULT);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags.first().must()?.code, codes::NESTING_TOO_DEEP);
    Ok(())
}

#[test]
fn long_else_if_chain_is_bounded() {
    let mut src = String::from("fn main()\n  if true\n    print(0)\n");
    for _ in 0..5_000 {
        src.push_str("  else if true\n    print(1)\n");
    }
    let (_, _, diags) = run(&src, ParseLimits::DEFAULT);
    let last = diags.last().map(|d| d.code);
    assert_eq!(last, Some(codes::NESTING_TOO_DEEP));
    assert!(diags.len() <= 257, "{}", diags.len());
}

#[test]
fn node_limit_is_exact() -> R {
    let src = "fn main()\n  let x = 1 + 2\n  print(x)\n";
    let (m, diags) = parse(src);
    assert!(diags.is_empty());
    let nodes = m.node_count();
    let limits = |max_nodes| ParseLimits {
        tree: Limits {
            max_nodes,
            max_height: MAX_HEIGHT,
        },
        ..ParseLimits::DEFAULT
    };
    let (m, _, diags) = run(src, limits(nodes));
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(m.items().len(), 1);
    let (_, _, diags) = run(src, limits(nodes - 1));
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags.first().must()?.code, codes::TOO_MANY_NODES);
    assert_eq!(
        diags.first().must()?.message,
        format!(
            "file is too large: it has more than {} syntax tree nodes",
            nodes - 1
        )
    );
    Ok(())
}

#[test]
fn node_limit_abandons_the_file() -> R {
    // Many faulty statements after the limit add no diagnostic.
    let mut src = String::from("fn main()\n  print(1 + 2 + 3)\n");
    for _ in 0..50 {
        src.push_str("  let if = 1\n");
    }
    let limits = ParseLimits {
        tree: Limits {
            max_nodes: 5,
            max_height: MAX_HEIGHT,
        },
        ..ParseLimits::DEFAULT
    };
    let (_, _, diags) = run(&src, limits);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags.first().must()?.code, codes::TOO_MANY_NODES);
    Ok(())
}

fn chain(operands: usize) -> String {
    let mut src = String::from("fn main()\n  let x = 1");
    for _ in 1..operands {
        src.push_str(" + 1");
    }
    src.push('\n');
    src
}

#[test]
fn height_limit_with_chains() -> R {
    // D54 hostile shapes: module > fn > block > let > chain.
    let (m, _, diags) = run(&chain(2_000), ParseLimits::DEFAULT);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(m.height(), 2_004);
    let (_, _, diags) = run(&chain(2_100), ParseLimits::DEFAULT);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags.first().must()?.code, codes::TREE_TOO_HIGH);
    // Exact: a chain of 2 044 operands has height 2 044, the tree 2 048.
    let (_, _, diags) = run(&chain(2_044), ParseLimits::DEFAULT);
    assert!(diags.is_empty(), "{diags:?}");
    let (_, _, diags) = run(&chain(2_045), ParseLimits::DEFAULT);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags.first().must()?.code, codes::TREE_TOO_HIGH);
    Ok(())
}

/// Lexes and parses `src` and returns the work counter of the parser.
fn steps_for(src: &str) -> u64 {
    let (tokens, lex_diags) = lex_with(src, F, &StubLiterals);
    let mut p = Parser::new(
        src,
        F,
        &tokens,
        &lex_diags,
        ParseLimits::DEFAULT,
        &TestExprs,
    );
    let _ = p.program();
    assert!(p.diags.is_empty(), "{:?}", p.diags);
    p.steps()
}

/// `lines` statements of the form `let xN = N + 1` in one function.
fn many_lets(lines: usize) -> String {
    let mut src = String::from("fn main()\n");
    for i in 0..lines {
        src.push_str(&format!("  let x{i} = {i} + 1\n"));
    }
    src
}

#[test]
fn many_statements_are_linear() -> R {
    // D74: no wall clock in the gate. The work counter of the parser must
    // grow linearly with the input: doubling the statements at most doubles
    // the work (plus a constant), and the work per token is bounded.
    const LINES: usize = 20_000;
    let (m, _, diags) = run(&many_lets(LINES), ParseLimits::DEFAULT);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        block_stmts(&m, fn_items(&m).first().copied().must()?.body).len(),
        LINES
    );
    let small = steps_for(&many_lets(LINES));
    let large = steps_for(&many_lets(2 * LINES));
    assert!(large <= 2 * small + 64, "{small} steps, then {large}");
    let tokens = lex_with(&many_lets(LINES), F, &StubLiterals).0.len();
    let per_token = small / u64::try_from(tokens).map_err(|e| e.to_string())?;
    assert!(per_token <= 16, "{per_token} steps per token");
    Ok(())
}

#[test]
fn hostile_lines_are_linear() -> R {
    // Long lines with many `(`, `+=` candidates and lexer faults: the work
    // per token stays bounded.
    let line = |n: usize| {
        let mut src = String::from("fn main()\n");
        for _ in 0..n {
            src.push_str("  print(((1 + 2) * 3) - 4, 5, 6 + 7 + 8 + 9)\n");
        }
        src
    };
    let small = steps_for(&line(5_000));
    let large = steps_for(&line(10_000));
    assert!(large <= 2 * small + 64, "{small} steps, then {large}");
    Ok(())
}

#[test]
fn lexer_fault_does_not_hide_the_next_line() -> R {
    // Review T9 @33d2618: the end of the `NL` of a reported line is the start
    // of the next line; a fault in column 1 of that line is its own fault.
    let src = "fn main()\n  print(1) @\nlet x = 1\n";
    let (_, lex_diags, diags) = run(src, ParseLimits::DEFAULT);
    assert_eq!(lex_diags.len(), 1, "{lex_diags:?}");
    assert_eq!(diags.len(), 1, "{diags:?}");
    let d = diags.first().must()?;
    assert_eq!((d.code, d.span.start), (codes::EXPECTED, at(src, "let", 0)));
    assert!(d.message.contains("found `let`"), "{}", d.message);
    // The same with the faulty character in column 1 of line 2.
    let src = "fn main()\n@\nlet x = 1\n";
    let (_, lex_diags, diags) = run(src, ParseLimits::DEFAULT);
    assert_eq!(lex_diags.len(), 1, "{lex_diags:?}");
    assert_eq!(diags.len(), 1, "{diags:?}");
    let d = diags.first().must()?;
    assert_eq!((d.code, d.span.start), (codes::EXPECTED, at(src, "let", 0)));
    // The faulty line itself still gets no second diagnostic.
    let src = "fn main()\n  print(1) @ (\n  print(2)\n";
    let (_, lex_diags, diags) = run(src, ParseLimits::DEFAULT);
    assert_eq!(lex_diags.len(), 1, "{lex_diags:?}");
    assert!(diags.is_empty(), "{diags:?}");
    Ok(())
}

#[test]
fn suppression_follows_lexer_diagnostic_spans() -> R {
    // Which lines are reported is taken from the lexer diagnostics, not from
    // the tokens: a diagnostic on a clean line suppresses that line only.
    let src = "fn main()\n  let = 1\n  let = 2\n";
    let (tokens, lex_diags) = lex_with(src, F, &StubLiterals);
    assert!(lex_diags.is_empty(), "{lex_diags:?}");
    let parse_lexed = |lexed: &[Diagnostic]| {
        let (_, diags) = parse_with(src, F, &tokens, lexed, ParseLimits::DEFAULT, &TestExprs);
        diags.iter().map(|d| d.span.start).collect::<Vec<_>>()
    };
    let first = at(src, "=", 0);
    let second = at(src, "=", 1);
    assert_eq!(parse_lexed(&[]), vec![first, second]);
    let on = |offset: u32| Diagnostic::error(Code::new(1), Span::point(F, offset), "lexer");
    // Anywhere on line 2, including its indentation and its line end.
    for offset in [at(src, "  let", 0), first, at(src, "1\n", 0) + 1] {
        assert_eq!(parse_lexed(&[on(offset)]), vec![second], "at {offset}");
    }
    // The start of line 3 belongs to line 3, not to the end of line 2.
    assert_eq!(parse_lexed(&[on(at(src, "  let", 1))]), vec![first]);
    // A comment line is no part of the next line: a fault in it does not hide
    // a fault below it.
    let src = "fn main()\n  // note\n  let = 1\n";
    let (tokens, _) = lex_with(src, F, &StubLiterals);
    let comment = Diagnostic::error(Code::new(1), Span::point(F, at(src, "note", 0)), "lexer");
    let (_, diags) = parse_with(
        src,
        F,
        &tokens,
        &[comment],
        ParseLimits::DEFAULT,
        &TestExprs,
    );
    assert_eq!(diags.len(), 1, "{diags:?}");
    // A diagnostic at the very end of the source belongs to the last line.
    let src = "fn main()\n  let = 1";
    let (tokens, _) = lex_with(src, F, &StubLiterals);
    let end = u32::try_from(src.len()).map_err(|e| e.to_string())?;
    let (_, diags) = parse_with(
        src,
        F,
        &tokens,
        &[on(end)],
        ParseLimits::DEFAULT,
        &TestExprs,
    );
    assert!(diags.is_empty(), "{diags:?}");
    Ok(())
}

#[test]
fn compound_assignment_after_let() -> R {
    // Review T9 F2: `let x += 1` and `let x := 1` are E0020, not E0028.
    for (src, op) in [
        ("fn main()\n  let x += 1\n", "+"),
        ("fn main()\n  let x := 1\n", ":"),
    ] {
        assert_eq!(
            codes_at(src),
            vec![(codes::ASSIGN_OPERATOR.number(), at(src, op, 0))],
            "{src:?}"
        );
    }
    Ok(())
}

#[test]
fn unclosed_paren_swallowing_the_file_is_one_diagnostic() -> R {
    let mut src = String::from("fn main()\n  print(1\n");
    for _ in 0..1_000 {
        src.push_str("  let if = (\n");
    }
    let (_, diags) = parse(&src);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags.first().must()?.code, codes::UNCLOSED_PAREN);
    Ok(())
}
