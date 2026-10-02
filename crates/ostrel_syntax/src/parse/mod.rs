//! Parser for the v0.1 slice (ARCHITECTURE 3.4, SYNTAX 7, WP T1-3).
//!
//! The parser turns the tokens of one file into an [`ast::Module`](crate::ast::Module) and never
//! fails: every fault is a [`Diagnostic`]. It reads declarations, blocks and
//! statements by recursive descent. Expressions are read by an
//! [`ExprParse`] implementation (WP T1-3b, `expr.rs`, precedence climbing with
//! an explicit stack, D43) through the small interface of [`Parser`].
//!
//! # Grammar read here
//!
//! ```text
//! program = { fnDecl } ;
//! fnDecl  = "fn" name "(" [ name ":" type { "," name ":" type } ] ")" [ "->" type ] block ;
//! block   = NL INDENT { stmt } DEDENT ;
//! stmt    = "let" name "=" expr NL
//!         | "if" expr block [ "else" block ]
//!         | "return" expr NL
//!         | expr NL ;
//! ```
//!
//! Other declarations and statements of SYNTAX 7 (`data`, `for`, assignment,
//! `if let` and so on) are reported once with `E0100 not available in v0.1`
//! and skipped, so the next item or statement still parses.
//!
//! # Recovery
//!
//! One fault gives one diagnostic (SPEC 12.1):
//!
//! * A fault in a statement skips the rest of its logical line. An indented
//!   block below a line that no known block head opened is skipped without a
//!   further diagnostic.
//! * A fault in a block head (`fn`, `if`, `else`) skips the rest of the head
//!   line, but the block below it is still parsed, so faults inside the block
//!   are reported and nothing in it is reported twice.
//! * A logical line on which a lexer diagnostic starts has already been
//!   reported. The parser never reports a second diagnostic on such a line.
//!   The lines are found from the spans of the lexer diagnostics passed to
//!   [`parse_with`], never from the shape of the tokens, so every fault the
//!   lexer reports (an `Error` token, a tab, a jump of indentation and so on)
//!   is covered the same way.
//! * A `(` that is not closed on its logical line is reported once at the
//!   `(` (E0025) before anything else on that line.
//! * After `{` at the end of a block head (E0022), the matching lone `}` line
//!   is skipped without a diagnostic.
//!
//! # Limits
//!
//! Three limits (D43, D54, ARCHITECTURE 3.4) end the parse with one
//! diagnostic at the triggering token and no further diagnostics: the
//! nesting depth of brackets and blocks ([`MAX_DEPTH`]), the height of the
//! syntax tree ([`MAX_HEIGHT`](crate::ast::MAX_HEIGHT)) and the number of
//! tree nodes ([`MAX_NODES`](crate::ast::MAX_NODES)).
//! Each can be lowered with [`ParseLimits`] to test the exact boundary.

pub mod codes;

#[cfg(test)]
mod tests;

use crate::ast::{
    AstError, BlockId, ExprId, ExprKind, FnDecl, Ident, Limits, Module, Param, StmtId, StmtKind,
    TextRange, TypeRef,
};
use crate::lex::{Keyword, Token, TokenKind};
use ostrel_core::{Code, Diagnostic, FileId, Span};

/// Deepest allowed nesting of brackets and blocks (ARCHITECTURE 3.4). A
/// function body is level 1.
pub const MAX_DEPTH: u32 = 256;

/// The limits a parse enforces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseLimits {
    /// Deepest allowed nesting of brackets and blocks.
    pub max_depth: u32,
    /// Node and height limits of the syntax tree.
    pub tree: Limits,
}

impl ParseLimits {
    /// [`MAX_DEPTH`], [`MAX_NODES`](crate::ast::MAX_NODES) and
    /// [`MAX_HEIGHT`](crate::ast::MAX_HEIGHT).
    pub const DEFAULT: ParseLimits = ParseLimits {
        max_depth: MAX_DEPTH,
        tree: Limits::DEFAULT,
    };
}

impl Default for ParseLimits {
    fn default() -> Self {
        ParseLimits::DEFAULT
    }
}

/// Why parsing of a construct stopped early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The current logical line is faulty and its diagnostic has been
    /// reported (or suppressed, see [`Parser::report`]). The caller skips the
    /// rest of the line.
    Line,
    /// A limit was exceeded and reported; the file is abandoned.
    File,
}

