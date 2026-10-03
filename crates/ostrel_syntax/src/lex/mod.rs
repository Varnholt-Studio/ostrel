//! Lexer for the v0.1 token set (ARCHITECTURE 3, WP T1-2).
//!
//! The lexer turns source text into [`Token`]s and never fails: every fault is a
//! [`Diagnostic`] plus, where source is dropped, a [`TokenKind::Error`] token.
//! Comments stay in the stream as trivia for the formatter.
//!
//! # Layout
//!
//! * Indentation is two spaces per level (SYNTAX 3, P1). The lexer keeps a
//!   stack of indentation columns, starting with column 0 (ARCHITECTURE 3.5,
//!   D68). It emits `Nl` at the end of every logical line. A line deeper than
//!   the top of the stack opens exactly one block: one `Indent`, its column is
//!   pushed. A line more than two columns deeper is E0013; it still opens one
//!   block with its real column, so the following lines of that block get no
//!   further diagnostic. A shallower line pops columns with one `Dedent` each
//!   until the top is not deeper than the line; a column that is not on the
//!   stack (only possible after a recovery) closes to the next outer level
//!   without a second diagnostic.
//! * A tab in the indentation is E0001 and an odd column E0002. Recovery
//!   counts a tab as two columns and rounds an odd column up, then applies the
//!   stack rules above without a further diagnostic.
//! * Blank lines and lines holding only a comment produce no `Nl`, `Indent` or
//!   `Dedent`, and their leading whitespace is not checked (SPEC 12.2).
//! * While a `(` is open, line ends and indentation are ignored. From v0.2 `[`
//!   and `{` continue lines as well (SPEC 12.2); see [`continues_line`].
//! * At the end of the input the lexer closes the last line with `Nl`, emits a
//!   `Dedent` for every open block and ends with `Eof`. A last line without a
//!   line end is valid, and an empty input gives only `Eof`.
//!
//! # Line ends and code points
//!
//! * `\r\n` is a line end like `\n`; mixed line ends are allowed. A lone
//!   carriage return is E0008 in code and in comments (in string text the
//!   literal scanner reports it).
//! * Exactly one U+FEFF at byte 0 is skipped. Anywhere else it is E0008 in code
//!   and allowed in comments and string text.
//! * Raw bidirectional control characters are E0011 everywhere (D53).
//!
//! # Split with the literal scanner
//!
//! The driver in this module owns layout, comments, names, keywords,
//! punctuation, the structure of string literals and the interpolation brace
//! count. The contents of integer literals and of string text (escapes, the
//! `Int` range, characters allowed in text) belong to the literal scanner
//! behind [`LiteralScan`] (WP T1-2b, `literal.rs`).
//!
//! # Recovery
//!
//! One fault gives one diagnostic (SPEC 12.1). Later stages never report a
//! diagnostic for an `Error` token. A `;` is reported once and then acts as a
//! line end, so the statements on both sides still parse.

pub mod codes;
mod literal;
mod token;

#[cfg(test)]
mod tests;

pub use token::{Keyword, Token, TokenKind};

use ostrel_core::{Diagnostic, FileId, Span};

/// Where a piece of string text ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextStop {
    /// After the closing `"`; the string is complete.
    Quote,
    /// After an unescaped `{` that opens an interpolation.
    Brace,
    /// Before a line end or at the end of the input; the string is
    /// unterminated. The line end is not consumed.
    LineEnd,
}

/// Scanner for the contents of literals, implemented by WP T1-2b.
///
/// Contract between the driver and the scanner:
///
/// * Offsets are byte offsets into `src` and always on character boundaries.
/// * The scanner reports faults inside the literal (E0004, E0005, E0010 and
///   D53 characters inside text) to `diags`, with spans in `file`, and keeps
///   going. It never reports E0003, E0006 or E0007; those are structural and
///   belong to the driver.
/// * A returned end that breaks this contract (behind `start`, past the end of
///   `src`, inside a character, or not after the `"` or `{` it claims) is
///   treated by the driver as an unterminated string at the next line end, or
///   as an integer of only ASCII digits. The driver never loops or panics on a
///   faulty scanner.
pub trait LiteralScan {
    /// Scans an integer literal. `src[start..]` begins with an ASCII digit.
    ///
    /// Returns the end offset, after at least that digit. A literal outside the
    /// `Int` range (G9) is reported as [`codes::INT_OUT_OF_RANGE`] at `start`
    /// and still becomes one `Int` token.
    fn int(&self, src: &str, start: usize, file: FileId, diags: &mut Vec<Diagnostic>) -> usize;

