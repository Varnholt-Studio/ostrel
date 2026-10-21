//! Parser (ARCHITECTURE 3.4, SYNTAX 7, WP T1-3, T119).
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
//! program = { decl } ;
//! block   = NL INDENT { stmt } DEDENT ;
//! stmt    = "let" name "=" expr NL
//!         | "if" cond block [ "else" block ]
//!         | "for" name "in" source block
//!         | "drop" source NL
//!         | "return" expr NL
//!         | expr [ "=" expr ] NL ;
//! cond    = "let" name "=" expr | expr ;
//! ```
//!
//! The declarations (`decl`), `source` and the function heads with their
//! `call` rule are read in [`items`]. `extern` (v0.3, D95) is reported once
//! with `E0100` and skipped, so the next item still parses. Which of the
//! parsed constructs a stage accepts is decided by that stage: the v0.1
//! checker reports `E0100 not available in v0.1` for everything outside the
//! slice (ARCHITECTURE 3.4).
//!
//! # Recovery
//!
//! One fault gives one diagnostic (SPEC 12.1):
//!
//! * A fault in a statement or a line of a declaration skips the rest of its
//!   logical line. An indented block below a line that no known block head
//!   opened is skipped without a further diagnostic.
//! * A fault in a block head (`fn`, `if`, `else`, `for`, `data`, `view`, an
//!   element line and so on) skips the rest of the head line, but the block
//!   below it is still parsed, so faults inside the block are reported and
//!   nothing in it is reported twice.
//! * A logical line on which a lexer diagnostic starts has already been
//!   reported. The parser never reports a second diagnostic on such a line.
//!   The lines are found from the spans of the lexer diagnostics passed to
//!   [`parse_with`], never from the shape of the tokens, so every fault the
//!   lexer reports (an `Error` token, a tab, a jump of indentation and so on)
//!   is covered the same way.
//! * Bracket faults of a logical line give one diagnostic, the earliest,
//!   before anything else on that line (D166): E0025 at a `(`, `[` or `{`
//!   still open at the line end or closed over by a closer of another kind,
//!   E0028 at a closer with no open bracket of its kind. A closer closes the
//!   nearest open bracket of its own kind and every bracket above it, as in
//!   the lexer. On the directly following logical line a closer of a bracket
//!   left open (and so already reported) is dropped without a diagnostic.
//!   The lexer ends a continuation at a line that is not deeper, so an open
//!   bracket never takes the rest of the file.
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
mod expr;
mod items;

#[cfg(test)]
mod tests;

pub use expr::{Exprs, parse};

use crate::ast::{
    AstError, BlockId, CallRule, Cond, ExprId, ExprKind, Ident, Limits, Module, StmtId, StmtKind,
    TextRange,
};
use crate::lex::{self, Keyword, Token, TokenKind};
use ostrel_core::{Code, Diagnostic, FileId, Span};
use std::borrow::Cow;

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
/// * The statement parser has already reported a `(`, `[` or `{` that is not
///   closed on the logical line (E0025), so the expression parser always
///   finds a closer for every bracket before the end of the line.
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
    /// Token indexes, ascending, of the closers of brackets left open on the
    /// logical line before (D166); the parser never sees them.
    silent: Vec<u32>,
}

impl Lines {
    /// Removes the tokens `silent` from the per token facts and renumbers
    /// the token indexes after them, like [`Tokens::drop_kept`].
    fn drop_silent(&mut self) {
        if self.silent.is_empty() {
            return;
        }
        let silent = std::mem::take(&mut self.silent);
        let mut next = silent.iter().peekable();
        let mut index = 0u32;
        self.line_of.retain(|_| {
            let keep = next.next_if(|&&s| s == index).is_none();
            index = index.saturating_add(1);
            keep
        });
        let shift = |at: u32| {
            let before = silent.partition_point(|&s| s < at);
            at.saturating_sub(u32::try_from(before).unwrap_or(u32::MAX))
        };
        for fault in self.unclosed.iter_mut().flatten() {
            *fault = shift(*fault);
        }
    }
}