/// Expression parser, implemented by WP T1-3b (`expr.rs`).
///
/// Contract between the statement parser and the expression parser:
///
/// * [`ExprParse::expr`] is called at the first token of an expression. It
///   consumes one expression and stops at the first token that cannot
///   continue it, for example `NL`, `)`, `,`, `:`, `{`, `=` or the name `is`.
///   It never consumes `NL`, `INDENT`, `DEDENT` or `EOF`.
/// * On a fault it reports exactly one diagnostic with [`Parser::report`] or
///   [`Parser::expected`] and returns [`Stop::Line`]. It never reports a
///   diagnostic for an `Error` token; [`Parser::report`] suppresses
///   diagnostics on lines the lexer has already reported, so reporting
///   through it is always safe.
/// * Every `(` it opens goes through [`Parser::enter`] and [`Parser::leave`],
///   and every node through [`Parser::add_expr`]. A [`Stop::File`] from these
///   is returned unchanged.
/// * The statement parser has already reported a `(` that is not closed on
///   the logical line (E0025), so the expression parser always finds the
///   `)` of every `(` before the end of the line.
pub trait ExprParse {
    /// Parses one expression starting at the current token of `p`.
    fn expr(&self, p: &mut Parser<'_>) -> Result<ExprId, Stop>;
}

/// Parses the tokens of one file.
///
/// `src` is the source text the tokens were lexed from, `file` its id for
/// diagnostics. `lexed` holds the diagnostics the lexer reported for these
/// tokens: no parser diagnostic is reported on a logical line on which one
/// of them starts (one fault, one diagnostic). Trivia tokens are skipped. A
/// token stream without a final `EOF` is treated as if it had one at the end
/// of `src`.
pub fn parse_with(
    src: &str,
    file: FileId,
    tokens: &[Token],
    lexed: &[Diagnostic],
    limits: ParseLimits,
    exprs: &dyn ExprParse,
) -> (Module, Vec<Diagnostic>) {
    let mut p = Parser::new(src, file, tokens, lexed, limits, exprs);
    // A `Stop::File` has been reported; the module keeps what was parsed.
    let _ = p.program();
    (p.module, p.diags)
}

/// Facts about the logical lines, computed before parsing.
#[derive(Default)]
struct Lines {
    line_of: Vec<u32>,
    unclosed: Vec<Option<u32>>,
    faulty: Vec<(u32, u32)>,
    steps: u64,
}

/// Parser state. [`ExprParse`] implementations use its public methods.
pub struct Parser<'a> {
    src: &'a str,
    file: FileId,
    exprs: &'a dyn ExprParse,
    toks: Vec<Token>,
    /// Logical line of every token, as an index into `unclosed`.
    line_of: Vec<u32>,
    /// Per logical line: token index of the outermost `(` not closed on it.
    unclosed: Vec<Option<u32>>,
    /// Byte ranges `start..end` (end exclusive) of the logical lines the
    /// lexer has already reported, sorted and disjoint.
    faulty: Vec<(u32, u32)>,
    /// Work counter: token lookups plus scanned tokens. Tests use it to show
    /// that parsing is linear without measuring time (D74).
    steps: std::cell::Cell<u64>,
    pos: usize,
    module: Module,
    diags: Vec<Diagnostic>,
    depth: u32,
    max_depth: u32,
    /// Blocks opened with `{` (E0022) whose lone `}` is skipped silently.
    braces: u32,
}

