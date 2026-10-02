//! Expression parser for the v0.1 slice (WP T1-3b, D43, SYNTAX 7).
//!
//! [`Exprs`] implements [`ExprParse`], the seam to the statement parser in
//! `mod.rs`. The contract is documented there.
//!
//! # Grammar read here
//!
//! ```text
//! expr    = orExpr ;
//! orExpr  = andExpr { "or" andExpr } ;
//! andExpr = notExpr { "and" notExpr } ;
//! notExpr = "not" notExpr | cmpExpr ;
//! cmpExpr = addExpr [ cmpOp addExpr ] ;
//! addExpr = mulExpr { ( "+" | "-" ) mulExpr } ;
//! mulExpr = unary { ( "*" | "/" | "%" ) unary } ;
//! unary   = [ "-" ] postfix ;
//! postfix = primary { "(" [ expr { "," expr } ] ")" } ;
//! primary = Number | String | "true" | "false" | name | "(" expr ")" ;
//! ```
//!
//! # How operator chains are read (D43)
//!
//! The levels from `orExpr` down to `mulExpr` are not one function each.
//! [`expr`] reads them by precedence climbing with its own stack of
//! [`Pending`] operators: it reads an operand, then looks at the next binary
//! operator and first combines every pending operator that binds at least as
//! tightly. Prefix `not` waits on the same stack. So a chain such as
//! `1 + 1 + ... + 1` or `not not ... x` costs heap memory, never native stack,
//! however long it is. The AST height limit (D54) then ends a chain that is
//! too high for the later recursive passes.
//!
//! Native recursion happens only through brackets: a call's arguments, a
//! parenthesized expression and the expression of a string interpolation.
//! Calls and parentheses pass [`Parser::enter`], so their depth is bounded by
//! the nesting limit; an interpolation cannot contain another string (G1), so
//! it adds at most one level around them.
//!
//! # Constructs of later milestones
//!
//! The AST of v0.1 has no node for `none`, `me`, `now`, `signed`, `make`,
//! set, list and map literals, `.name`, `[expr]`, `..`, `??`, `catch`, `in`
//! and `not in`. Each of them is reported once with `E0100 not available in
//! v0.1` and the line is skipped, like the statement parser does for `for`.

use super::{ExprParse, ParseLimits, Parser, Stop, codes, text_range};
use crate::ast::{BinaryOp, ExprId, ExprKind, Ident, Module, StrPart, TextRange, UnaryOp};
use crate::lex::{Keyword, Token, TokenKind};
use ostrel_core::{Diagnostic, FileId};

/// Parses the tokens of one file with the default limits and this
/// expression parser.
///
/// `src` is the source text the tokens were lexed from, `file` its id for
/// diagnostics, and `lexed` the diagnostics the lexer reported for these
/// tokens. See [`super::parse_with`] for the details.
pub fn parse(
    src: &str,
    file: FileId,
    tokens: &[Token],
    lexed: &[Diagnostic],
) -> (Module, Vec<Diagnostic>) {
    super::parse_with(src, file, tokens, lexed, ParseLimits::DEFAULT, &Exprs)
}

/// The expression parser of the v0.1 slice.
#[derive(Clone, Copy, Debug, Default)]
pub struct Exprs;

impl ExprParse for Exprs {
    fn expr(&self, p: &mut Parser<'_>) -> Result<ExprId, Stop> {
        expr(p)
    }
}

/// Binding strength of the operator levels, from loosest to tightest
/// (`docs/grammar.md` section 4). Unary `-` and calls bind tighter than all
/// of them and are read with the operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Or,
    And,
    Not,
    Cmp,
    Add,
    Mul,
}

/// An operator on the stack of [`expr`], still waiting for its right
/// operand.
#[derive(Clone, Copy, Debug)]
enum Pending {
    /// A prefix `not` that starts at byte offset `start`.
    Not { start: u32 },
    /// A binary operator and its complete left operand.
    Binary {
        op: BinaryOp,
        prec: Prec,
        lhs: ExprId,
    },
}

impl Pending {
    fn prec(self) -> Prec {
        match self {
            Pending::Not { .. } => Prec::Not,
            Pending::Binary { prec, .. } => prec,
        }
    }
}