    /// Scans string text starting at `start`, which is just after the opening
    /// `"` or just after the `}` that closes an interpolation.
    ///
    /// Stops after the first unescaped `"` ([`TextStop::Quote`]), after the
    /// first unescaped `{` ([`TextStop::Brace`]), or before a line end (`\n` or
    /// `\r\n`) or the end of the input ([`TextStop::LineEnd`]). A `\` never
    /// consumes a line end. An escape is consumed whole: the braces of
    /// `\u{...}`, `\{` and `\}` never open or close an interpolation.
    fn string_text(
        &self,
        src: &str,
        start: usize,
        file: FileId,
        diags: &mut Vec<Diagnostic>,
    ) -> (usize, TextStop);
}

/// Lexes `src` with the literal scanner of this crate.
///
/// The token stream always ends with exactly one [`TokenKind::Eof`] token, and
/// every `Indent` has a matching `Dedent` before it.
pub fn lex(src: &str, file: FileId) -> (Vec<Token>, Vec<Diagnostic>) {
    lex_with(src, file, &literal::Literals)
}

/// Lexes `src` with the given literal scanner.
///
/// The token stream always ends with exactly one [`TokenKind::Eof`] token, and
/// every `Indent` has a matching `Dedent` before it.
pub fn lex_with<L: LiteralScan + ?Sized>(
    src: &str,
    file: FileId,
    literals: &L,
) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lexer = Lexer {
        src,
        file,
        literals,
        pos: 0,
        tokens: Vec::new(),
        diags: Vec::new(),
        indents: vec![0],
        open: 0,
        line_has_tokens: false,
        at_line_start: true,
        in_string: false,
    };
    lexer.run();
    (lexer.tokens, lexer.diags)
}

/// True if an open `bracket` lets a logical line continue over a line end.
///
/// v0.1: only `(` (SPEC 12.2). From v0.2 `[` and `{` also continue a line.
pub const fn continues_line(bracket: char) -> bool {
    matches!(bracket, '(')
}

struct Lexer<'a, L: ?Sized> {
    src: &'a str,
    file: FileId,
    literals: &'a L,
    pos: usize,
    tokens: Vec<Token>,
    diags: Vec<Diagnostic>,
    /// Stack of indentation columns of the open blocks. The first entry is
    /// column 0 and is never popped.
    indents: Vec<u32>,
    /// Number of open brackets that continue a line.
    open: u32,
    /// The current logical line has a token other than trivia.
    line_has_tokens: bool,
    /// The next character is the first of a physical line outside brackets.
    at_line_start: bool,
    /// The lexer is inside an interpolation.
    in_string: bool,
}

