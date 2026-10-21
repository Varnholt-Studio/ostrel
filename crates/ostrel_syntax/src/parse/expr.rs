//! Expression parser (WP T1-3b, D43, SYNTAX 7).
//!
//! [`Exprs`] implements [`ExprParse`], the seam to the statement parser in
//! `mod.rs`. The contract is documented there.
//!
//! # Grammar read here
//!
//! ```text
//! expr      = orExpr ;
//! orExpr    = andExpr { "or" andExpr } ;
//! andExpr   = notExpr { "and" notExpr } ;
//! notExpr   = "not" notExpr | cmpExpr ;
//! cmpExpr   = fbExpr [ cmpOp fbExpr ] ;
//! cmpOp     = "==" | "!=" | "<" | "<=" | ">" | ">=" | "in" | "not" "in" ;
//! fbExpr    = rangeExpr [ "catch" Name { "," Name } ] { "??" rangeExpr } ;
//! rangeExpr = addExpr [ ".." addExpr ] ;
//! addExpr   = mulExpr { ( "+" | "-" ) mulExpr } ;
//! mulExpr   = unary { ( "*" | "/" | "%" ) unary } ;
//! unary     = [ "-" ] postfix ;
//! postfix   = primary { "." name | "(" [ expr { "," expr } ] ")" | "[" expr "]" } ;
//! primary   = Number | String | "true" | "false" | "none" | "me" | "now" | "signed"
//!           | name | "(" expr ")" | "{" [ expr { "," expr } ] "}"
//!           | "[" [ listOrMap ] "]"
//!           | "make" Name "{" [ name ":" expr { "," name ":" expr } ] "}" ;
//! listOrMap = ":" | expr ( { "," expr } | ":" expr { "," expr ":" expr } ) ;
//! ```
//!
//! # How operator chains are read (D43)
//!
//! The levels from `orExpr` down to `mulExpr` are not one function each.
//! [`expr`] reads them by precedence climbing with its own stack of
//! [`Pending`] operators: it reads an operand, then looks at the next infix
//! operator and first combines every pending operator that binds at least as
//! tightly. Prefix `not` waits on the same stack. So a chain such as
//! `1 + 1 + ... + 1`, `a ?? b ?? ...` or `not not ... x` costs heap memory,
//! never native stack, however long it is. The AST height limit (D54) then
//! ends a chain that is too high for the later recursive passes.
//!
//! Comparisons (`in` and `not in` included) and the range operator `..` do
//! not chain: the grammar allows one of them per level. `catch` follows the
//! value before the first `??` (G11).
//!
//! Native recursion happens only through brackets: call arguments, index
//! brackets, parentheses, set, list and map literals, the field list of
//! `make` and the expression of a string interpolation. Each bracket passes
//! [`Parser::enter`], so the depth is bounded by the nesting limit; an
//! interpolation cannot contain another string (G1), so it adds at most one
//! level around them.

use super::{ExprParse, ParseLimits, Parser, Stop, codes, text_range};
use crate::ast::{
    BinaryOp, ExprId, ExprKind, FieldInit, Ident, KeywordValue, MakeExpr, MapEntry, Module,
    StrPart, TextRange, UnaryOp,
};
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

/// The expression parser of the language.
#[derive(Clone, Copy, Debug, Default)]
pub struct Exprs;

impl ExprParse for Exprs {
    fn expr(&self, p: &mut Parser<'_>) -> Result<ExprId, Stop> {
        expr(p)
    }
}

/// Binding strength of the operator levels, from loosest to tightest
/// (`docs/grammar.md` section 4). Unary `-` and the postfix forms bind
/// tighter than all of them and are read with the operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Or,
    And,
    Not,
    Cmp,
    Fallback,
    Range,
    Add,
    Mul,
}

/// An infix operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Infix {
    /// One of the operators of the v0.1 slice.
    Binary(BinaryOp),
    /// `in`, or `not in` when `negated` is true.
    In { negated: bool },
    /// `??`
    Fallback,
    /// `..`
    Range,
}