/// The binary operator that `kind` spells, if any.
fn binary_op(kind: TokenKind) -> Option<(BinaryOp, Prec)> {
    let op = match kind {
        TokenKind::Kw(Keyword::Or) => (BinaryOp::Or, Prec::Or),
        TokenKind::Kw(Keyword::And) => (BinaryOp::And, Prec::And),
        TokenKind::EqEq => (BinaryOp::Eq, Prec::Cmp),
        TokenKind::BangEq => (BinaryOp::Ne, Prec::Cmp),
        TokenKind::Lt => (BinaryOp::Lt, Prec::Cmp),
        TokenKind::Le => (BinaryOp::Le, Prec::Cmp),
        TokenKind::Gt => (BinaryOp::Gt, Prec::Cmp),
        TokenKind::Ge => (BinaryOp::Ge, Prec::Cmp),
        TokenKind::Plus => (BinaryOp::Add, Prec::Add),
        TokenKind::Minus => (BinaryOp::Sub, Prec::Add),
        TokenKind::Star => (BinaryOp::Mul, Prec::Mul),
        TokenKind::Slash => (BinaryOp::Div, Prec::Mul),
        TokenKind::Percent => (BinaryOp::Rem, Prec::Mul),
        _ => return None,
    };
    Some(op)
}

/// Reads one expression (`orExpr`) by precedence climbing.
fn expr(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let mut pending: Vec<Pending> = Vec::new();
    loop {
        // `notExpr` may start only where `and`, `or` or another `not` waits
        // for its operand, or at the start of the expression.
        while p.peek() == TokenKind::Kw(Keyword::Not) {
            if pending.last().is_some_and(|top| top.prec() > Prec::Not) {
                return Err(p.expected(
                    "an operand (`not` binds more loosely than comparisons and \
                     arithmetic; put `not ...` in parentheses)",
                ));
            }
            let not = p.bump();
            pending.push(Pending::Not {
                start: not.span.start,
            });
        }

        let mut operand = operand(p)?;
        reject_later_operator(p)?;
        let next = binary_op(p.peek());

        // Combine every pending operator that binds at least as tightly as
        // the next one; all of them at the end of the expression. This makes
        // the binary levels left associative.
        while let Some(&top) = pending.last() {
            if let Some((_, next_prec)) = next {
                if top.prec() < next_prec {
                    break;
                }
                if top.prec() == Prec::Cmp && next_prec == Prec::Cmp {
                    let at = p.current();
                    return Err(p.report(codes::COMPARISON_CHAIN, at, codes::MSG_COMPARISON_CHAIN));
                }
            }
            pending.pop();
            operand = combine(p, top, operand)?;
        }

        let Some((op, prec)) = next else {
            return Ok(operand);
        };
        p.bump();
        pending.push(Pending::Binary {
            op,
            prec,
            lhs: operand,
        });
    }
}

/// Builds the node of a pending operator with its now complete right
/// operand.
fn combine(p: &mut Parser<'_>, pending: Pending, rhs: ExprId) -> Result<ExprId, Stop> {
    match pending {
        Pending::Not { start } => {
            let range = text_range(start, range_of(p, rhs).end());
            let kind = ExprKind::Unary {
                op: UnaryOp::Not,
                operand: rhs,
            };
            p.add_expr(kind, range)
        }
        Pending::Binary { op, lhs, .. } => {
            let range = range_of(p, lhs).cover(range_of(p, rhs));
            p.add_expr(ExprKind::Binary { op, lhs, rhs }, range)
        }
    }
}

/// Reports E0100 for a binary operator of a later milestone after an
/// operand: `..`, `??`, `catch`, `in` and `not in`.
fn reject_later_operator(p: &mut Parser<'_>) -> Result<(), Stop> {
    let what = match p.peek() {
        TokenKind::DotDot => "the range operator `..`",
        TokenKind::QuestionQuestion => "the fallback operator `??`",
        TokenKind::Kw(Keyword::Catch) => "`catch`",
        TokenKind::Kw(Keyword::In) => "the operator `in`",
        TokenKind::Kw(Keyword::Not) if p.nth(1) == TokenKind::Kw(Keyword::In) => {
            "the operator `not in`"
        }
        _ => return Ok(()),
    };
    Err(not_in_v0_1(p, what))
}

/// `unary = [ "-" ] postfix`. The minus applies once (`docs/grammar.md` 3).
fn operand(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    if p.peek() != TokenKind::Minus {
        return postfix(p);
    }
    let minus = p.bump();
    if p.peek() == TokenKind::Minus {
        return Err(p.expected("an operand after `-` (write `-(-x)` for a double negation)"));
    }
    let operand = postfix(p)?;
    let range = text_range(minus.span.start, range_of(p, operand).end());
    let kind = ExprKind::Unary {
        op: UnaryOp::Neg,
        operand,
    };
    p.add_expr(kind, range)
}