impl<L: LiteralScan + ?Sized> Lexer<'_, L> {
    fn run(&mut self) {
        if self.src.starts_with('\u{FEFF}') {
            // Exactly one byte order mark at byte 0 is skipped (D68). A second
            // one is an unexpected character like any U+FEFF in code.
            self.pos = '\u{FEFF}'.len_utf8();
        }
        loop {
            if self.at_line_start && self.open == 0 {
                self.line_start();
            }
            let Some(c) = self.peek() else { break };
            match c {
                '\n' => self.line_end(1),
                '\r' if self.peek_at(1) == Some('\n') => self.line_end(2),
                ' ' | '\t' => self.pos += 1,
                _ => self.token(c),
            }
        }
        let end = self.src.len();
        if self.line_has_tokens {
            self.push(TokenKind::Nl, end, end);
        }
        while self.indents.len() > 1 {
            self.indents.pop();
            self.push(TokenKind::Dedent, end, end);
        }
        self.push(TokenKind::Eof, end, end);
    }

    // ----- layout -----

    /// Handles the indentation of a physical line outside brackets.
    fn line_start(&mut self) {
        self.at_line_start = false;
        let start = self.pos;
        let mut width: u32 = 0;
        let mut first_tab = None;
        while let Some(c) = self.peek() {
            match c {
                ' ' => width = width.saturating_add(1),
                '\t' => {
                    first_tab.get_or_insert(self.pos);
                    // Recovery only: a tab stands for one level.
                    width = width.saturating_add(2);
                }
                _ => break,
            }
            self.pos += 1;
        }
        let rest = self.src.get(self.pos..).unwrap_or("");
        if rest.is_empty()
            || rest.starts_with('\n')
            || rest.starts_with("\r\n")
            || rest.starts_with("//")
        {
            // Blank or comment only line: no layout tokens, nothing checked.
            return;
        }
        let ws_end = self.pos;
        let faulty = if let Some(tab) = first_tab {
            self.error(codes::TAB_INDENT, tab, tab + 1, codes::MSG_TAB_INDENT);
            true
        } else if !width.is_multiple_of(2) {
            self.error(codes::ODD_INDENT, start, ws_end, codes::MSG_ODD_INDENT);
            true
        } else {
            false
        };
        // Recovery for E0001 and E0002: the column rounded up to a multiple of
        // two. The stack rules below then hold without a second diagnostic.
        let column = if faulty {
            width.div_ceil(2).saturating_mul(2)
        } else {
            width
        };
        let top = self.top();
        if column > top {
            if !faulty && column - top > 2 {
                self.error(codes::DEEP_INDENT, start, ws_end, codes::MSG_DEEP_INDENT);
            }
            // Exactly one block opens, with the real column, so the following
            // lines of that block are not reported again (D68).
            self.push(TokenKind::Indent, start, ws_end);
            self.indents.push(column);
            return;
        }
        while self.top() > column {
            self.indents.pop();
            self.push(TokenKind::Dedent, ws_end, ws_end);
        }
        // A column between two stack entries can only follow a recovery; the
        // line belongs to the block that is now on top, without a diagnostic.
    }

    /// The column of the innermost open block.
    fn top(&self) -> u32 {
        self.indents.last().copied().unwrap_or(0)
    }

    /// Handles a line end of `len` bytes at the current position.
    fn line_end(&mut self, len: usize) {
        let start = self.pos;
        self.pos += len;
        if self.open == 0 {
            if self.line_has_tokens {
                self.push(TokenKind::Nl, start, self.pos);
            }
            self.line_has_tokens = false;
            self.at_line_start = true;
        }
    }

    // ----- tokens -----

    /// Lexes one token that starts with `c`, which is not a line end or blank.
    fn token(&mut self, c: char) {
        let start = self.pos;
        let single = |kind| (kind, 1);
        let (kind, len) = match c {
            '"' => return self.string(),
            '/' if self.peek_at(1) == Some('/') => return self.comment(),
            '0'..='9' => return self.int(),
            'a'..='z' | 'A'..='Z' => return self.ident(),
            '(' => {
                if continues_line('(') {
                    self.open = self.open.saturating_add(1);
                }
                single(TokenKind::LParen)
            }
            ')' => {
                if continues_line('(') {
                    self.open = self.open.saturating_sub(1);
                }
                single(TokenKind::RParen)
            }
            '[' => single(TokenKind::LBracket),
            ']' => single(TokenKind::RBracket),
            '{' => single(TokenKind::LBrace),
            '}' => single(TokenKind::RBrace),
            ',' => single(TokenKind::Comma),
            ':' => single(TokenKind::Colon),
            '|' => single(TokenKind::Pipe),
            '~' => single(TokenKind::Tilde),
            '+' => single(TokenKind::Plus),
            '*' => single(TokenKind::Star),
            '/' => single(TokenKind::Slash),
            '%' => single(TokenKind::Percent),
            '.' => self.pair('.', TokenKind::DotDot, TokenKind::Dot),
            '?' => self.pair('?', TokenKind::QuestionQuestion, TokenKind::Question),
            '-' => self.pair('>', TokenKind::Arrow, TokenKind::Minus),
            '=' => self.pair('=', TokenKind::EqEq, TokenKind::Eq),
            '<' => self.pair('=', TokenKind::Le, TokenKind::Lt),
            '>' => self.pair('=', TokenKind::Ge, TokenKind::Gt),
            '!' if self.peek_at(1) == Some('=') => (TokenKind::BangEq, 2),
            ';' => {
                self.error(codes::SEMICOLON, start, start + 1, codes::MSG_SEMICOLON);
                // Recovery: the `;` separates statements like a line end.
                self.pos += 1;
                if self.open == 0 && self.line_has_tokens && !self.in_string {
                    self.push(TokenKind::Nl, start, self.pos);
                    self.line_has_tokens = false;
                }
                return;
            }
            c if codes::is_bidi_control(c) => {
                self.bidi(c);
                self.pos += c.len_utf8();
                self.push_code(TokenKind::Error, start, self.pos);
                return;
            }
            c => return self.unexpected(c),
        };
        self.pos += len;
        self.push_code(kind, start, self.pos);
    }

    /// One or two character token: `two` if the next character is `next`.
    fn pair(&self, next: char, two: TokenKind, one: TokenKind) -> (TokenKind, usize) {
        if self.peek_at(1) == Some(next) {
            (two, 2)
        } else {
            (one, 1)
        }
    }

    fn ident(&mut self) {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = self.src.get(start..self.pos).unwrap_or("");
        let kind = match Keyword::from_text(text) {
            Some(kw) => TokenKind::Kw(kw),
            None => TokenKind::Ident,
        };
        self.push_code(kind, start, self.pos);
    }

    fn int(&mut self) {
        let start = self.pos;
        let end = self
            .literals
            .int(self.src, start, self.file, &mut self.diags);
        let end = if self.valid_end(start, end) && end > start {
            end
        } else {
            self.digits_end(start)
        };
        self.pos = end;
        self.push_code(TokenKind::Int, start, end);
    }

    fn comment(&mut self) {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == '\n' || (c == '\r' && self.peek_at(1) == Some('\n')) {
                break;
            }
            if codes::is_bidi_control(c) {
                self.bidi(c);
            } else if c == '\r' {
                // A lone carriage return is not a line end (D68). Some editors
                // show it as one, so the rest of the comment could pass for code.
                let at = self.pos;
                self.error(
                    codes::UNEXPECTED_CHAR,
                    at,
                    at + 1,
                    codes::msg_unexpected_char(c),
                );
            }
            self.pos += c.len_utf8();
        }
        self.push(TokenKind::Comment, start, self.pos);
    }

    /// A run of characters that cannot start a token, reported once at its first
    /// character. The run also takes the name characters that follow, so
    /// `grüße` gives one diagnostic, not one per character.
    fn unexpected(&mut self, first: char) {
        let start = self.pos;
        self.pos += first.len_utf8();
        while let Some(c) = self.peek() {
            let absorb = c.is_ascii_alphanumeric()
                || c == '_'
                || (!c.is_ascii() && !c.is_whitespace() && !codes::is_bidi_control(c))
                || matches!(c, '@' | '#' | '$' | '&' | '^' | '`' | '\\' | '\'');
            if !absorb {
                break;
            }
            self.pos += c.len_utf8();
        }
        self.error(
            codes::UNEXPECTED_CHAR,
            start,
            start + first.len_utf8(),
            codes::msg_unexpected_char(first),
        );
        self.push_code(TokenKind::Error, start, self.pos);
    }

    // ----- strings -----

    /// Lexes a string literal starting at its opening quote (G1).
    fn string(&mut self) {
        self.in_string = true;
        self.string_inner();
        self.in_string = false;
    }

    fn string_inner(&mut self) {
        let quote = self.pos;
        let first_token = self.tokens.len();
        let open_before = self.open;
        self.line_has_tokens = true;
        let (end, stop) = self.text(quote + 1);
        self.pos = end;
        match stop {
            TextStop::Quote => return self.push(TokenKind::Str, quote, end),
            TextStop::LineEnd => {
                let open = self.open;
                return self.cut_text(quote, first_token, open);
            }
            TextStop::Brace => self.push(TokenKind::StrHead, quote, end),
        }
        let mut depth: u32 = 1;
        loop {
            let Some(c) = self.peek() else {
                return self.unterminated_in(quote, first_token, open_before);
            };
            match c {
                ' ' | '\t' => self.pos += 1,
                '\n' => return self.unterminated_in(quote, first_token, open_before),
                '\r' if self.peek_at(1) == Some('\n') => {
                    return self.unterminated_in(quote, first_token, open_before);
                }
                '"' => {
                    let at = self.pos;
                    self.error(
                        codes::STRING_IN_INTERPOLATION,
                        at,
                        at + 1,
                        codes::MSG_STRING_IN_INTERPOLATION,
                    );
                    return self.skip_string(quote, first_token, open_before, depth);
                }
                '{' => {
                    if depth >= codes::MAX_INTERPOLATION_DEPTH {
                        let at = self.pos;
                        self.error(
                            codes::INTERPOLATION_TOO_DEEP,
                            at,
                            at + 1,
                            codes::MSG_INTERPOLATION_TOO_DEEP,
                        );
                        return self.skip_string(quote, first_token, open_before, depth);
                    }
                    depth += 1;
                    self.token(c);
                }
                '}' if depth == 1 => {
                    let piece = self.pos;
                    let (end, stop) = self.text(piece + 1);
                    self.pos = end;
                    match stop {
                        TextStop::Quote => {
                            self.open = open_before;
                            return self.push(TokenKind::StrTail, piece, end);
                        }
                        TextStop::Brace => self.push(TokenKind::StrMid, piece, end),
                        TextStop::LineEnd => {
                            return self.cut_text(quote, first_token, open_before);
                        }
                    }
                }
                '}' => {
                    depth -= 1;
                    self.token(c);
                }
                c => self.token(c),
            }
        }
    }

    /// Scans string text from `start` through the literal scanner and checks
    /// the result against the contract of [`LiteralScan::string_text`].
    fn text(&mut self, start: usize) -> (usize, TextStop) {
        let (end, stop) = self
            .literals
            .string_text(self.src, start, self.file, &mut self.diags);
        let ok = self.valid_end(start, end)
            && match stop {
                TextStop::Quote => end > start && self.byte_before(end) == Some(b'"'),
                TextStop::Brace => end > start && self.byte_before(end) == Some(b'{'),
                TextStop::LineEnd => true,
            };
        if ok {
            (end, stop)
        } else {
            (self.line_end_from(start), TextStop::LineEnd)
        }
    }

    /// E0003 for a string whose text (not the code of an interpolation) ran
    /// into a line end or the end of the input.
    ///
    /// A string cut by a line end is often meant to span two lines, and the
    /// quote that was meant to close it then opens a new string on the next
    /// line. Both are one fault (SPEC 12.1), so under SPEC 12.6 ("Recovery
    /// after a line end in a string") the next physical line closes the cut
    /// string only when all of these hold:
    ///
    /// * (a) its first character after leading spaces and tabs is `"`,
    /// * (b) lexed on its own, the line reports E0003 at exactly that quote,
    /// * (c) the text after that quote lexes without any diagnostic.
    ///
    /// Then the string is one `Error` token from its opening quote through that
    /// closing quote, the line end inside it does not end the logical line,
    /// and lexing goes on after the quote with the brackets that were open
    /// before the string. Only the directly following line is looked at. In
    /// every other case only the line that holds the opening quote belongs to
    /// the string, and the next line is lexed as usual, so a second real fault
    /// there keeps its own E0003.
    fn cut_text(&mut self, quote: usize, first_token: usize, open_before: u32) {
        let Some(close) = self.closing_quote_on_next_line() else {
            return self.unterminated_in(quote, first_token, open_before);
        };
        self.error(
            codes::UNTERMINATED_STRING,
            quote,
            quote + 1,
            codes::MSG_UNTERMINATED_STRING,
        );
        self.pos = close;
        self.collapse(quote, first_token, open_before);
    }

    /// The offset just after the quote that closes a string cut at the line
    /// end at `self.pos`, if the next line qualifies under [`Self::cut_text`].
    fn closing_quote_on_next_line(&self) -> Option<usize> {
        let rest = self.src.get(self.pos..)?;
        let start = self.pos
            + if rest.starts_with('\n') {
                1
            } else if rest.starts_with("\r\n") {
                2
            } else {
                return None;
            };
        let line_end = self.line_end_from(start);
        let line = self.src.get(start..line_end)?;
        // (a) Nothing but spaces and tabs before the quote.
        let blank = line.len() - line.trim_start_matches([' ', '\t']).len();
        if line.as_bytes().get(blank) != Some(&b'"') {
            return None;
        }
        // (b) On its own the line is cut at exactly this quote.
        let at = u32::try_from(blank).ok()?;
        let (_, line_diags) = lex_with(line, self.file, self.literals);
        let cut_here = line_diags
            .iter()
            .any(|d| d.code == codes::UNTERMINATED_STRING && d.span.start == at);
        if !cut_here {
            return None;
        }
        // (c) The rest of the line is clean on its own. It continues the line,
        // so its leading blanks are not indentation and are left out.
        let after = line.get(blank + 1..)?.trim_start_matches([' ', '\t']);
        let (_, rest_diags) = lex_with(after, self.file, self.literals);
        if !rest_diags.is_empty() {
            return None;
        }
        Some(start + blank + 1)
    }

    /// E0003: replaces every token of the string with one `Error` token from
    /// the opening quote to the current position.
    ///
    /// The line end that cut the string also ends the logical line, even inside
    /// an open `(`, so a missing quote does not pull the following lines into
    /// the statement and cause follow up diagnostics.
    fn unterminated_in(&mut self, quote: usize, first_token: usize, open_before: u32) {
        self.error(
            codes::UNTERMINATED_STRING,
            quote,
            quote + 1,
            codes::MSG_UNTERMINATED_STRING,
        );
        self.collapse(quote, first_token, open_before);
        self.open = 0;
    }

    /// Recovery after E0006 or E0007: skips the rest of the string without
    /// further diagnostics, counting braces from `depth`, and collapses the
    /// string into one `Error` token. Stops after the `"` that ends the string
    /// or before a line end.
    fn skip_string(&mut self, quote: usize, first_token: usize, open_before: u32, depth: u32) {
        let mut depth = depth;
        while let Some(c) = self.peek() {
            if c == '\n' || (c == '\r' && self.peek_at(1) == Some('\n')) {
                break;
            }
            self.pos += c.len_utf8();
            match c {
                '\\' => self.skip_escape(),
                '{' => depth = depth.saturating_add(1),
                '}' => depth = depth.saturating_sub(1),
                '"' if depth == 0 => break,
                _ => {}
            }
        }
        self.collapse(quote, first_token, open_before);
    }

    /// Skips the rest of an escape after its `\`, without diagnostics. A
    /// `\u{...}` escape is skipped up to its `}`, so its braces are never
    /// counted as interpolation braces.
    fn skip_escape(&mut self) {
        let Some(next) = self.peek().filter(|&n| n != '\n' && n != '\r') else {
            return;
        };
        self.pos += next.len_utf8();
        if next == 'u' && self.peek() == Some('{') {
            let rest = self.src.get(self.pos..).unwrap_or("");
            let len = rest.find(['}', '"', '\n', '\r']).map_or(rest.len(), |i| {
                if rest.get(i..).is_some_and(|r| r.starts_with('}')) {
                    i + 1
                } else {
                    i
                }
            });
            self.pos += len;
        }
    }

    fn collapse(&mut self, quote: usize, first_token: usize, open_before: u32) {
        self.tokens.truncate(first_token);
        self.open = open_before;
        self.push(TokenKind::Error, quote, self.pos);
    }

    // ----- helpers -----

    fn bidi(&mut self, c: char) {
        let at = self.pos;
        self.error(
            codes::BIDI_CONTROL,
            at,
            at + c.len_utf8(),
            codes::msg_bidi_control(c),
        );
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.pos..).and_then(|s| s.chars().next())
    }

    /// The character `n` characters after the current one.
    fn peek_at(&self, n: usize) -> Option<char> {
        self.src.get(self.pos..).and_then(|s| s.chars().nth(n))
    }

    fn byte_before(&self, end: usize) -> Option<u8> {
        end.checked_sub(1)
            .and_then(|i| self.src.as_bytes().get(i).copied())
    }

    fn valid_end(&self, start: usize, end: usize) -> bool {
        end >= start && end <= self.src.len() && self.src.is_char_boundary(end)
    }

    fn digits_end(&self, start: usize) -> usize {
        let digits = self
            .src
            .get(start..)
            .unwrap_or("")
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        start + digits.max(1)
    }

    fn line_end_from(&self, start: usize) -> usize {
        let rest = self.src.get(start..).unwrap_or("");
        let len = rest.find('\n').unwrap_or(rest.len());
        let end = start + len;
        if self.byte_before(end) == Some(b'\r') && end > start {
            end - 1
        } else {
            end
        }
    }

    fn span(&self, start: usize, end: usize) -> Span {
        let at = |o: usize| u32::try_from(o).unwrap_or(u32::MAX);
        Span::new(self.file, at(start), at(end))
    }

    fn push(&mut self, kind: TokenKind, start: usize, end: usize) {
        let span = self.span(start, end);
        self.tokens.push(Token::new(kind, span));
    }

    /// Pushes a token that belongs to the code of the current line.
    fn push_code(&mut self, kind: TokenKind, start: usize, end: usize) {
        self.line_has_tokens = true;
        self.push(kind, start, end);
    }

    fn error(&mut self, code: ostrel_core::Code, start: usize, end: usize, msg: impl Into<String>) {
        let span = self.span(start, end);
        self.diags.push(Diagnostic::error(code, span, msg));
    }
}