/// An operator on the stack of [`expr`], still waiting for its right
/// operand.
#[derive(Clone, Copy, Debug)]
enum Pending {
    /// A prefix `not` that starts at byte offset `start`.
    Not { start: u32 },
    /// An infix operator and its complete left operand.
    Infix { op: Infix, prec: Prec, lhs: ExprId },
}

impl Pending {
    fn prec(self) -> Prec {
        match self {
            Pending::Not { .. } => Prec::Not,
            Pending::Infix { prec, .. } => prec,
        }
    }
}

/// The infix operator at the current token, its level and the number of
/// tokens it takes (2 for `not in`).
fn infix(p: &Parser<'_>) -> Option<(Infix, Prec, usize)> {
    let binary = |op| Some((Infix::Binary(op), Prec::Cmp, 1));
    match p.peek() {
        TokenKind::Kw(Keyword::Or) => Some((Infix::Binary(BinaryOp::Or), Prec::Or, 1)),
        TokenKind::Kw(Keyword::And) => Some((Infix::Binary(BinaryOp::And), Prec::And, 1)),
        TokenKind::EqEq => binary(BinaryOp::Eq),
        TokenKind::BangEq => binary(BinaryOp::Ne),
        TokenKind::Lt => binary(BinaryOp::Lt),
        TokenKind::Le => binary(BinaryOp::Le),
        TokenKind::Gt => binary(BinaryOp::Gt),
        TokenKind::Ge => binary(BinaryOp::Ge),
        TokenKind::Kw(Keyword::In) => Some((Infix::In { negated: false }, Prec::Cmp, 1)),
        TokenKind::Kw(Keyword::Not) if p.nth(1) == TokenKind::Kw(Keyword::In) => {
            Some((Infix::In { negated: true }, Prec::Cmp, 2))
        }
        TokenKind::QuestionQuestion => Some((Infix::Fallback, Prec::Fallback, 1)),
        TokenKind::DotDot => Some((Infix::Range, Prec::Range, 1)),
        TokenKind::Plus => Some((Infix::Binary(BinaryOp::Add), Prec::Add, 1)),
        TokenKind::Minus => Some((Infix::Binary(BinaryOp::Sub), Prec::Add, 1)),
        TokenKind::Star => Some((Infix::Binary(BinaryOp::Mul), Prec::Mul, 1)),
        TokenKind::Slash => Some((Infix::Binary(BinaryOp::Div), Prec::Mul, 1)),
        TokenKind::Percent => Some((Infix::Binary(BinaryOp::Rem), Prec::Mul, 1)),
        _ => None,
    }
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
        if p.peek() == TokenKind::Kw(Keyword::Catch) {
            operand = catch(p, &mut pending, operand)?;
        }
        let next = infix(p);

        // Combine every pending operator that binds at least as tightly as
        // the next one; all of them at the end of the expression. This makes
        // the chaining levels left associative.
        while let Some(&top) = pending.last() {
            if let Some((_, next_prec, _)) = next {
                if top.prec() < next_prec {
                    break;
                }
                if top.prec() == next_prec {
                    if next_prec == Prec::Cmp {
                        let at = p.current();
                        return Err(p.report(
                            codes::COMPARISON_CHAIN,
                            at,
                            codes::MSG_COMPARISON_CHAIN,
                        ));
                    }
                    if next_prec == Prec::Range {
                        return Err(p.expected("the end of the range (`..` does not chain)"));
                    }
                }
            }
            pending.pop();
            operand = combine(p, top, operand)?;
        }

        let Some((op, prec, tokens)) = next else {
            return Ok(operand);
        };
        for _ in 0..tokens {
            p.bump();
        }
        pending.push(Pending::Infix {
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
        Pending::Infix { op, lhs, .. } => {
            let range = range_of(p, lhs).cover(range_of(p, rhs));
            let kind = match op {
                Infix::Binary(op) => ExprKind::Binary { op, lhs, rhs },
                Infix::In { negated } => ExprKind::In {
                    element: lhs,
                    collection: rhs,
                    negated,
                },
                Infix::Fallback => ExprKind::Fallback {
                    value: lhs,
                    fallback: rhs,
                },
                Infix::Range => ExprKind::Range { lo: lhs, hi: rhs },
            };
            p.add_expr(kind, range)
        }
    }
}

/// `catch Name { "," Name }` after `operand` (G11); the current token is
/// `catch`.
///
/// `catch` belongs to the whole `rangeExpr` before it, so the pending
/// operators that bind more tightly than `??` are combined first. It must
/// come before the first `??` of its `fbExpr`, and only once.
fn catch(p: &mut Parser<'_>, pending: &mut Vec<Pending>, operand: ExprId) -> Result<ExprId, Stop> {
    let mut value = operand;
    while let Some(&top) = pending.last() {
        if top.prec() <= Prec::Fallback {
            break;
        }
        pending.pop();
        value = combine(p, top, value)?;
    }
    if pending
        .last()
        .is_some_and(|top| top.prec() == Prec::Fallback)
    {
        return Err(p.expected(
            "`??` or the end of the expression (`catch` stands right after the remote \
             value, before the first `??`)",
        ));
    }
    p.bump();
    let mut kinds = Vec::new();
    loop {
        kinds.push(p.name("an error kind such as `Denied` after `catch`")?);
        if p.peek() != TokenKind::Comma {
            break;
        }
        p.bump();
    }
    let range = text_range(range_of(p, value).start(), p.prev_end());
    let kind = ExprKind::Catch {
        value,
        kinds: kinds.into_boxed_slice(),
    };
    let caught = p.add_expr(kind, range)?;
    if p.peek() == TokenKind::Kw(Keyword::Catch) {
        return Err(p.expected("`??` (name all error kinds after one `catch`)"));
    }
    Ok(caught)
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

/// `postfix = primary { "." name | "(" args ")" | "[" expr "]" }`.
fn postfix(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let mut value = primary(p)?;
    loop {
        match p.peek() {
            TokenKind::LParen => value = call(p, value)?,
            TokenKind::Dot => {
                p.bump();
                let name = p.name("a field or property name after `.`")?;
                let range = text_range(range_of(p, value).start(), name.range.end());
                value = p.add_expr(ExprKind::Field { base: value, name }, range)?;
            }
            TokenKind::LBracket => {
                let open = p.bump();
                p.enter(open)?;
                let index = expr(p).and_then(|index| close(p, TokenKind::RBracket, "`]`", index));
                p.leave();
                let index = index?;
                let range = text_range(range_of(p, value).start(), p.prev_end());
                value = p.add_expr(ExprKind::Index { base: value, index }, range)?;
            }
            _ => return Ok(value),
        }
    }
}

/// Consumes the closing bracket `kind` after `inner`, or reports E0028
/// naming `what`.
fn close<T>(p: &mut Parser<'_>, kind: TokenKind, what: &str, inner: T) -> Result<T, Stop> {
    if p.peek() == kind {
        p.bump();
        Ok(inner)
    } else {
        Err(p.expected(what))
    }
}

/// A call of `callee`; the current token is its `(`.
fn call(p: &mut Parser<'_>, callee: ExprId) -> Result<ExprId, Stop> {
    let open = p.bump();
    p.enter(open)?;
    let args = list_items(p, TokenKind::RParen, "`,` or `)`");
    p.leave();
    let args = args?;
    let range = range_of(p, callee).cover(text_range(open.span.start, p.prev_end()));
    let kind = ExprKind::Call {
        callee,
        args: args.into_boxed_slice(),
    };
    p.add_expr(kind, range)
}

/// Comma separated expressions after an opening bracket, up to and
/// including the closing bracket `end`.
fn list_items(p: &mut Parser<'_>, end: TokenKind, what: &str) -> Result<Vec<ExprId>, Stop> {
    let mut items = Vec::new();
    if p.peek() == end {
        p.bump();
        return Ok(items);
    }
    loop {
        items.push(expr(p)?);
        match p.peek() {
            TokenKind::Comma => {
                p.bump();
            }
            kind if kind == end => {
                p.bump();
                return Ok(items);
            }
            _ => return Err(p.expected(what)),
        }
    }
}

/// `primary`, see the grammar in the module documentation.
fn primary(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let tok = p.current();
    let range = text_range(tok.span.start, tok.span.end);
    let kind = match tok.kind {
        TokenKind::Int => ExprKind::Int(int_value(p.text(tok))),
        TokenKind::Kw(Keyword::True) => ExprKind::Bool(true),
        TokenKind::Kw(Keyword::False) => ExprKind::Bool(false),
        TokenKind::Kw(Keyword::None) => ExprKind::Keyword(KeywordValue::NoneValue),
        TokenKind::Kw(Keyword::Me) => ExprKind::Keyword(KeywordValue::Me),
        TokenKind::Kw(Keyword::Now) => ExprKind::Keyword(KeywordValue::Now),
        TokenKind::Kw(Keyword::Signed) => ExprKind::Keyword(KeywordValue::Signed),
        TokenKind::Ident => ExprKind::Name(Ident::new(p.text(tok), range)),
        TokenKind::Str => {
            let mut parts = Vec::new();
            push_text(&mut parts, p.text(tok));
            ExprKind::Str(parts.into_boxed_slice())
        }
        TokenKind::StrHead => return interpolated(p),
        TokenKind::LParen => return parenthesized(p),
        TokenKind::LBrace => return set_literal(p),
        TokenKind::LBracket => return list_or_map(p),
        TokenKind::Kw(Keyword::Make) => return make(p),
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

/// `"{" [ expr { "," expr } ] "}"`, a set literal (G2); the current token is
/// the `{`.
fn set_literal(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let open = p.bump();
    p.enter(open)?;
    let items = list_items(p, TokenKind::RBrace, "`,` or `}`");
    p.leave();
    let items = items?;
    let range = text_range(open.span.start, p.prev_end());
    p.add_expr(ExprKind::Set(items.into_boxed_slice()), range)
}

/// `"[" [ listOrMap ] "]"`, a list or map literal (G2); the current token is
/// the `[`. The first element decides: `k: v` makes a map, `[:]` is the
/// empty map.
fn list_or_map(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let open = p.bump();
    p.enter(open)?;
    let kind = list_or_map_items(p);
    p.leave();
    let kind = kind?;
    let range = text_range(open.span.start, p.prev_end());
    p.add_expr(kind, range)
}

fn list_or_map_items(p: &mut Parser<'_>) -> Result<ExprKind, Stop> {
    match p.peek() {
        TokenKind::RBracket => {
            p.bump();
            return Ok(ExprKind::List(Box::default()));
        }
        TokenKind::Colon => {
            p.bump();
            return close(
                p,
                TokenKind::RBracket,
                "`]` after `[:`",
                ExprKind::Map(Box::default()),
            );
        }
        _ => {}
    }
    let first = expr(p)?;
    if p.peek() != TokenKind::Colon {
        let mut items = vec![first];
        match p.peek() {
            TokenKind::RBracket => {
                p.bump();
            }
            TokenKind::Comma => {
                p.bump();
                items.extend(list_items(p, TokenKind::RBracket, "`,` or `]`")?);
            }
            _ => return Err(p.expected("`,`, `:` or `]`")),
        }
        return Ok(ExprKind::List(items.into_boxed_slice()));
    }
    let mut entries = Vec::new();
    let mut key = first;
    loop {
        if p.peek() != TokenKind::Colon {
            return Err(p.expected("`:` and a value (every entry of a map is `key: value`)"));
        }
        p.bump();
        let value = expr(p)?;
        entries.push(MapEntry { key, value });
        match p.peek() {
            TokenKind::RBracket => {
                p.bump();
                return Ok(ExprKind::Map(entries.into_boxed_slice()));
            }
            TokenKind::Comma => {
                p.bump();
            }
            _ => return Err(p.expected("`,` or `]`")),
        }
        key = expr(p)?;
    }
}

/// `"make" Name "{" [ name ":" expr { "," name ":" expr } ] "}"`; the current
/// token is `make`.
fn make(p: &mut Parser<'_>) -> Result<ExprId, Stop> {
    let kw = p.bump();
    let ty = p.name("a data type name after `make`")?;
    if p.peek() != TokenKind::LBrace {
        let what = format!("`{{` and the field values after `make {}`", ty.text);
        return Err(p.expected(&what));
    }
    let open = p.bump();
    p.enter(open)?;
    let fields = make_fields(p);
    p.leave();
    let fields = fields?;
    let range = text_range(kw.span.start, p.prev_end());
    let make = MakeExpr {
        ty,
        fields: fields.into_boxed_slice(),
    };
    p.add_expr(ExprKind::Make(Box::new(make)), range)
}

/// The `name: value` pairs of `make` after its `{`, up to and including the
/// `}`.
fn make_fields(p: &mut Parser<'_>) -> Result<Vec<FieldInit>, Stop> {
    let mut fields = Vec::new();
    if p.peek() == TokenKind::RBrace {
        p.bump();
        return Ok(fields);
    }
    loop {
        let name = p.name("a field name")?;
        if p.peek() != TokenKind::Colon {
            let what = format!("`:` and a value after field `{}`", name.text);
            return Err(p.expected(&what));
        }
        p.bump();
        let value = expr(p)?;
        let range = text_range(name.range.start(), range_of(p, value).end());
        fields.push(FieldInit { name, value, range });
        match p.peek() {
            TokenKind::Comma => {
                p.bump();
            }
            TokenKind::RBrace => {
                p.bump();
                return Ok(fields);
            }
            _ => return Err(p.expected("`,` or `}`")),
        }
    }
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
            let Item::Fn(decl) = item else { continue };
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
        super::super::items::dump::expr(m, id)
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
                ExprKind::Field { base, .. } => vec![*base],
                ExprKind::Index { base, index } => vec![*base, *index],
                ExprKind::In {
                    element,
                    collection,
                    ..
                } => vec![*element, *collection],
                ExprKind::Range { lo, hi } => vec![*lo, *hi],
                ExprKind::Catch { value, .. } => vec![*value],
                ExprKind::Fallback { value, fallback } => vec![*value, *fallback],
                ExprKind::Set(items) | ExprKind::List(items) => items.to_vec(),
                ExprKind::Map(entries) => entries.iter().flat_map(|e| [e.key, e.value]).collect(),
                ExprKind::Make(make) => make.fields.iter().map(|f| f.value).collect(),
                _ => Vec::new(),
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

    // ----- v0.2 expressions (T119) -----

    #[test]
    fn value_keywords_fields_and_indexes() {
        assert_eq!(tree("none"), "none");
        assert_eq!(tree("me == signed"), "(== me signed)");
        assert_eq!(tree("now - days.days"), "(- now (. days days))");
        assert_eq!(tree("m.author.handle"), "(. (. m author) handle)");
        assert_eq!(tree("t.roles[me]"), "(index (. t roles) me)");
        assert_eq!(tree("r.members.add(me)"), "(call (. (. r members) add) me)");
        assert_eq!(tree("f(x)[0].y"), "(. (index (call f x) 0) y)");
        assert_eq!(tree("-a.b"), "(neg (. a b))");
    }

    #[test]
    fn membership_ranges_and_fallbacks() {
        assert_eq!(tree("me in room.members"), "(in me (. room members))");
        assert_eq!(tree("a not in b"), "(not-in a b)");
        assert_eq!(tree("not a in b"), "(not (in a b))");
        assert_eq!(tree("text.len in 1..2000"), "(in (. text len) (.. 1 2000))");
        assert_eq!(tree("a + 1..b * 2"), "(.. (+ a 1) (* b 2))");
        assert_eq!(tree("a ?? b ?? c"), "(?? (?? a b) c)");
        assert_eq!(
            tree("(team.roles[me] ?? guest) >= member"),
            "(>= (paren (?? (index (. team roles) me) guest)) member)"
        );
        assert_eq!(tree("a ?? b == c"), "(== (?? a b) c)");
        assert_eq!(tree("a ?? b + 1"), "(?? a (+ b 1))");
        assert_eq!(
            tree("added == {me} or removed == {me}"),
            "(or (== added (set me)) (== removed (set me)))"
        );
    }

    #[test]
    fn catch_names_the_kinds_before_the_first_fallback() {
        assert_eq!(
            tree("f(x) catch Denied ?? y"),
            "(?? (catch (call f x) Denied) y)"
        );
        assert_eq!(
            tree("f(x) catch Denied, Invalid ?? y ?? z"),
            "(?? (?? (catch (call f x) Denied Invalid) y) z)"
        );
        assert_eq!(tree("a + b catch Offline"), "(catch (+ a b) Offline)");
        assert_eq!(
            tree("not f(x) catch Denied"),
            "(not (catch (call f x) Denied))"
        );
        assert_eq!(
            tree("x == f(y) catch Denied ?? z"),
            "(== x (?? (catch (call f y) Denied) z))"
        );
    }

    #[test]
    fn collection_literals_and_make() {
        assert_eq!(tree("{}"), "(set)");
        assert_eq!(tree("{a, b + 1}"), "(set a (+ b 1))");
        assert_eq!(tree("[]"), "(list)");
        assert_eq!(tree("[1, 2, 3]"), "(list 1 2 3)");
        assert_eq!(tree("[:]"), "(map)");
        assert_eq!(tree("[me: admin]"), "(map (me admin))");
        assert_eq!(tree("[a: 1, b: {c}]"), "(map (a 1) (b (set c)))");
        assert_eq!(tree("make Room {}"), "(make Room)");
        assert_eq!(
            tree("make Message { room: r, text: it }"),
            "(make Message (room r) (text it))"
        );
        assert_eq!(
            tree("make Team { roles: [me: admin] }.roles"),
            "(. (make Team (roles (map (me admin)))) roles)"
        );
    }

    #[test]
    fn v0_2_ranges_cover_the_source_of_each_node() {
        assert_eq!(
            node_texts("t.roles[me] ?? guest"),
            [
                "t.roles[me] ?? guest",
                "t.roles[me]",
                "t.roles",
                "t",
                "me",
                "guest"
            ]
        );
        assert_eq!(
            node_texts("f(x) catch Denied, Invalid"),
            ["f(x) catch Denied, Invalid", "f(x)", "f", "x"]
        );
        assert_eq!(
            node_texts("make M { a: [k: v], b: {1} }"),
            [
                "make M { a: [k: v], b: {1} }",
                "[k: v]",
                "k",
                "v",
                "{1}",
                "1"
            ]
        );
        assert_eq!(
            node_texts("x not in 1..9"),
            ["x not in 1..9", "x", "1..9", "1", "9"]
        );
    }

    #[test]
    fn deeper_lines_continue_brackets_and_operand_braces() {
        // D164: multi line literals, records and calls whose lines stand
        // deeper than the start of the logical line, or start with a closer,
        // parse without a diagnostic and keep the block structure around them.
        let src = concat!(
            "fn main()\n",
            "  let a = [\n    1,\n    2\n  ]\n",
            "  let m = [\n    \"k\": {\n      1\n    }\n]\n",
            "  let r = make Room {\n    name: \"a\",\n    size: f(\n      1\n    )\n  }\n",
            "  if a == {\n    1\n  }\n    print(1)\n",
            "  print(a)\n",
        );
        assert_eq!(diags_of(src, ParseLimits::DEFAULT), vec![]);
        let (m, _, _) = parse_src(src, ParseLimits::DEFAULT);
        assert_eq!(let_values(&m).len(), 3);
        // A block brace after a complete head is still E0022 with its lone
        // `}` skipped, also when the head has an operand `{` before it.
        let src = "fn main()\n  if a == {1} {\n    print(1)\n  }\n  print(2)\n";
        let found: Vec<(u16, u32)> = diags_of(src, ParseLimits::DEFAULT)
            .into_iter()
            .map(|(code, at, _)| (code, at))
            .collect();
        let brace = src.rfind(" {\n").map_or(0, |i| i + 1);
        assert_eq!(found, vec![(22, u32::try_from(brace).unwrap_or(0))]);
    }

    #[test]
    fn faulty_v0_2_expressions_report_once() {
        let msg = codes::MSG_COMPARISON_CHAIN.to_string();
        assert_eq!(one_error("a in b in c"), (27, 7, msg.clone()));
        assert_eq!(one_error("a == b not in c"), (27, 7, msg));
        let cases = [
            (
                "1..2..3",
                4,
                "the end of the range (`..` does not chain)",
                "`..`",
            ),
            (
                "a ?? b catch Denied",
                7,
                "`??` or the end of the expression (`catch` stands right after the remote \
                 value, before the first `??`)",
                "`catch`",
            ),
            (
                "f(x) catch Denied catch Invalid",
                18,
                "`??` (name all error kinds after one `catch`)",
                "`catch`",
            ),
            (
                "f(x) catch",
                10,
                "an error kind such as `Denied` after `catch`",
                "end of line",
            ),
            ("a.", 2, "a field or property name after `.`", "end of line"),
            ("{1 2}", 3, "`,` or `}`", "`2`"),
            ("[1, 2: 3]", 5, "`,` or `]`", "`:`"),
            (
                "[1: 2, 3]",
                8,
                "`:` and a value (every entry of a map is `key: value`)",
                "`]`",
            ),
            ("[1 2]", 3, "`,`, `:` or `]`", "`2`"),
            ("make", 4, "a data type name after `make`", "end of line"),
            (
                "make M(a)",
                6,
                "`{` and the field values after `make M`",
                "`(`",
            ),
            ("make M { a }", 11, "`:` and a value after field `a`", "`}`"),
            ("make M { a: 1 b: 2 }", 14, "`,` or `}`", "`b`"),
        ];
        for (expr, at, what, found) in cases {
            let expected = (28, at, codes::msg_expected(what, found));
            assert_eq!(one_error(expr), expected, "{expr}");
        }
        // D164: a `[` or `{` not closed on its logical line is E0025 at the
        // outermost open bracket, like `(`, not an expected closer at the
        // line end (formerly E0028 for `a[1` and `[:`).
        for (expr, at, msg) in [
            ("a[1", 1, codes::MSG_UNCLOSED_BRACKET),
            ("[:", 0, codes::MSG_UNCLOSED_BRACKET),
            ("{1, 2", 0, codes::MSG_UNCLOSED_BRACE),
            ("make M { a: 1", 7, codes::MSG_UNCLOSED_BRACE),
            ("f([1, {2", 1, codes::MSG_UNCLOSED_PAREN),
            ("[1, f(2", 0, codes::MSG_UNCLOSED_BRACKET),
        ] {
            assert_eq!(one_error(expr), (25, at, msg.to_string()), "{expr}");
        }
        assert_eq!(
            one_error("a.if"),
            (
                26,
                2,
                "`if` is a keyword and cannot be used as a name".into()
            )
        );
        assert_eq!(
            one_error("make M { if: 1 }"),
            (
                26,
                9,
                "`if` is a keyword and cannot be used as a name".into()
            )
        );
    }

    #[test]
    fn long_fallback_chains_and_nested_literals_are_bounded() {
        let lifted = limits(MAX_DEPTH, u32::MAX, u32::MAX);
        let chain = vec!["a"; 50_000].join(" ?? ");
        assert!(diags_of(&wrap(&chain), lifted).is_empty());
        // Every bracket form counts as one nesting level.
        for (open, close) in [("{", "}"), ("[", "]"), ("a[", "]"), ("make M { f: ", " }")] {
            let ok = format!("{}1{}", open.repeat(255), close.repeat(255));
            assert!(
                diags_of(&wrap(&ok), ParseLimits::DEFAULT).is_empty(),
                "{open}"
            );
            let deep = format!("{}1{}", open.repeat(256), close.repeat(256));
            let codes: Vec<u16> = diags_of(&wrap(&deep), ParseLimits::DEFAULT)
                .iter()
                .map(|d| d.0)
                .collect();
            assert_eq!(codes, [29], "{open}");
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