/// `postfix = primary { "(" args ")" }`.
fn postfix(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let mut value = primary(p)?;
    loop {
        match p.peek() {
            TokenKind::LParen => value = call(p, value)?,
            TokenKind::Dot => return Err(not_in_v0_1(p, "field access with `.`")),
            TokenKind::LBracket => return Err(not_in_v0_1(p, "indexing with `[...]`")),
            _ => return Ok(value),
        }
    }
}

/// A call of `callee`; the current token is its `(`.
fn call(p: &mut Parser<'_>, callee: ExprId) -> Result<ExprId, Stop> {
    let open = p.bump();
    p.enter(open)?;
    let args = call_args(p);
    p.leave();
    let args = args?;
    let range = range_of(p, callee).cover(text_range(open.span.start, p.prev_end()));
    let kind = ExprKind::Call {
        callee,
        args: args.into_boxed_slice(),
    };
    p.add_expr(kind, range)
}

/// The arguments of a call after its `(`, up to and including the `)`.
fn call_args(p: &mut Parser<'_>) -> Result<Vec<ExprId>, Stop> {
    let mut args = Vec::new();
    if p.peek() == TokenKind::RParen {
        p.bump();
        return Ok(args);
    }
    loop {
        args.push(expr(p)?);
        match p.peek() {
            TokenKind::Comma => {
                p.bump();
            }
            TokenKind::RParen => {
                p.bump();
                return Ok(args);
            }
            _ => return Err(p.expected("`,` or `)`")),
        }
    }
}

/// `primary = Number | String | "true" | "false" | name | "(" expr ")"`.
fn primary(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let tok = p.current();
    let range = text_range(tok.span.start, tok.span.end);
    let kind = match tok.kind {
        TokenKind::Int => ExprKind::Int(int_value(p.text(tok))),
        TokenKind::Kw(Keyword::True) => ExprKind::Bool(true),
        TokenKind::Kw(Keyword::False) => ExprKind::Bool(false),
        TokenKind::Ident => ExprKind::Name(Ident::new(p.text(tok), range)),
        TokenKind::Str => {
            let mut parts = Vec::new();
            push_text(&mut parts, p.text(tok));
            ExprKind::Str(parts.into_boxed_slice())
        }
        TokenKind::StrHead => return interpolated(p),
        TokenKind::LParen => return parenthesized(p),
        TokenKind::LBrace => return Err(not_in_v0_1(p, "a set literal")),
        TokenKind::LBracket => return Err(not_in_v0_1(p, "a list or map literal")),
        TokenKind::Kw(
            kw @ (Keyword::None | Keyword::Me | Keyword::Now | Keyword::Signed | Keyword::Make),
        ) => return Err(not_in_v0_1(p, &format!("`{}`", kw.as_str()))),
        TokenKind::Kw(kw) if !is_operator_word(kw) => {
            let msg = codes::msg_keyword_as_name(kw);
            return Err(p.report(codes::KEYWORD_AS_NAME, tok, msg));
        }
        _ => return Err(p.expected("an expression")),
    };
    p.bump();
    p.add_expr(kind, range)
}

/// Keywords that are operators. Where an operand is expected they are a
/// misplaced operator, not a keyword used as a name.
fn is_operator_word(kw: Keyword) -> bool {
    matches!(
        kw,
        Keyword::And | Keyword::Or | Keyword::Not | Keyword::In | Keyword::Catch
    )
}

/// `"(" expr ")"`; the current token is the `(`.
fn parenthesized(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let open = p.bump();
    p.enter(open)?;
    let inner = expr(p).and_then(|inner| {
        if p.peek() == TokenKind::RParen {
            p.bump();
            Ok(inner)
        } else {
            Err(p.expected("`)`"))
        }
    });
    p.leave();
    let inner = inner?;
    let range = text_range(open.span.start, p.prev_end());
    p.add_expr(ExprKind::Paren(inner), range)
}

/// A string with interpolations: `StrHead expr { StrMid expr } StrTail`.
fn interpolated(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let head = p.bump();
    let mut parts = Vec::new();
    push_text(&mut parts, p.text(head));
    loop {
        parts.push(StrPart::Interp(expr(p)?));
        let tok = p.current();
        match tok.kind {
            TokenKind::StrMid | TokenKind::StrTail => {
                p.bump();
                push_text(&mut parts, p.text(tok));
                if tok.kind == TokenKind::StrTail {
                    break;
                }
            }
            _ => return Err(p.expected("`}` to close the interpolation")),
        }
    }
    let range = text_range(head.span.start, p.prev_end());
    p.add_expr(ExprKind::Str(parts.into_boxed_slice()), range)
}