/// Tests of the D68 layout, line end and code point rules (ARCHITECTURE 3.5).
#[cfg(test)]
mod d68_tests {
    use super::{TokenKind, codes, lex};
    use TokenKind::{Dedent, Eof, Error, Ident, Indent, Nl};
    use ostrel_core::{Code, FileId};

    fn run(src: &str) -> (Vec<TokenKind>, Vec<(Code, u32, u32)>) {
        let (tokens, diags) = lex(src, FileId::from_raw(0));
        let kinds = tokens.iter().map(|t| t.kind).collect();
        let diags = diags
            .iter()
            .map(|d| (d.code, d.span.start, d.span.end))
            .collect();
        (kinds, diags)
    }

    fn kinds(src: &str) -> Vec<TokenKind> {
        run(src).0
    }

    fn texts(src: &str) -> Vec<String> {
        let (tokens, _) = lex(src, FileId::from_raw(0));
        tokens
            .iter()
            .map(|t| {
                let r = t.span.start as usize..t.span.end as usize;
                src.get(r).unwrap_or("<bad span>").to_string()
            })
            .collect()
    }

    #[test]
    fn one_level_deeper_opens_one_block_without_diagnostic() {
        let (k, d) = run("a\n  b\n    c\n");
        assert_eq!(
            k,
            vec![
                Ident, Nl, Indent, Ident, Nl, Indent, Ident, Nl, Dedent, Dedent, Eof
            ]
        );
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn jump_of_more_than_one_level_is_e0013_with_one_indent() {
        // a, then a line six columns deeper: one diagnostic on the leading
        // spaces, one `Indent`, one `Dedent` at the end.
        let src = "a\n      b\n";
        let (k, d) = run(src);
        assert_eq!(k, vec![Ident, Nl, Indent, Ident, Nl, Dedent, Eof]);
        assert_eq!(d, vec![(codes::DEEP_INDENT, 2, 8)]);
    }

    #[test]
    fn e0013_message_is_the_architecture_text() {
        let (_, diags) = lex("a\n    b\n", FileId::from_raw(0));
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "indentation is more than one level deeper than the line above; \
              indent a block by two spaces"
            ]
        );
        assert_eq!(codes::DEEP_INDENT.number(), 13);
    }

    #[test]
    fn lines_of_a_recovered_block_get_no_further_diagnostic() {
        // The real column 4 is pushed, so `c` and `d` stay in the block, and the
        // nested block of `d` at column 6 is one level deeper again.
        let src = "a\n    b\n    c\n      d\ne\n";
        let (k, d) = run(src);
        assert_eq!(
            k,
            vec![
                Ident, Nl, Indent, Ident, Nl, Ident, Nl, Indent, Ident, Nl, Dedent, Dedent, Ident,
                Nl, Eof,
            ]
        );
        assert_eq!(d, vec![(codes::DEEP_INDENT, 2, 6)]);
    }

    #[test]
    fn column_not_on_the_stack_closes_to_the_outer_level() {
        // Stack [0, 4] after the E0013 recovery; column 2 is not on it. It pops
        // the block of `b` and belongs to the outer level, without a diagnostic.
        let src = "a\n    b\n  c\n";
        let (k, d) = run(src);
        assert_eq!(
            k,
            vec![Ident, Nl, Indent, Ident, Nl, Dedent, Ident, Nl, Eof]
        );
        assert_eq!(d, vec![(codes::DEEP_INDENT, 2, 6)]);
    }

    #[test]
    fn dedent_pops_one_dedent_per_closed_block() {
        let src = "a\n  b\n    c\n      d\ne\n";
        let (k, d) = run(src);
        assert_eq!(
            k,
            vec![
                Ident, Nl, Indent, Ident, Nl, Indent, Ident, Nl, Indent, Ident, Nl, Dedent, Dedent,
                Dedent, Ident, Nl, Eof,
            ]
        );
        assert!(d.is_empty(), "{d:?}");
        // Dedent to a middle level.
        let (k, _) = run("a\n  b\n    c\n  d\n");
        assert_eq!(
            k,
            vec![
                Ident, Nl, Indent, Ident, Nl, Indent, Ident, Nl, Dedent, Ident, Nl, Dedent, Eof
            ]
        );
    }

    #[test]
    fn odd_column_is_only_e0002_even_when_it_jumps() {
        // Column 5 under column 0: E0002, no E0013; recovery opens one block
        // with the rounded column 6, so a following line at 6 is in that block.
        let src = "a\n     b\n      c\n";
        let (k, d) = run(src);
        assert_eq!(
            k,
            vec![Ident, Nl, Indent, Ident, Nl, Ident, Nl, Dedent, Eof]
        );
        assert_eq!(d, vec![(codes::ODD_INDENT, 2, 7)]);
    }

    #[test]
    fn odd_column_rounds_up_like_t8() {
        let (k, d) = run("a\n  b\n    c\n   d\n");
        assert_eq!(
            k,
            vec![
                Ident, Nl, Indent, Ident, Nl, Indent, Ident, Nl, Ident, Nl, Dedent, Dedent, Eof
            ]
        );
        assert_eq!(d, vec![(codes::ODD_INDENT, 12, 15)]);
    }

    #[test]
    fn tab_jump_is_only_e0001() {
        let (k, d) = run("a\n\t\tb\n");
        assert_eq!(k, vec![Ident, Nl, Indent, Ident, Nl, Dedent, Eof]);
        assert_eq!(d, vec![(codes::TAB_INDENT, 2, 3)]);
    }

    #[test]
    fn blank_and_comment_lines_do_not_count_for_the_jump() {
        let src = "a\n\n          // deep\n  b\n";
        let (k, d) = run(src);
        assert_eq!(
            k,
            vec![
                Ident,
                Nl,
                TokenKind::Comment,
                Indent,
                Ident,
                Nl,
                Dedent,
                Eof
            ]
        );
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn crlf_is_a_line_end_in_layout_and_spans() {
        let src = "a\r\n  b\r\n    c\n  d\r\n";
        let (k, d) = run(src);
        assert_eq!(
            k,
            vec![
                Ident, Nl, Indent, Ident, Nl, Indent, Ident, Nl, Dedent, Ident, Nl, Dedent, Eof
            ]
        );
        assert!(d.is_empty(), "{d:?}");
        let t = texts(src);
        assert_eq!(t.get(1).map(String::as_str), Some("\r\n"));
        assert_eq!(t.get(2).map(String::as_str), Some("  "));
        assert_eq!(t.get(3).map(String::as_str), Some("b"));
        // Mixed line ends after a jump: still one E0013 at the leading spaces.
        let (_, d) = run("a\r\n    b\n");
        assert_eq!(d, vec![(codes::DEEP_INDENT, 3, 7)]);
    }

    #[test]
    fn lone_cr_is_e0008_in_code_and_in_comments() {
        let (k, d) = run("a\rb\n");
        assert_eq!(k, vec![Ident, Error, Nl, Eof]);
        assert_eq!(d, vec![(codes::UNEXPECTED_CHAR, 1, 2)]);
        // In a comment: reported, the comment goes on to the real line end.
        let (k, d) = run("// x\ry\nz\n");
        assert_eq!(k, vec![TokenKind::Comment, Ident, Nl, Eof]);
        assert_eq!(d, vec![(codes::UNEXPECTED_CHAR, 4, 5)]);
        // A lone CR does not end a line, so it does not start layout.
        let (k, _) = run("a\r  b\n");
        assert!(!k.contains(&Indent), "{k:?}");
        // CR CR LF: the first CR is lone, the pair is a line end.
        let (k, d) = run("a\r\r\nb\n");
        assert_eq!(k, vec![Ident, Error, Nl, Ident, Nl, Eof]);
        assert_eq!(d, vec![(codes::UNEXPECTED_CHAR, 1, 2)]);
    }

    #[test]
    fn lone_cr_message_names_the_code_point() {
        let (_, diags) = lex("a\r", FileId::from_raw(0));
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages, ["unexpected character U+000D"]);
    }

    #[test]
    fn bom_only_at_byte_zero_is_skipped() {
        let (k, d) = run("\u{FEFF}a\n  b\n");
        assert_eq!(k, vec![Ident, Nl, Indent, Ident, Nl, Dedent, Eof]);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(texts("\u{FEFF}a").first().map(String::as_str), Some("a"));
        // A second BOM is code.
        let (_, d) = run("\u{FEFF}\u{FEFF}a\n");
        assert_eq!(d, vec![(codes::UNEXPECTED_CHAR, 3, 6)]);
        // A BOM later in code, also at the start of a line.
        let (_, d) = run("a \u{FEFF}\n");
        assert_eq!(d, vec![(codes::UNEXPECTED_CHAR, 2, 5)]);
        let (_, d) = run("a\n\u{FEFF}b\n");
        assert_eq!(d, vec![(codes::UNEXPECTED_CHAR, 2, 5)]);
    }

    #[test]
    fn bom_is_allowed_in_comments_and_strings() {
        let (_, d) = run("a // \u{FEFF}\nb = \"\u{FEFF}\"\n");
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn last_line_without_line_end_is_valid() {
        let (k, d) = run("a\n  b");
        assert_eq!(k, vec![Ident, Nl, Indent, Ident, Nl, Dedent, Eof]);
        assert!(d.is_empty(), "{d:?}");
        let (k, _) = run("a\n    b");
        assert_eq!(k, vec![Ident, Nl, Indent, Ident, Nl, Dedent, Eof]);
    }

    #[test]
    fn empty_input_gives_only_eof() {
        for src in ["", "\u{FEFF}", "\n", "\r\n", "  \n\n", "// c"] {
            let (k, d) = run(src);
            let k: Vec<TokenKind> = k.into_iter().filter(|t| *t != TokenKind::Comment).collect();
            assert_eq!(k, vec![Eof], "{src:?}");
            assert!(d.is_empty(), "{src:?}: {d:?}");
        }
    }

    #[test]
    fn indents_and_dedents_always_balance() {
        for src in [
            "a\n      b\n  c\n    d\n e\n\tf\ng",
            "a\n    b\n  c\n      d\n",
            "a\n\t\t\tb\n c\n   d",
            "\u{FEFF}a\r\n        b\r  c\n",
        ] {
            let k = kinds(src);
            let indents = k.iter().filter(|t| **t == Indent).count();
            let dedents = k.iter().filter(|t| **t == Dedent).count();
            assert_eq!(indents, dedents, "{src:?}");
            assert_eq!(k.last(), Some(&Eof), "{src:?}");
            let mut depth: i64 = 0;
            for t in &k {
                match t {
                    Indent => depth += 1,
                    Dedent => depth -= 1,
                    _ => {}
                }
                assert!(depth >= 0, "{src:?}");
            }
        }
    }
}
