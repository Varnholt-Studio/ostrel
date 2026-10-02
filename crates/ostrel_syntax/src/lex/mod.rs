//! Lexer for the v0.1 token set (ARCHITECTURE 3, WP T1-2).
//!
//! The lexer turns source text into [`Token`]s and never fails: every fault is a
//! [`Diagnostic`] plus, where source is dropped, a [`TokenKind::Error`] token.
//! Comments stay in the stream as trivia for the formatter.
//!
//! # Layout
//!
//! * Indentation is two spaces per level (SYNTAX 3, P1). The lexer emits `Nl` at
//!   the end of every logical line, one `Indent` per level the next line is
//!   deeper and one `Dedent` per level it is shallower.
//! * Blank lines and lines holding only a comment produce no `Nl`, `Indent` or
//!   `Dedent`, and their leading whitespace is not checked (SPEC 12.2).
//! * While a `(` is open, line ends and indentation are ignored. From v0.2 `[`
//!   and `{` continue lines as well (SPEC 12.2); see [`continues_line`].
//! * At the end of the input the lexer closes the last line with `Nl`, emits a
//!   `Dedent` for every open level and ends with `Eof`.
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
        level: 0,
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
    /// Current indentation level, in steps of two spaces.
    level: u32,
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
            // ASSUMPTION: a byte order mark at the start of the file is skipped.
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
        for _ in 0..self.level {
            self.push(TokenKind::Dedent, end, end);
        }
        self.level = 0;
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
        let target = if faulty {
            // Recovery: at most one level deeper, so a faulty first line of a
            // block still opens the block and causes no follow up diagnostic.
            width.div_ceil(2).min(self.level.saturating_add(1))
        } else {
            width / 2
        };
        // ASSUMPTION (QUESTION #136 (3) open): every multiple of two spaces is a
        // level, so a jump of several levels gives several `Indent` tokens and a
        // dedent to any even column is valid. The parser judges the jump.
        while self.level < target {
            self.push(TokenKind::Indent, start, ws_end);
            self.level += 1;
        }
        while self.level > target {
            self.push(TokenKind::Dedent, ws_end, ws_end);
            self.level -= 1;
        }
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
            TextStop::LineEnd => return self.unterminated(quote, first_token),
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
                            return self.unterminated_in(quote, first_token, open_before);
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

    /// E0003 for a string whose text ran into a line end before any
    /// interpolation.
    fn unterminated(&mut self, quote: usize, first_token: usize) {
        let open = self.open;
        self.unterminated_in(quote, first_token, open);
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