/// Adds the text of a string token to `parts`, without its delimiters and
/// with escapes resolved. Empty text adds nothing.
///
/// Every string token starts and ends with a one byte delimiter: `"` or `}`
/// in front, `"` or `{` at the end (see [`TokenKind`]).
fn push_text(parts: &mut Vec<StrPart>, token_text: &str) {
    let inner = token_text
        .get(1..token_text.len().saturating_sub(1))
        .unwrap_or("");
    if !inner.is_empty() {
        parts.push(StrPart::Text(unescape(inner).into()));
    }
}

/// Resolves the escapes of string text (`docs/grammar.md` 2.6).
///
/// A malformed escape has already been reported by the lexer (E0004,
/// E0005); it is kept as written, so this never fails.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(backslash) = rest.find('\\') {
        out.push_str(rest.get(..backslash).unwrap_or(""));
        let after = rest.get(backslash + 1..).unwrap_or("");
        match escape(after) {
            Some((c, len)) => {
                out.push(c);
                rest = after.get(len..).unwrap_or("");
            }
            None => {
                out.push('\\');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The character of the escape that `after` (the text behind a `\`) starts
/// with, and the bytes it takes. `None` for a malformed escape.
fn escape(after: &str) -> Option<(char, usize)> {
    let c = match after.chars().next()? {
        '{' => '{',
        '}' => '}',
        '"' => '"',
        '\\' => '\\',
        'n' => '\n',
        't' => '\t',
        'u' => return unicode_escape(after),
        _ => return None,
    };
    Some((c, 1))
}

/// `u{h}` with 1 to 6 hex digits that name a Unicode scalar value.
fn unicode_escape(after: &str) -> Option<(char, usize)> {
    let digits = after.strip_prefix("u{")?;
    // At most 7 digits are looked at, so a long run of hex digits after
    // `\u{` costs constant time.
    let len = digits
        .bytes()
        .take(7)
        .take_while(u8::is_ascii_hexdigit)
        .count();
    if !(1..=6).contains(&len) || digits.as_bytes().get(len) != Some(&b'}') {
        return None;
    }
    let value = u32::from_str_radix(digits.get(..len)?, 16).ok()?;
    let c = char::from_u32(value)?;
    // `u`, `{`, the digits and `}`.
    Some((c, len + 3))
}

/// The value of an integer literal.
///
/// The lexer has reported a literal outside the `Int` range (E0010). Such a
/// literal still reaches the parser; its value saturates and is never used,
/// because a file with a diagnostic is not run.
fn int_value(text: &str) -> i64 {
    text.bytes()
        .filter(u8::is_ascii_digit)
        .fold(0, |value: i64, digit| {
            value
                .saturating_mul(10)
                .saturating_add(i64::from(digit - b'0'))
        })
}

/// The source range of an expression that the parser has built.
fn range_of(p: &Parser<'_>, id: ExprId) -> TextRange {
    p.module()
        .expr(id)
        .map_or(TextRange::default(), |e| e.range)
}

/// Reports E0100 for `what` at the current token.
fn not_in_v0_1(p: &mut Parser<'_>, what: &str) -> Stop {
    let at = p.current();
    p.report(codes::NOT_IN_V0_1, at, codes::msg_not_in_v0_1(what))
}

#[cfg(test)]
mod tests {
    //! Unit tests of the expression parser. Sources go through the real
    //! lexer driver and the real statement parser; expressions are written
    //! as `let x = <expr>` in `fn main()` and read back as S-expressions.

    use super::super::MAX_DEPTH;
    use super::*;
    use crate::ast::{Item, Limits, StmtKind};
    use crate::lex::lex;
    use crate::{MAX_HEIGHT, MAX_NODES};

    const F: FileId = FileId::from_raw(0);

    // ----- helpers -----

    fn parse_src(src: &str, limits: ParseLimits) -> (Module, Vec<Diagnostic>, Vec<Diagnostic>) {
        let (tokens, lex_diags) = lex(src, F);
        let (module, diags) = super::super::parse_with(src, F, &tokens, &lex_diags, limits, &Exprs);
        (module, lex_diags, diags)
    }

    /// `fn main()` with one line `let x = <expr>`.
    fn wrap(expr: &str) -> String {
        format!("fn main()\n  let x = {expr}\n")
    }

    /// The values of all `let` statements of all functions, in order.
    fn let_values(m: &Module) -> Vec<ExprId> {
        let mut values = Vec::new();
        for item in m.items() {
            let Item::Fn(decl) = item;
            let stmts = m.block(decl.body).map_or(&[][..], |b| &b.stmts[..]);
            for stmt in stmts.iter().filter_map(|s| m.stmt(*s)) {
                if let StmtKind::Let { value, .. } = &stmt.kind {
                    values.push(*value);
                }
            }
        }
        values
    }

    /// An expression as an S-expression, for example `(+ 1 (* 2 3))`.
    fn sexpr(m: &Module, id: ExprId) -> String {
        let Some(e) = m.expr(id) else {
            return "<missing>".into();
        };
        match &e.kind {
            ExprKind::Int(v) => v.to_string(),
            ExprKind::Bool(b) => b.to_string(),
            ExprKind::Name(name) => name.text.to_string(),
            ExprKind::Str(parts) => {
                let parts: Vec<String> = parts
                    .iter()
                    .map(|part| match part {
                        StrPart::Text(t) => format!("{t:?}"),
                        StrPart::Interp(i) => format!("{{{}}}", sexpr(m, *i)),
                    })
                    .collect();
                format!("(str {})", parts.join(" "))
            }
            ExprKind::Unary { op, operand } => {
                let op = match op {
                    UnaryOp::Neg => "neg",
                    UnaryOp::Not => "not",
                };
                format!("({op} {})", sexpr(m, *operand))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                format!("({} {} {})", op.as_str(), sexpr(m, *lhs), sexpr(m, *rhs))
            }
            ExprKind::Call { callee, args } => {
                let mut out = format!("(call {}", sexpr(m, *callee));
                for arg in args.iter() {
                    out.push(' ');
                    out.push_str(&sexpr(m, *arg));
                }
                out + ")"
            }
            ExprKind::Paren(inner) => format!("(paren {})", sexpr(m, *inner)),
        }
    }

    /// Parses `let x = <expr>` that must have no diagnostic; returns the
    /// S-expression of the value.
    fn tree(expr: &str) -> String {
        let (m, lex_diags, diags) = parse_src(&wrap(expr), ParseLimits::DEFAULT);
        assert!(
            lex_diags.is_empty(),
            "lexer diagnostics for {expr}: {lex_diags:?}"
        );
        assert!(diags.is_empty(), "parser diagnostics for {expr}: {diags:?}");
        let values = let_values(&m);
        assert_eq!(values.len(), 1, "one let value for {expr}");
        values.first().map_or(String::new(), |v| sexpr(&m, *v))
    }

    /// Parses `src` that must lex cleanly; returns code number, start offset
    /// and message of every parser diagnostic.
    fn diags_of(src: &str, limits: ParseLimits) -> Vec<(u16, u32, String)> {
        let (_, lex_diags, diags) = parse_src(src, limits);
        assert!(lex_diags.is_empty(), "lexer diagnostics: {lex_diags:?}");
        diags
            .into_iter()
            .map(|d| (d.code.number(), d.span.start, d.message))
            .collect()
    }

    /// The single diagnostic of `let x = <expr>`: code, column of the
    /// diagnostic inside `expr`, message.
    fn one_error(expr: &str) -> (u16, u32, String) {
        let src = wrap(expr);
        let diags = diags_of(&src, ParseLimits::DEFAULT);
        assert_eq!(
            diags.len(),
            1,
            "exactly one diagnostic for {expr}: {diags:?}"
        );
        let prefix = u32::try_from(src.find(expr).unwrap_or(0)).unwrap_or(0);
        diags
            .into_iter()
            .next()
            .map_or((0, 0, String::new()), |(code, at, msg)| {
                (code, at.saturating_sub(prefix), msg)
            })
    }

    fn limits(max_depth: u32, max_nodes: u32, max_height: u32) -> ParseLimits {
        ParseLimits {
            max_depth,
            tree: Limits {
                max_nodes,
                max_height,
            },
        }
    }

    /// `1 + 1 + ... + 1` with `n` operands.
    fn chain(n: usize) -> String {
        vec!["1"; n].join(" + ")
    }

    // ----- precedence and associativity -----

    #[test]
    fn precedence_follows_the_operator_table() {
        assert_eq!(tree("1 + 2 * 3"), "(+ 1 (* 2 3))");
        assert_eq!(tree("1 * 2 + 3"), "(+ (* 1 2) 3)");
        assert_eq!(tree("a + b == c * d"), "(== (+ a b) (* c d))");
        assert_eq!(tree("a < b and c >= d"), "(and (< a b) (>= c d))");
        assert_eq!(tree("a or b and c"), "(or a (and b c))");
        assert_eq!(tree("a and b or c"), "(or (and a b) c)");
        assert_eq!(tree("-a * b"), "(* (neg a) b)");
        assert_eq!(tree("-f(x) % 2"), "(% (neg (call f x)) 2)");
    }

    #[test]
    fn binary_levels_are_left_associative() {
        assert_eq!(tree("1 - 2 - 3"), "(- (- 1 2) 3)");
        assert_eq!(tree("8 / 4 / 2"), "(/ (/ 8 4) 2)");
        assert_eq!(tree("a % b * c / d"), "(/ (* (% a b) c) d)");
        assert_eq!(tree("a or b or c"), "(or (or a b) c)");
        assert_eq!(tree("a and b and c"), "(and (and a b) c)");
    }

    #[test]
    fn not_binds_more_loosely_than_comparison_and_more_tightly_than_and() {
        assert_eq!(tree("not a == b"), "(not (== a b))");
        assert_eq!(tree("not a and b"), "(and (not a) b)");
        assert_eq!(tree("a or not b"), "(or a (not b))");
        assert_eq!(tree("not not a"), "(not (not a))");
        assert_eq!(tree("not a + 1 < b or c"), "(or (not (< (+ a 1) b)) c)");
    }

    #[test]
    fn parentheses_and_calls() {
        assert_eq!(tree("(1 + 2) * 3"), "(* (paren (+ 1 2)) 3)");
        assert_eq!(tree("f()"), "(call f)");
        assert_eq!(tree("f(1, g(2, 3))"), "(call f 1 (call g 2 3))");
        assert_eq!(tree("f(a)(b)"), "(call (call f a) b)");
        assert_eq!(tree("-(-x)"), "(neg (paren (neg x)))");
        assert_eq!(tree("(a < b) == c"), "(== (paren (< a b)) c)");
    }

    #[test]
    fn call_arguments_continue_over_line_ends() {
        let src = "fn main()\n  let x = f(1,\n    2)\n";
        let (m, lex_diags, diags) = parse_src(src, ParseLimits::DEFAULT);
        assert!(
            lex_diags.is_empty() && diags.is_empty(),
            "{lex_diags:?} {diags:?}"
        );
        let values = let_values(&m);
        let text: Vec<String> = values.iter().map(|v| sexpr(&m, *v)).collect();
        assert_eq!(text, ["(call f 1 2)"]);
    }

    // ----- literals -----

    #[test]
    fn literals() {
        assert_eq!(tree("0"), "0");
        assert_eq!(tree("9007199254740991"), "9007199254740991");
        assert_eq!(tree("true"), "true");
        assert_eq!(tree("false"), "false");
        assert_eq!(tree("someName"), "someName");
        assert_eq!(tree("\"\""), "(str )");
        assert_eq!(tree("\"hello\""), "(str \"hello\")");
    }

    #[test]
    fn string_escapes_are_resolved() {
        assert_eq!(tree(r#""a\n\t\{\}\"\\b""#), "(str \"a\\n\\t{}\\\"\\\\b\")");
        assert_eq!(tree(r#""\u{41}\u{1F600}\u{0}""#), "(str \"A\u{1F600}\\0\")");
    }

    #[test]
    fn malformed_escapes_are_kept_as_written() {
        // The lexer reports these; the parser must neither fail nor guess.
        assert_eq!(unescape(r"\q"), r"\q");
        assert_eq!(unescape(r"\u{}"), r"\u{}");
        assert_eq!(unescape(r"\u{1234567}"), r"\u{1234567}");
        assert_eq!(unescape(r"\u{D800}"), r"\u{D800}");
        assert_eq!(unescape(r"\u{110000}"), r"\u{110000}");
        assert_eq!(unescape(r"\u{41"), r"\u{41");
        assert_eq!(unescape("end\\"), "end\\");
        assert_eq!(unescape(r"\\\q"), r"\\q");
    }

    #[test]
    fn interpolation_splits_text_and_expressions() {
        assert_eq!(
            tree(r#""x = {x}, next = {x + 1}!""#),
            "(str \"x = \" {x} \", next = \" {(+ x 1)} \"!\")"
        );
        assert_eq!(tree(r#""{a}{b}""#), "(str {a} {b})");
        assert_eq!(tree(r#""{f(a, (b))}""#), "(str {(call f a (paren b))})");
        assert_eq!(tree(r#""\{{a}\}""#), "(str \"{\" {a} \"}\")");
    }

    #[test]
    fn int_value_saturates_above_the_lexer_range() {
        assert_eq!(int_value("42"), 42);
        assert_eq!(int_value("99999999999999999999999999"), i64::MAX);
    }

    // ----- ranges -----

    #[test]
    fn ranges_cover_the_source_of_each_node() {
        assert_eq!(
            node_texts("-f(a, b) + (c)"),
            [
                "-f(a, b) + (c)",
                "-f(a, b)",
                "f(a, b)",
                "f",
                "a",
                "b",
                "(c)",
                "c"
            ]
        );
        assert_eq!(
            node_texts("not \"a{b}\" == c"),
            ["not \"a{b}\" == c", "\"a{b}\" == c", "\"a{b}\"", "b", "c"]
        );
    }

    /// The source text of every node of `let x = <expr>`, root first, then
    /// the children from left to right.
    fn node_texts(expr: &str) -> Vec<String> {
        let src = wrap(expr);
        let (m, _, diags) = parse_src(&src, ParseLimits::DEFAULT);
        assert!(diags.is_empty(), "{diags:?}");
        let mut texts = Vec::new();
        let mut todo: Vec<ExprId> = let_values(&m);
        while let Some(id) = todo.pop() {
            let Some(e) = m.expr(id) else { continue };
            let text = src.get(e.range.start() as usize..e.range.end() as usize);
            texts.push(text.unwrap_or("<bad range>").to_string());
            let mut children = match &e.kind {
                ExprKind::Unary { operand, .. } => vec![*operand],
                ExprKind::Binary { lhs, rhs, .. } => vec![*lhs, *rhs],
                ExprKind::Call { callee, args } => std::iter::once(*callee)
                    .chain(args.iter().copied())
                    .collect(),
                ExprKind::Paren(inner) => vec![*inner],
                ExprKind::Str(parts) => parts
                    .iter()
                    .filter_map(|part| match part {
                        StrPart::Interp(id) => Some(*id),
                        StrPart::Text(_) => None,
                    })
                    .collect(),
                ExprKind::Int(_) | ExprKind::Bool(_) | ExprKind::Name(_) => Vec::new(),
            };
            children.reverse();
            todo.extend(children);
        }
        texts
    }

    // ----- diagnostics -----

    #[test]
    fn comparisons_do_not_chain() {
        let msg = codes::MSG_COMPARISON_CHAIN.to_string();
        assert_eq!(one_error("1 < 2 < 3"), (27, 6, msg.clone()));
        assert_eq!(one_error("a == b != c"), (27, 7, msg.clone()));
        assert_eq!(one_error("a < b + 1 > c"), (27, 10, msg.clone()));
        assert_eq!(one_error("not a < b < c"), (27, 10, msg));
    }

    #[test]
    fn misplaced_operators_and_operands() {
        let (code, at, msg) = one_error("--x");
        assert_eq!((code, at), (28, 1));
        assert!(msg.contains("-(-x)"), "{msg}");

        let (code, at, msg) = one_error("a < not b");
        assert_eq!((code, at), (28, 4));
        assert!(msg.contains("in parentheses"), "{msg}");

        assert_eq!(
            one_error("f(1 2)"),
            (28, 4, "expected `,` or `)`, found `2`".into())
        );
        assert_eq!(
            one_error("f(a,)"),
            (28, 4, "expected an expression, found `)`".into())
        );
        assert_eq!(
            one_error("1 +"),
            (28, 3, "expected an expression, found end of line".into())
        );
        assert_eq!(
            one_error("and b"),
            (28, 0, "expected an expression, found `and`".into())
        );
        assert_eq!(
            one_error("\"{1 2}\""),
            (
                28,
                4,
                "expected `}` to close the interpolation, found `2`".into()
            )
        );
        assert_eq!(
            one_error("data"),
            (
                26,
                0,
                "`data` is a keyword and cannot be used as a name".into()
            )
        );
    }

    #[test]
    fn constructs_of_later_milestones_are_e0100() {
        let cases = [
            ("none", 0, "`none`"),
            ("me", 0, "`me`"),
            ("now", 0, "`now`"),
            ("signed", 0, "`signed`"),
            ("make Msg { text: t }", 0, "`make`"),
            ("{1, 2}", 0, "a set literal"),
            ("[1, 2]", 0, "a list or map literal"),
            ("a.len", 1, "field access with `.`"),
            ("f(x)[0]", 4, "indexing with `[...]`"),
            ("1..5", 1, "the range operator `..`"),
            ("a ?? b", 2, "the fallback operator `??`"),
            ("f(x) catch Denied", 5, "`catch`"),
            ("a in b", 2, "the operator `in`"),
            ("a not in b", 2, "the operator `not in`"),
            ("1 + a.b", 5, "field access with `.`"),
        ];
        for (expr, at, what) in cases {
            let expected = (100, at, codes::msg_not_in_v0_1(what));
            assert_eq!(one_error(expr), expected, "{expr}");
        }
    }

    #[test]
    fn a_faulty_line_does_not_disturb_the_next_lines() {
        // Two faulty lines inside brackets, then valid nested brackets right
        // at the depth limit: a leaked nesting level would report E0029.
        let src = "fn main()\n  let a = f((1 2))\n  let b = ((--x))\n  let c = ((1))\n";
        let diags = diags_of(src, limits(4, MAX_NODES, MAX_HEIGHT));
        let codes: Vec<u16> = diags.iter().map(|d| d.0).collect();
        assert_eq!(codes, [28, 28], "{diags:?}");
        let (m, _, _) = parse_src(src, limits(4, MAX_NODES, MAX_HEIGHT));
        let values: Vec<String> = let_values(&m).iter().map(|v| sexpr(&m, *v)).collect();
        assert_eq!(values, ["(paren (paren 1))"]);
    }

    #[test]
    fn no_parser_diagnostic_on_a_line_the_lexer_reported() {
        let (_, lex_diags, diags) = parse_src(&wrap("1 + $ + 2"), ParseLimits::DEFAULT);
        assert_eq!(lex_diags.len(), 1, "{lex_diags:?}");
        assert!(diags.is_empty(), "{diags:?}");
    }

    // ----- limits (D43, D54) -----

    #[test]
    fn nesting_limit_counts_parentheses_and_calls() {
        // The function body is level 1, so 255 brackets reach the default
        // limit of 256 exactly. This also shows that the native recursion
        // through brackets fits a test thread's stack.
        let open = 255;
        let ok = format!("{}1{}", "(".repeat(open), ")".repeat(open));
        assert!(diags_of(&wrap(&ok), ParseLimits::DEFAULT).is_empty());
        let calls = format!("{}1{}", "f(".repeat(open), ")".repeat(open));
        assert!(diags_of(&wrap(&calls), ParseLimits::DEFAULT).is_empty());

        let deep = format!("{}1{}", "(".repeat(open + 1), ")".repeat(open + 1));
        let diags = diags_of(&wrap(&deep), ParseLimits::DEFAULT);
        let codes: Vec<u16> = diags.iter().map(|d| d.0).collect();
        assert_eq!(codes, [29], "{diags:?}");
    }

    #[test]
    fn height_limit_boundary_is_exact() {
        let src = wrap(&chain(50));
        let (m, _, diags) = parse_src(&src, ParseLimits::DEFAULT);
        assert!(diags.is_empty(), "{diags:?}");
        let height = m.height();
        assert!(diags_of(&src, limits(MAX_DEPTH, MAX_NODES, height)).is_empty());
        let diags = diags_of(&src, limits(MAX_DEPTH, MAX_NODES, height - 1));
        let codes: Vec<u16> = diags.iter().map(|d| d.0).collect();
        assert_eq!(codes, [30], "{diags:?}");
    }

    #[test]
    fn node_limit_boundary_is_exact() {
        let src = wrap(&format!("not f(1, \"a{{b}}\") or {}", chain(20)));
        let (m, _, diags) = parse_src(&src, ParseLimits::DEFAULT);
        assert!(diags.is_empty(), "{diags:?}");
        let nodes = m.node_count();
        assert!(diags_of(&src, limits(MAX_DEPTH, nodes, MAX_HEIGHT)).is_empty());
        let diags = diags_of(&src, limits(MAX_DEPTH, nodes - 1, MAX_HEIGHT));
        let codes: Vec<u16> = diags.iter().map(|d| d.0).collect();
        assert_eq!(codes, [31], "{diags:?}");
    }

    #[test]
    fn hostile_chain_sizes_of_architecture_3_4() {
        // `chain_below` and `chain_above` of the hostile suite.
        assert!(diags_of(&wrap(&chain(2_000)), ParseLimits::DEFAULT).is_empty());
        let diags = diags_of(&wrap(&chain(2_100)), ParseLimits::DEFAULT);
        let codes: Vec<u16> = diags.iter().map(|d| d.0).collect();
        assert_eq!(codes, [30], "{diags:?}");
    }

    #[test]
    fn long_chains_use_no_native_stack() {
        // With the height limit lifted, a recursive parser would overflow
        // the stack long before these sizes.
        let lifted = limits(MAX_DEPTH, u32::MAX, u32::MAX);
        let src = wrap(&chain(50_000));
        assert!(diags_of(&src, lifted).is_empty());
        let nots = format!("{}x", "not ".repeat(50_000));
        assert!(diags_of(&wrap(&nots), lifted).is_empty());
        let mixed = vec!["a or not b and -c * d"; 5_000].join(" or ");
        assert!(diags_of(&wrap(&mixed), lifted).is_empty());
    }

    #[test]
    fn parse_uses_the_default_limits_and_this_parser() {
        let src = "fn main()\n  print(\"{1 + 2}\")\n";
        let (tokens, lexed) = lex(src, F);
        let (m, diags) = parse(src, F, &tokens, &lexed);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(m.limits(), Limits::DEFAULT);
        assert_eq!(m.items().len(), 1);
    }
}