/// Parser state. [`ExprParse`] implementations use its public methods.
pub struct Parser<'a> {
    src: &'a str,
    file: FileId,
    exprs: &'a dyn ExprParse,
    toks: Tokens<'a>,
    /// Logical line of every token, as an index into `unclosed`.
    line_of: Vec<u32>,
    /// Per logical line: token index of its earliest bracket fault (D166),
    /// an unclosed opener or a stray closer.
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
        tokens: &'a [Token],
        lexed: &[Diagnostic],
        limits: ParseLimits,
        exprs: &'a dyn ExprParse,
    ) -> Parser<'a> {
        let mut toks = Tokens::new(src, file, tokens);
        let mut lines = scan_lines(src, &toks, lexed);
        toks.drop_kept(&lines.silent);
        lines.drop_silent();
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
        let last = self.toks.last().unwrap_or(eof);
        self.pos
            .checked_add(n)
            .and_then(|i| self.toks.get(i))
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

    /// Reports the earliest bracket fault of the rest of the current logical
    /// line, if any (D166): E0025 at a `(`, `[` or `{` without its closer,
    /// E0028 at a closer with no open bracket of its kind.
    fn check_unclosed(&mut self) -> Result<(), Stop> {
        match self.unclosed_on_line(self.pos) {
            Some(index) if index as usize >= self.pos => {
                let at = self.toks.get(index as usize).unwrap_or(self.current());
                Err(match lex::bracket_of(at.kind) {
                    Some((_, false)) => self.report(codes::EXPECTED, at, codes::msg_stray(at.kind)),
                    _ => self.report(codes::UNCLOSED_PAREN, at, codes::msg_unclosed(at.kind)),
                })
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
        match tok.kind {
            TokenKind::Kw(Keyword::Fn) => self.fn_decl(),
            TokenKind::Kw(Keyword::Server) if self.nth(1) == TokenKind::Kw(Keyword::Fn) => {
                self.fn_decl()
            }
            TokenKind::Ident
                if self.is_word(tok, "client") && self.nth(1) == TokenKind::Kw(Keyword::Fn) =>
            {
                self.fn_decl()
            }
            TokenKind::Kw(Keyword::App) => self.app_decl(),
            TokenKind::Kw(Keyword::Enum) => self.enum_decl(),
            TokenKind::Kw(Keyword::Data) => self.data_decl(),
            TokenKind::Kw(Keyword::Var) => self.var_decl(),
            TokenKind::Kw(Keyword::View) => self.view_decl(),
            TokenKind::Kw(Keyword::Style) => self.style_decl(),
            TokenKind::Kw(Keyword::Extern) => {
                let what = codes::msg_not_in_v0_1("`extern` declaration");
                Err(self.report(codes::NOT_IN_V0_1, tok, what))
            }
            _ => Err(self.expected("a declaration such as `fn main()`")),
        }
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
        self.block_with(None)
    }

    /// [`Parser::block`] that, when `call` is given, reads a `call` rule on
    /// the first line of the block into it (a function body, SYNTAX 7
    /// `fnDecl`). The rule is not a statement of the block.
    fn block_with(&mut self, mut call: Option<&mut Option<CallRule>>) -> Result<BlockId, Stop> {
        let indent = self.bump();
        self.enter(indent)?;
        let mut stmts = Vec::new();
        let mut first = true;
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
                kind => {
                    let result = match call.as_deref_mut() {
                        Some(slot) if first && kind == TokenKind::Kw(Keyword::Call) => {
                            self.call_rule().map(|rule| {
                                *slot = Some(rule);
                                None
                            })
                        }
                        _ => self.stmt(),
                    };
                    first = false;
                    match result {
                        Ok(Some(stmt)) => stmts.push(stmt),
                        Ok(None) => {}
                        Err(Stop::Line) => self.recover(),
                        Err(Stop::File) => return Err(Stop::File),
                    }
                }
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
        match tok.kind {
            TokenKind::Kw(Keyword::Let) => self.let_stmt().map(Some),
            TokenKind::Kw(Keyword::If) => self.if_stmt(),
            TokenKind::Kw(Keyword::Return) => self.return_stmt().map(Some),
            TokenKind::Kw(Keyword::For) => self.for_stmt(),
            TokenKind::Kw(Keyword::Drop) => self.drop_stmt().map(Some),
            TokenKind::Kw(Keyword::Call) => Err(self.expected(
                "a statement (a `call` rule stands only on the first line of a function body)",
            )),
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

    /// `if cond block [else block]`, where `cond` is an expression or
    /// `let name = value`. A fault in a head line is reported and the blocks
    /// are still parsed; then no statement is built.
    fn if_stmt(&mut self) -> Result<Option<StmtId>, Stop> {
        let kw = self.bump();
        let cond = self.head(Parser::cond)?;
        let then_block = self.body_after(cond.is_some(), "after `if`")?;
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
        let kind = match cond {
            Cond::Expr(cond) => StmtKind::If {
                cond,
                then_block,
                else_block,
            },
            Cond::Let { name, value } => StmtKind::IfLet {
                name,
                value,
                then_block,
                else_block,
            },
        };
        let added = self.module.add_stmt(kind, text_range(kw.span.start, end));
        self.tree(added).map(Some)
    }

    /// Reads the rest of a block head with `read` and its line end. A fault
    /// is reported and the rest of the line skipped; then `None` comes back,
    /// so the caller still parses the block below the head.
    fn head<T>(&mut self, read: fn(&mut Parser<'a>) -> Result<T, Stop>) -> Result<Option<T>, Stop> {
        let head = read(self).and_then(|value| self.head_end().map(|()| value));
        match head {
            Ok(value) => Ok(Some(value)),
            Err(Stop::File) => Err(Stop::File),
            Err(Stop::Line) => {
                self.skip_line();
                Ok(None)
            }
        }
    }

    /// The block after a head line. A missing block is reported once, but
    /// only after a head without fault (one fault, one diagnostic).
    fn body_after(&mut self, head_ok: bool, what: &str) -> Result<Option<BlockId>, Stop> {
        if !head_ok && self.peek() != TokenKind::Indent {
            return Ok(None);
        }
        self.block_after_head(what)
    }

    /// `cond = "let" name "=" expr | expr`, the condition of an `if` in a
    /// function body or a view.
    fn cond(&mut self) -> Result<Cond, Stop> {
        if self.peek() != TokenKind::Kw(Keyword::Let) {
            return self.expr().map(Cond::Expr);
        }
        self.check_assign_operator()?;
        self.bump();
        let name = self.name("a name after `if let`")?;
        if self.peek() != TokenKind::Eq {
            return Err(self.expected(&format!("`=` after `if let {}`", name.text)));
        }
        self.bump();
        let value = self.expr()?;
        Ok(Cond::Let { name, value })
    }

    /// `for name in source block`. Like `if`, a faulty head still parses the
    /// block.
    fn for_stmt(&mut self) -> Result<Option<StmtId>, Stop> {
        let kw = self.bump();
        let head = self.head(Parser::for_head)?;
        let body = self.body_after(head.is_some(), "as the body of `for`")?;
        let (Some((name, source)), Some(body)) = (head, body) else {
            return Ok(None);
        };
        let end = self
            .module
            .block(body)
            .map_or(self.prev_end(), |b| b.range.end());
        let kind = StmtKind::For { name, source, body };
        let added = self.module.add_stmt(kind, text_range(kw.span.start, end));
        self.tree(added).map(Some)
    }

    /// `name "in" source` after `for`, in a function body or a view.
    fn for_head(&mut self) -> Result<(Ident, ExprId), Stop> {
        let name = self.name("a loop variable after `for`")?;
        if self.peek() != TokenKind::Kw(Keyword::In) {
            return Err(self.expected("`in` after the loop variable (Ostrel writes `for x in xs`)"));
        }
        self.bump();
        let source = self.source()?;
        Ok((name, source))
    }

    /// `drop source`: one record or every record of a query.
    fn drop_stmt(&mut self) -> Result<StmtId, Stop> {
        let kw = self.bump();
        if matches!(
            self.peek(),
            TokenKind::Nl | TokenKind::Eof | TokenKind::Dedent
        ) {
            return Err(self.expected("a record or a query after `drop`"));
        }
        let source = self.source()?;
        let range = self.range_from(kw.span.start);
        self.line_end()?;
        let added = self.module.add_stmt(StmtKind::Drop { source }, range);
        self.tree(added)
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
        let head_ok = match self.head_end() {
            Ok(()) => true,
            Err(Stop::File) => return Err(Stop::File),
            Err(Stop::Line) => {
                self.skip_line();
                false
            }
        };
        self.body_after(head_ok, "after `else`")
    }

    /// `expr [ "=" expr ]`: an expression statement or an assignment. Which
    /// targets are assignable is checked in sema (SYNTAX 7).
    fn expr_stmt(&mut self) -> Result<StmtId, Stop> {
        let start = self.current().span.start;
        self.check_assign_operator()?;
        let value = self.expr()?;
        let kind = if self.peek() == TokenKind::Eq {
            self.bump();
            let new = self.expr()?;
            StmtKind::Assign {
                target: value,
                value: new,
            }
        } else {
            StmtKind::Expr(value)
        };
        let range = self.range_from(start);
        self.line_end()?;
        let added = self.module.add_stmt(kind, range);
        self.tree(added)
    }

    /// E0020 for `+=`, `-=`, `*=`, `/=`, `%=` and `:=` anywhere on the line,
    /// written as two adjacent tokens by the lexer.
    fn check_assign_operator(&mut self) -> Result<(), Stop> {
        let mut i = self.pos;
        while let (Some(a), Some(b)) = (self.toks.get(i), self.toks.get(i + 1)) {
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

/// The tokens the parser reads: every token except trivia, ending with `EOF`.
///
/// The token stream of the lexer is borrowed, never copied, because a hostile
/// file can have millions of tokens (AC-04, memory limit of
/// `tests/hostile/README`). If it has trivia, only the indexes of the other
/// tokens are kept, a quarter of the size of a copy. Only a stream without a
/// final `EOF`, which the lexer never produces, is copied to append one.
struct Tokens<'a> {
    all: Cow<'a, [Token]>,
    /// Indexes into `all` of the tokens that are not trivia, or `None` if
    /// `all` has no trivia.
    kept: Option<Vec<u32>>,
}

impl<'a> Tokens<'a> {
    fn new(src: &str, file: FileId, tokens: &'a [Token]) -> Tokens<'a> {
        let has_eof = tokens
            .iter()
            .rev()
            .find(|t| !t.is_trivia())
            .is_some_and(|t| t.kind == TokenKind::Eof);
        let all = if has_eof {
            Cow::Borrowed(tokens)
        } else {
            let end = u32::try_from(src.len()).unwrap_or(u32::MAX);
            let mut owned = tokens.to_vec();
            owned.push(Token::new(TokenKind::Eof, Span::point(file, end)));
            Cow::Owned(owned)
        };
        let kept = all.iter().any(|t| t.is_trivia()).then(|| {
            all.iter()
                .enumerate()
                .filter(|(_, t)| !t.is_trivia())
                .filter_map(|(i, _)| u32::try_from(i).ok())
                .collect()
        });
        Tokens { all, kept }
    }

    fn len(&self) -> usize {
        self.kept.as_ref().map_or(self.all.len(), Vec::len)
    }

    fn get(&self, index: usize) -> Option<Token> {
        match &self.kept {
            None => self.all.get(index).copied(),
            Some(kept) => kept
                .get(index)
                .and_then(|&i| self.all.get(i as usize))
                .copied(),
        }
    }

    fn last(&self) -> Option<Token> {
        self.len().checked_sub(1).and_then(|i| self.get(i))
    }

    fn iter(&self) -> impl Iterator<Item = Token> + '_ {
        (0..self.len()).filter_map(|i| self.get(i))
    }

    /// Removes the tokens at the ascending indexes `drop` (indexes into this
    /// sequence, not into `all`).
    fn drop_kept(&mut self, drop: &[u32]) {
        if drop.is_empty() {
            return;
        }
        let mut next = drop.iter().peekable();
        let kept: Vec<u32> = match &self.kept {
            Some(kept) => kept.clone(),
            None => (0..self.all.len())
                .filter_map(|i| u32::try_from(i).ok())
                .collect(),
        };
        let mut index = 0u32;
        let kept = kept
            .into_iter()
            .filter(|_| {
                let keep = next.next_if(|&&d| d == index).is_none();
                index = index.saturating_add(1);
                keep
            })
            .collect();
        self.kept = Some(kept);
    }
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
fn scan_lines(src: &str, toks: &Tokens<'_>, lexed: &[Diagnostic]) -> Lines {
    let mut lines = Lines {
        line_of: Vec::with_capacity(toks.len()),
        ..Lines::default()
    };
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    let mut start: Option<u32> = None;
    // The open brackets of the logical line, the earliest bracket fault on
    // it, the kinds left open on the line before (outermost first) and the
    // `{` on it that open no level (D166).
    let mut open: lex::Brackets<u32> = lex::Brackets::new();
    let mut fault: Option<u32> = None;
    let mut rest: Vec<usize> = Vec::new();
    let mut quiet_braces: u32 = 0;
    // Stack lengths at the start of the open interpolations.
    let mut floors: Vec<usize> = Vec::new();
    // The last two code tokens of the logical line, for `brace_opens_level`.
    let mut before: (Option<TokenKind>, Option<TokenKind>) = (None, None);
    for (index, tok) in toks.iter().enumerate() {
        let line = u32::try_from(lines.unclosed.len()).unwrap_or(u32::MAX);
        lines.line_of.push(line);
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        start = Some(start.map_or(tok.span.start, |s| s.min(tok.span.start)));
        // One stack of kinds with the closing rule of the lexer (D166). A `{`
        // that is not in operand position opens nothing (it is E0022 at a
        // block head), and a `}` on its line is not a stray closer.
        let mut note = |at: u32| fault = Some(fault.map_or(at, |f| f.min(at)));
        match lex::bracket_of(tok.kind) {
            Some((2, true)) if !lex::brace_opens_level(before.0, before.1) => {
                quiet_braces = quiet_braces.saturating_add(1);
            }
            Some((kind, true)) => open.push(kind, index),
            Some((kind, false)) => match open.close(kind) {
                Some(over) => {
                    if let Some(at) = over {
                        note(at);
                    }
                }
                None if kind == 2 && quiet_braces > 0 => quiet_braces -= 1,
                // The closer of a bracket left open on the line before, which
                // has been reported there (SPEC 12.1).
                None if rest.last() == Some(&kind) => {
                    rest.pop();
                    lines.silent.push(index);
                }
                None => note(index),
            },
            None => {}
        }
        // An interpolation is closed in itself, as in the lexer: a closer in
        // it never closes a bracket opened before the string, and a bracket
        // it leaves open is unclosed at the end of the string.
        match tok.kind {
            TokenKind::StrHead => {
                open.floor();
                floors.push(open.len());
            }
            TokenKind::StrTail => {
                let floor = floors.pop().unwrap_or(0);
                if let Some(at) = open.get(floor) {
                    note(at);
                }
                open.unfloor();
                open.truncate(floor);
            }
            _ => {}
        }
        if !lex::is_not_code(tok.kind) {
            before = (Some(tok.kind), before.0);
        }
        if matches!(tok.kind, TokenKind::Nl | TokenKind::Eof) {
            if let Some(at) = open.first() {
                fault = Some(fault.map_or(at, |f| f.min(at)));
            }
            lines.unclosed.push(fault.take());
            rest = open.take_kinds();
            quiet_braces = 0;
            floors.clear();
            before = (None, None);
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