impl<'a> Parser<'a> {
    fn new(
        src: &'a str,
        file: FileId,
        tokens: &[Token],
        lexed: &[Diagnostic],
        limits: ParseLimits,
        exprs: &'a dyn ExprParse,
    ) -> Parser<'a> {
        let mut toks: Vec<Token> = tokens.iter().copied().filter(|t| !t.is_trivia()).collect();
        if toks.last().map(|t| t.kind) != Some(TokenKind::Eof) {
            let end = u32::try_from(src.len()).unwrap_or(u32::MAX);
            toks.push(Token::new(TokenKind::Eof, Span::point(file, end)));
        }
        let lines = scan_lines(src, &toks, lexed);
        Parser {
            src,
            file,
            exprs,
            toks,
            line_of: lines.line_of,
            unclosed: lines.unclosed,
            faulty: lines.faulty,
            steps: std::cell::Cell::new(lines.steps),
            pos: 0,
            module: Module::with_limits(limits.tree),
            diags: Vec::new(),
            depth: 0,
            max_depth: limits.max_depth,
            braces: 0,
        }
    }

    // ----- interface for the expression parser -----

    /// The current token. At the end it stays on `EOF`.
    pub fn current(&self) -> Token {
        self.nth_token(0)
    }

    /// Kind of the current token.
    pub fn peek(&self) -> TokenKind {
        self.current().kind
    }

    /// Kind of the token `n` tokens after the current one (`EOF` past the
    /// end).
    pub fn nth(&self, n: usize) -> TokenKind {
        self.nth_token(n).kind
    }

    /// Consumes the current token and returns it. `EOF` is never consumed.
    pub fn bump(&mut self) -> Token {
        let tok = self.current();
        if tok.kind != TokenKind::Eof {
            self.pos += 1;
        }
        tok
    }

    /// The source text of `tok`.
    pub fn text(&self, tok: Token) -> &'a str {
        self.src
            .get(tok.span.start as usize..tok.span.end as usize)
            .unwrap_or("")
    }

    /// End offset of the last consumed token, 0 before the first one.
    pub fn prev_end(&self) -> u32 {
        self.pos
            .checked_sub(1)
            .and_then(|i| self.toks.get(i))
            .map_or(0, |t| t.span.end)
    }

    /// The syntax tree built so far.
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// Adds an expression node. A tree limit ends the parse
    /// ([`Stop::File`]).
    pub fn add_expr(&mut self, kind: ExprKind, range: TextRange) -> Result<ExprId, Stop> {
        let added = self.module.add_expr(kind, range);
        self.tree(added)
    }

    /// Enters a bracket or block that starts at `open`. Exceeding the
    /// nesting limit ends the parse ([`Stop::File`]).
    pub fn enter(&mut self, open: Token) -> Result<(), Stop> {
        if self.depth >= self.max_depth {
            let msg = codes::msg_nesting_too_deep(self.max_depth);
            return Err(self.fatal(codes::NESTING_TOO_DEEP, open, msg));
        }
        self.depth += 1;
        Ok(())
    }

    /// Leaves the bracket or block entered last.
    pub fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Reports `msg` with `code` at `at` and returns [`Stop::Line`].
    ///
    /// The diagnostic is dropped if the lexer has already reported a fault on
    /// the logical line of `at` (one fault, one diagnostic).
    pub fn report(&mut self, code: Code, at: Token, msg: impl Into<String>) -> Stop {
        if !self.lexed_fault_at(at) {
            self.diags.push(Diagnostic::error(code, at.span, msg));
        }
        Stop::Line
    }

    /// Reports E0028 "expected `what`, found ..." at the current token and
    /// returns [`Stop::Line`].
    pub fn expected(&mut self, what: &str) -> Stop {
        let at = self.current();
        let msg = codes::msg_expected(what, &self.describe(at));
        self.report(codes::EXPECTED, at, msg)
    }

    /// A short description of `tok` for messages, for example `` `)` `` or
    /// `end of line`.
    pub fn describe(&self, tok: Token) -> String {
        let quoted = |text: &str| format!("`{text}`");
        match tok.kind {
            TokenKind::Nl => "end of line".into(),
            TokenKind::Indent => "an indented line".into(),
            TokenKind::Dedent => "end of block".into(),
            TokenKind::Eof => "end of file".into(),
            TokenKind::Str | TokenKind::StrHead => "a string".into(),
            TokenKind::StrMid | TokenKind::StrTail => "string text".into(),
            TokenKind::Error => "invalid source".into(),
            TokenKind::Int if tok.span.len() > 24 => "a number".into(),
            TokenKind::Ident if tok.span.len() > 40 => "a name".into(),
            _ => quoted(self.text(tok)),
        }
    }

    // ----- internals -----

    /// Work done so far (see the field `steps`).
    #[cfg(test)]
    fn steps(&self) -> u64 {
        self.steps.get()
    }

    fn step(&self) {
        self.steps.set(self.steps.get().saturating_add(1));
    }

    fn nth_token(&self, n: usize) -> Token {
        self.step();
        let eof = Token::new(TokenKind::Eof, Span::point(self.file, 0));
        let last = self.toks.last().copied().unwrap_or(eof);
        self.pos
            .checked_add(n)
            .and_then(|i| self.toks.get(i))
            .copied()
            .unwrap_or(last)
    }

    fn unclosed_on_line(&self, index: usize) -> Option<u32> {
        self.line_of
            .get(index)
            .and_then(|l| self.unclosed.get(*l as usize))
            .copied()
            .flatten()
    }

    /// True if `tok` lies on a logical line the lexer has already reported.
    fn lexed_fault_at(&self, tok: Token) -> bool {
        in_ranges(&self.faulty, tok.span.start)
    }

    /// Reports a limit diagnostic, never suppressed, and returns
    /// [`Stop::File`].
    fn fatal(&mut self, code: Code, at: Token, msg: String) -> Stop {
        self.diags.push(Diagnostic::error(code, at.span, msg));
        Stop::File
    }

    fn tree<T>(&mut self, added: Result<T, AstError>) -> Result<T, Stop> {
        added.map_err(|err| {
            let at = self.current();
            let limits = self.module.limits();
            match err {
                AstError::NodeLimit => self.fatal(
                    codes::TOO_MANY_NODES,
                    at,
                    codes::msg_too_many_nodes(limits.max_nodes),
                ),
                AstError::HeightLimit => self.fatal(
                    codes::TREE_TOO_HIGH,
                    at,
                    codes::msg_tree_too_high(limits.max_height),
                ),
                // Only a faulty expression parser can reuse or invent ids.
                AstError::UnknownId | AstError::AlreadyAttached => {
                    self.fatal(codes::EXPECTED, at, format!("internal parser error: {err}"))
                }
            }
        })
    }

    fn is_word(&self, tok: Token, word: &str) -> bool {
        tok.kind == TokenKind::Ident && self.text(tok) == word
    }

    /// Reports E0025 if the rest of the current logical line has a `(`
    /// without its `)`.
    fn check_unclosed(&mut self) -> Result<(), Stop> {
        match self.unclosed_on_line(self.pos) {
            Some(index) if index as usize >= self.pos => {
                let at = self
                    .toks
                    .get(index as usize)
                    .copied()
                    .unwrap_or(self.current());
                Err(self.report(codes::UNCLOSED_PAREN, at, codes::MSG_UNCLOSED_PAREN))
            }
            _ => Ok(()),
        }
    }

    fn range_from(&self, start: u32) -> TextRange {
        text_range(start, self.prev_end())
    }

    fn ident(&self, tok: Token) -> Ident {
        Ident::new(self.text(tok), text_range(tok.span.start, tok.span.end))
    }

    // ----- recovery -----

    /// Skips the rest of the logical line, including its `NL`.
    fn skip_line(&mut self) {
        loop {
            match self.peek() {
                TokenKind::Nl => {
                    self.bump();
                    return;
                }
                TokenKind::Eof | TokenKind::Indent | TokenKind::Dedent => return,
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// Skips an indented block from its `INDENT` to its `DEDENT`.
    fn skip_block(&mut self) {
        let mut level: u32 = 0;
        loop {
            match self.peek() {
                TokenKind::Indent => level += 1,
                TokenKind::Dedent => {
                    level = level.saturating_sub(1);
                    if level == 0 {
                        self.bump();
                        return;
                    }
                }
                TokenKind::Eof => return,
                _ => {}
            }
            self.bump();
        }
    }

    /// Recovery after a faulty line: skip it and any block below it.
    fn recover(&mut self) {
        self.skip_line();
        if self.peek() == TokenKind::Indent {
            self.skip_block();
        }
    }

    /// Recovery after a faulty block head: skip the rest of the head line and
    /// parse the block below it, if any, for its own diagnostics.
    fn recover_head(&mut self) -> Result<(), Stop> {
        self.skip_line();
        if self.peek() == TokenKind::Indent {
            self.block()?;
        }
        Ok(())
    }

    /// Consumes a lone `}` line left over from a block opened with `{`.
    fn skip_closing_brace(&mut self) -> bool {
        if self.peek() == TokenKind::RBrace && self.braces > 0 {
            self.braces -= 1;
            self.bump();
            if self.peek() == TokenKind::Nl {
                self.bump();
            }
            return true;
        }
        false
    }

    // ----- declarations -----

    fn program(&mut self) -> Result<(), Stop> {
        loop {
            let before = self.pos;
            match self.peek() {
                TokenKind::Eof => return Ok(()),
                TokenKind::Nl | TokenKind::Dedent => {
                    self.bump();
                }
                _ => match self.item() {
                    Ok(()) => {}
                    Err(Stop::Line) => self.recover(),
                    Err(Stop::File) => return Err(Stop::File),
                },
            }
            if self.pos == before {
                self.bump();
            }
        }
    }

    fn item(&mut self) -> Result<(), Stop> {
        if self.skip_closing_brace() {
            return Ok(());
        }
        let tok = self.current();
        if tok.kind == TokenKind::Indent {
            self.report(codes::UNEXPECTED_INDENT, tok, codes::MSG_UNEXPECTED_INDENT);
            self.skip_block();
            return Ok(());
        }
        self.check_unclosed()?;
        let not_in_slice = |what: &str| codes::msg_not_in_v0_1(what);
        match tok.kind {
            TokenKind::Kw(Keyword::Fn) => match self.fn_decl() {
                Err(Stop::Line) => self.recover_head(),
                other => other,
            },
            TokenKind::Kw(
                kw @ (Keyword::App
                | Keyword::Data
                | Keyword::Enum
                | Keyword::Var
                | Keyword::View
                | Keyword::Style
                | Keyword::Extern),
            ) => {
                let what = format!("`{}` declaration", kw.as_str());
                Err(self.report(codes::NOT_IN_V0_1, tok, not_in_slice(&what)))
            }
            TokenKind::Kw(Keyword::Server) if self.nth(1) == TokenKind::Kw(Keyword::Fn) => {
                Err(self.report(codes::NOT_IN_V0_1, tok, not_in_slice("`server fn`")))
            }
            TokenKind::Ident
                if self.is_word(tok, "client") && self.nth(1) == TokenKind::Kw(Keyword::Fn) =>
            {
                Err(self.report(codes::NOT_IN_V0_1, tok, not_in_slice("`client fn`")))
            }
            _ => Err(self.expected("a declaration such as `fn main()`")),
        }
    }

    fn fn_decl(&mut self) -> Result<(), Stop> {
        let fn_tok = self.bump();
        let name = self.name("a function name after `fn`")?;
        if self.peek() != TokenKind::LParen {
            return Err(self.expected("`(` after the function name"));
        }
        let open = self.bump();
        self.enter(open)?;
        let params = self.params();
        self.leave();
        let params = params?;
        let result = if self.peek() == TokenKind::Arrow {
            self.bump();
            Some(self.type_ref()?)
        } else {
            None
        };
        if self.peek() == TokenKind::Kw(Keyword::As) {
            let at = self.current();
            return Err(self.report(
                codes::NOT_IN_V0_1,
                at,
                codes::msg_not_in_v0_1("`as system`"),
            ));
        }
        let what = format!("`{}`", name.text);
        self.head_end()?;
        let Some(body) = self.block_after_head(&format!("as the body of {what}"))? else {
            return Ok(());
        };
        let end = self
            .module
            .block(body)
            .map_or(self.prev_end(), |b| b.range.end());
        let decl = FnDecl {
            name,
            params: params.into_boxed_slice(),
            result,
            body,
            range: text_range(fn_tok.span.start, end),
        };
        let added = self.module.add_fn(decl);
        self.tree(added)
    }

    fn params(&mut self) -> Result<Vec<Param>, Stop> {
        let mut params = Vec::new();
        if self.peek() == TokenKind::RParen {
            self.bump();
            return Ok(params);
        }
        loop {
            let name = self.name("a parameter name")?;
            if self.peek() != TokenKind::Colon {
                let what = format!("`:` and a type after parameter `{}`", name.text);
                return Err(self.expected(&what));
            }
            self.bump();
            let ty = self.type_ref()?;
            params.push(Param::new(name, ty));
            match self.peek() {
                TokenKind::Comma => {
                    self.bump();
                }
                TokenKind::RParen => {
                    self.bump();
                    return Ok(params);
                }
                _ => return Err(self.expected("`,` or `)`")),
            }
        }
    }

    fn type_ref(&mut self) -> Result<TypeRef, Stop> {
        if self.peek() != TokenKind::Ident {
            return Err(self.expected("a type name"));
        }
        let tok = self.bump();
        let what = match self.peek() {
            TokenKind::Question => "an optional type",
            TokenKind::LBracket => "a type with type arguments",
            _ => return Ok(TypeRef::named(self.ident(tok))),
        };
        Err(self.report(codes::NOT_IN_V0_1, tok, codes::msg_not_in_v0_1(what)))
    }

    /// A name where the grammar wants one; a keyword is E0026.
    fn name(&mut self, what: &str) -> Result<Ident, Stop> {
        let tok = self.current();
        match tok.kind {
            TokenKind::Ident => {
                self.bump();
                Ok(self.ident(tok))
            }
            TokenKind::Kw(kw) => {
                Err(self.report(codes::KEYWORD_AS_NAME, tok, codes::msg_keyword_as_name(kw)))
            }
            _ => Err(self.expected(what)),
        }
    }

    // ----- blocks -----

    /// The end of a block head line: `NL`, with E0023 for a `:` and E0022 for
    /// a `{` before it.
    fn head_end(&mut self) -> Result<(), Stop> {
        let tok = self.current();
        match tok.kind {
            TokenKind::Nl => {
                self.bump();
                Ok(())
            }
            TokenKind::Colon => {
                self.bump();
                self.report(codes::COLON_BLOCK_HEAD, tok, codes::MSG_COLON_BLOCK_HEAD);
                self.end_after_reported()
            }
            TokenKind::LBrace => {
                self.bump();
                self.report(codes::BRACE_BLOCK, tok, codes::MSG_BRACE_BLOCK);
                let ended = self.end_after_reported();
                if ended.is_ok() {
                    self.braces = self.braces.saturating_add(1);
                }
                ended
            }
            _ => self.line_end(),
        }
    }

    /// After a reported `:` or `{`: the head is fine if the line ends here,
    /// otherwise the rest is skipped without a second diagnostic.
    fn end_after_reported(&mut self) -> Result<(), Stop> {
        if self.peek() == TokenKind::Nl {
            self.bump();
            Ok(())
        } else {
            Err(Stop::Line)
        }
    }

    /// The end of a statement line.
    fn line_end(&mut self) -> Result<(), Stop> {
        let tok = self.current();
        match tok.kind {
            TokenKind::Nl => {
                self.bump();
                Ok(())
            }
            TokenKind::Eof => Ok(()),
            TokenKind::Ident if self.is_word(tok, "is") => {
                Err(self.report(codes::IS_OPERATOR, tok, codes::MSG_IS_OPERATOR))
            }
            _ => Err(self.expected("end of line")),
        }
    }

    /// The indented block after a head line whose `NL` was consumed. A
    /// missing block is reported once; the next line is not touched.
    fn block_after_head(&mut self, what: &str) -> Result<Option<BlockId>, Stop> {
        if self.peek() == TokenKind::Indent {
            return self.block().map(Some);
        }
        self.expected(&format!("an indented block {what}"));
        Ok(None)
    }

    /// `INDENT { stmt } DEDENT`. Statement faults are handled inside; only a
    /// limit stops it.
    fn block(&mut self) -> Result<BlockId, Stop> {
        let indent = self.bump();
        self.enter(indent)?;
        let mut stmts = Vec::new();
        loop {
            let before = self.pos;
            match self.peek() {
                TokenKind::Dedent => {
                    self.bump();
                    break;
                }
                TokenKind::Eof => break,
                TokenKind::Nl => {
                    self.bump();
                }
                _ => match self.stmt() {
                    Ok(Some(stmt)) => stmts.push(stmt),
                    Ok(None) => {}
                    Err(Stop::Line) => self.recover(),
                    Err(Stop::File) => return Err(Stop::File),
                },
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.leave();
        let first = stmts.first().and_then(|s| self.module.stmt(*s));
        let last = stmts.last().and_then(|s| self.module.stmt(*s));
        let range = match (first, last) {
            (Some(first), Some(last)) => first.range.cover(last.range),
            _ => TextRange::empty(indent.span.end),
        };
        let added = self.module.add_block(stmts, range);
        self.tree(added)
    }

    // ----- statements -----

    /// One statement. `Ok(None)` means a fault was reported and recovered
    /// from; `Err(Stop::Line)` asks the caller to skip the line.
    fn stmt(&mut self) -> Result<Option<StmtId>, Stop> {
        if self.skip_closing_brace() {
            return Ok(None);
        }
        let tok = self.current();
        if tok.kind == TokenKind::Indent {
            self.report(codes::UNEXPECTED_INDENT, tok, codes::MSG_UNEXPECTED_INDENT);
            self.skip_block();
            return Ok(None);
        }
        self.check_unclosed()?;
        let not_in_slice = |what: &str| codes::msg_not_in_v0_1(what);
        match tok.kind {
            TokenKind::Kw(Keyword::Let) => self.let_stmt().map(Some),
            TokenKind::Kw(Keyword::If) => self.if_stmt(),
            TokenKind::Kw(Keyword::Return) => self.return_stmt().map(Some),
            TokenKind::Kw(Keyword::For) => {
                Err(self.report(codes::NOT_IN_V0_1, tok, not_in_slice("`for`")))
            }
            TokenKind::Kw(Keyword::Drop) => {
                Err(self.report(codes::NOT_IN_V0_1, tok, not_in_slice("`drop`")))
            }
            TokenKind::Kw(kw) if !starts_expr(kw) => Err(self.expected("a statement")),
            _ => self.expr_stmt().map(Some),
        }
    }

    fn let_stmt(&mut self) -> Result<StmtId, Stop> {
        self.check_assign_operator()?;
        let kw = self.bump();
        let name = self.name("a name after `let`")?;
        if self.peek() != TokenKind::Eq {
            return Err(self.expected(&format!("`=` after `let {}`", name.text)));
        }
        self.bump();
        let value = self.expr()?;
        let range = self.range_from(kw.span.start);
        self.line_end()?;
        let added = self.module.add_stmt(StmtKind::Let { name, value }, range);
        self.tree(added)
    }

    fn return_stmt(&mut self) -> Result<StmtId, Stop> {
        let kw = self.bump();
        if matches!(
            self.peek(),
            TokenKind::Nl | TokenKind::Eof | TokenKind::Dedent
        ) {
            return Err(self.expected("a value after `return`"));
        }
        let value = self.expr()?;
        let range = self.range_from(kw.span.start);
        self.line_end()?;
        let added = self.module.add_stmt(StmtKind::Return { value }, range);
        self.tree(added)
    }

    /// `if cond block [else block]`. A fault in a head line is reported and
    /// the blocks are still parsed; then no statement is built.
    fn if_stmt(&mut self) -> Result<Option<StmtId>, Stop> {
        let kw = self.bump();
        let head = self.if_head();
        let cond = match head {
            Ok(cond) => Some(cond),
            Err(Stop::File) => return Err(Stop::File),
            Err(Stop::Line) => {
                self.skip_line();
                None
            }
        };
        let then_block = self.block_after_head("after `if`")?;
        if self.peek() == TokenKind::RBrace
            && self.braces > 0
            && self.nth(1) == TokenKind::Kw(Keyword::Else)
        {
            // `} else {` after a block opened with `{` (E0022 reported).
            self.braces -= 1;
            self.bump();
        }
        let else_block = if self.peek() == TokenKind::Kw(Keyword::Else) {
            self.else_part()?
        } else {
            None
        };
        let (Some(cond), Some(then_block)) = (cond, then_block) else {
            return Ok(None);
        };
        let end = else_block
            .or(Some(then_block))
            .and_then(|b| self.module.block(b))
            .map_or(self.prev_end(), |b| b.range.end());
        let kind = StmtKind::If {
            cond,
            then_block,
            else_block,
        };
        let added = self.module.add_stmt(kind, text_range(kw.span.start, end));
        self.tree(added).map(Some)
    }

    fn if_head(&mut self) -> Result<ExprId, Stop> {
        let tok = self.current();
        if tok.kind == TokenKind::Kw(Keyword::Let) {
            return Err(self.report(codes::NOT_IN_V0_1, tok, codes::msg_not_in_v0_1("`if let`")));
        }
        let cond = self.expr()?;
        self.head_end()?;
        Ok(cond)
    }

    /// `else` and its block. `else if` on one line is E0024; the `if` is then
    /// read as the only statement of the `else` block, so nothing after it is
    /// reported again.
    fn else_part(&mut self) -> Result<Option<BlockId>, Stop> {
        let else_tok = self.bump();
        let tok = self.current();
        if tok.kind == TokenKind::Kw(Keyword::If) {
            self.report(codes::ELSE_IF_ONE_LINE, tok, codes::MSG_ELSE_IF_ONE_LINE);
            // The synthetic block counts as one nesting level, which also
            // bounds the recursion of a long `else if` chain.
            self.enter(else_tok)?;
            let inner = self.if_stmt();
            self.leave();
            let Some(stmt) = inner? else {
                return Ok(None);
            };
            let range = self
                .module
                .stmt(stmt)
                .map_or(TextRange::empty(tok.span.start), |s| s.range);
            let added = self.module.add_block(vec![stmt], range);
            return self.tree(added).map(Some);
        }
        match self.head_end() {
            Ok(()) => {}
            Err(Stop::File) => return Err(Stop::File),
            Err(Stop::Line) => self.skip_line(),
        }
        self.block_after_head("after `else`")
    }

    fn expr_stmt(&mut self) -> Result<StmtId, Stop> {
        let start = self.current().span.start;
        self.check_assign_operator()?;
        let value = self.expr()?;
        if self.peek() == TokenKind::Eq {
            let at = self.current();
            return Err(self.report(codes::NOT_IN_V0_1, at, codes::msg_not_in_v0_1("assignment")));
        }
        let range = self.range_from(start);
        self.line_end()?;
        let added = self.module.add_stmt(StmtKind::Expr(value), range);
        self.tree(added)
    }

    /// E0020 for `+=`, `-=`, `*=`, `/=`, `%=` and `:=` anywhere on the line,
    /// written as two adjacent tokens by the lexer.
    fn check_assign_operator(&mut self) -> Result<(), Stop> {
        let mut i = self.pos;
        while let (Some(a), Some(b)) = (self.toks.get(i).copied(), self.toks.get(i + 1).copied()) {
            self.step();
            if matches!(a.kind, TokenKind::Nl | TokenKind::Eof) {
                break;
            }
            if b.kind == TokenKind::Eq && a.span.end == b.span.start {
                let msg = match a.kind {
                    TokenKind::Plus
                    | TokenKind::Minus
                    | TokenKind::Star
                    | TokenKind::Slash
                    | TokenKind::Percent => Some(codes::MSG_COMPOUND_ASSIGN),
                    TokenKind::Colon => Some(codes::MSG_WALRUS_ASSIGN),
                    _ => None,
                };
                if let Some(msg) = msg {
                    return Err(self.report(codes::ASSIGN_OPERATOR, a, msg));
                }
            }
            i += 1;
        }
        Ok(())
    }

    /// Parses an expression through the [`ExprParse`] implementation.
    fn expr(&mut self) -> Result<ExprId, Stop> {
        self.check_unclosed()?;
        let exprs = self.exprs;
        exprs.expr(self)
    }
}

/// Keywords that can start an expression (SYNTAX 7 `primary`, `notExpr`).
fn starts_expr(kw: Keyword) -> bool {
    matches!(
        kw,
        Keyword::Not
            | Keyword::True
            | Keyword::False
            | Keyword::None
            | Keyword::Me
            | Keyword::Now
            | Keyword::Signed
            | Keyword::Make
    )
}

/// The range `start..end`, or `end..start` if the two are swapped.
pub fn text_range(start: u32, end: u32) -> TextRange {
    TextRange::empty(start).cover(TextRange::empty(end))
}

/// True if `at` lies in one of the sorted, disjoint ranges `start..end`.
fn in_ranges(ranges: &[(u32, u32)], at: u32) -> bool {
    let after = ranges.partition_point(|&(start, _)| start <= at);
    after
        .checked_sub(1)
        .and_then(|i| ranges.get(i))
        .is_some_and(|&(start, end)| start <= at && at < end)
}

/// Splits the tokens into logical lines and marks the lines the lexer has
/// reported.
///
/// A line ends with its `NL` (or `EOF`); the `INDENT` and `DEDENT` tokens
/// before its first token belong to it. Its byte range runs from the start
/// of the source line of its first token (so leading whitespace belongs to
/// it) to the end of its `NL`, end exclusive, so the next line never
/// overlaps it (the end of an `NL` is the start of the next line). Blank and
/// comment lines before it are not part of it. The last line also holds the
/// offset at the very end of the source. A line is faulty if a diagnostic in
/// `lexed` starts in its range.
fn scan_lines(src: &str, toks: &[Token], lexed: &[Diagnostic]) -> Lines {
    let mut lines = Lines {
        line_of: Vec::with_capacity(toks.len()),
        ..Lines::default()
    };
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    let mut start: Option<u32> = None;
    let mut open: Vec<u32> = Vec::new();
    for (index, tok) in toks.iter().enumerate() {
        let line = u32::try_from(lines.unclosed.len()).unwrap_or(u32::MAX);
        lines.line_of.push(line);
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        start = Some(start.map_or(tok.span.start, |s| s.min(tok.span.start)));
        match tok.kind {
            TokenKind::LParen => open.push(index),
            TokenKind::RParen => {
                open.pop();
            }
            _ => {}
        }
        if matches!(tok.kind, TokenKind::Nl | TokenKind::Eof) {
            lines.unclosed.push(open.first().copied());
            open.clear();
            let first = start.unwrap_or(tok.span.start);
            let prev = ranges.last().map_or(0, |&(_, end)| end);
            let from = src
                .get(prev.min(first) as usize..first as usize)
                .and_then(|before| before.rfind('\n'))
                .and_then(|i| u32::try_from(i + 1).ok())
                .map_or(prev.min(first), |i| prev.min(first) + i);
            // An empty `NL` (at the end of the source) or `EOF` also holds
            // its own offset.
            let to = if tok.span.is_empty() {
                tok.span.end.saturating_add(1)
            } else {
                tok.span.end
            };
            // Keep the ranges disjoint even for a malformed token stream.
            let from = ranges.last().map_or(from, |&(_, end)| from.max(end));
            if from < to {
                ranges.push((from, to));
            }
            start = None;
        }
    }
    let mut faulty: Vec<(u32, u32)> = Vec::new();
    for diag in lexed {
        let at = diag.span.start;
        let after = ranges.partition_point(|&(start, _)| start <= at);
        let hit = after
            .checked_sub(1)
            .and_then(|i| ranges.get(i))
            .filter(|&&(start, end)| start <= at && at < end);
        if let Some(&range) = hit {
            faulty.push(range);
        }
    }
    faulty.sort_unstable();
    faulty.dedup();
    let scanned = toks.len().saturating_add(lexed.len());
    lines.steps = u64::try_from(scanned).unwrap_or(u64::MAX);
    lines.faulty = faulty;
    lines
}
