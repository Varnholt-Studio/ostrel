//! Writes the tokens of a file with canonical white space.

use ostrel_syntax::lex::{Keyword, Token, TokenKind};

use crate::space::{self, Side};
use crate::{INDENT, WIDTH, comment_lines, text};

/// Output of [`print`].
pub(crate) struct Printed {
    pub(crate) text: String,
    pub(crate) long_lines: Vec<u32>,
}

/// Writes `tokens`, lexed from `src`, with canonical white space.
pub(crate) fn print(src: &str, tokens: &[Token]) -> Printed {
    let mut p = Printer {
        src,
        tokens,
        records: records(tokens),
        brace: 0,
        out: String::with_capacity(src.len() + src.len() / 8),
        width: 0,
        line: 1,
        long_lines: Vec::new(),
        depth: 0,
        brackets: Vec::new(),
        before: None,
        prev: None,
        prev_end: 0,
        line_indent: 0,
        line_codes: 0,
        line_is_style: false,
        style_pending: false,
        css_level: None,
        ahead: None,
    };
    for (i, tok) in tokens.iter().enumerate() {
        p.token(i, *tok);
    }
    p.finish()
}

/// For every `{` of the file, in order: true if its group is a record, that
/// is, a `:` stands directly inside it.
fn records(tokens: &[Token]) -> Vec<bool> {
    use TokenKind as K;
    let mut flags = Vec::new();
    let mut open: Vec<Option<usize>> = Vec::new();
    for tok in tokens {
        match tok.kind {
            K::LBrace => {
                open.push(Some(flags.len()));
                flags.push(false);
            }
            K::LParen | K::LBracket | K::StrHead => open.push(None),
            K::RBrace | K::RParen | K::RBracket | K::StrTail => {
                open.pop();
            }
            K::Colon => {
                if let Some(Some(i)) = open.last()
                    && let Some(flag) = flags.get_mut(*i)
                {
                    *flag = true;
                }
            }
            K::Nl => open.clear(),
            _ => {}
        }
    }
    flags
}

/// An open bracket of the current logical line.
struct Open {
    /// Indentation of the output line that opened it.
    indent: usize,
    /// For `{`: the group is a record.
    record: bool,
}

struct Printer<'a> {
    src: &'a str,
    tokens: &'a [Token],
    /// Record flag of every `{`, see [`records`].
    records: Vec<bool>,
    /// Number of `{` written so far: the index into `records`.
    brace: usize,
    out: String,
    /// Columns of the current output line.
    width: usize,
    /// Number of the current output line, 1 based.
    line: u32,
    long_lines: Vec<u32>,
    /// Block level from `INDENT` and `DEDENT`.
    depth: usize,
    /// Open brackets of the current logical line.
    brackets: Vec<Open>,
    /// Kind of the code token before `prev` on this logical line, for the
    /// unary minus.
    before: Option<TokenKind>,
    /// The last token written on this logical line, as the spacing rules
    /// see it. `None` at the start of a logical line.
    prev: Option<Side>,
    /// End of the last token written, as a byte offset into the source.
    prev_end: usize,
    /// Indentation of the current output line.
    line_indent: usize,
    /// Code tokens (not comments) on the current logical line.
    line_codes: usize,
    /// The only code token of the current logical line so far is `style`.
    line_is_style: bool,
    /// The last logical line was `style` alone: the next `INDENT` opens a
    /// CSS body.
    style_pending: bool,
    /// Block level of the CSS body the printer is in, if any.
    css_level: Option<usize>,
    /// Cache for [`Printer::level_ahead`]: token index where the scan ended
    /// and the block level found there.
    ahead: Option<(usize, usize)>,
}

impl Printer<'_> {
    fn token(&mut self, i: usize, tok: Token) {
        use TokenKind as K;
        match tok.kind {
            K::Indent => {
                self.depth += 1;
                if self.style_pending {
                    self.css_level = Some(self.depth);
                }
                self.style_pending = false;
            }
            K::Dedent => {
                self.depth = self.depth.saturating_sub(1);
                if self.css_level.is_some_and(|level| self.depth < level) {
                    self.css_level = None;
                }
            }
            K::Nl => {
                self.style_pending = self.line_is_style && self.line_codes == 1;
                self.brackets.clear();
                self.before = None;
                self.prev = None;
                self.line_codes = 0;
                self.line_is_style = false;
            }
            K::Eof => {}
            _ => self.write(i, tok),
        }
    }

    /// Writes a token that has text of its own.
    fn write(&mut self, i: usize, tok: Token) {
        use TokenKind as K;
        let start = tok.span.start as usize;
        let gap = self.src.get(self.prev_end..start).unwrap_or("");
        let breaks = gap.bytes().filter(|b| *b == b'\n').count();
        let first_on_line = self.out.is_empty() || breaks > 0;
        let comment = tok.kind == K::Comment;

        let side = self.side(tok.kind);
        if first_on_line {
            if !self.out.is_empty() {
                self.end_line();
                if breaks > 1 {
                    self.end_line();
                }
            }
            let indent = if comment {
                self.comment_indent(i, gap)
            } else {
                self.code_indent(tok.kind)
            };
            self.line_indent = indent;
            self.push_spaces(indent);
        } else if let Some(prev) = self.prev {
            let css = self.css_level.is_some()
                && (matches!(prev.kind, K::Css | K::Comment)
                    || matches!(tok.kind, K::Css | K::Comment));
            let wanted = if css {
                !gap.is_empty()
            } else {
                space::space(prev, side)
            };
            if wanted {
                self.push_str(" ");
            }
        }

        if comment {
            let mut lines = comment_lines(text(self.src, tok));
            if let Some(first) = lines.next() {
                self.push_str(first);
            }
            for line in lines {
                self.end_line();
                self.push_str(line);
            }
        } else {
            self.push_str(text(self.src, tok));
            self.code(side);
        }
        self.prev_end = tok.span.end as usize;
        if !comment {
            self.before = self.prev.map(|p| p.kind);
            self.prev = Some(side);
        }
    }

    /// The token as the spacing rules see it, with its flag.
    fn side(&self, kind: TokenKind) -> Side {
        use TokenKind as K;
        match kind {
            K::Minus => Side::flagged(kind, space::unary_minus(self.prev.map(|p| p.kind))),
            K::LBrace => {
                Side::flagged(kind, self.records.get(self.brace).copied().unwrap_or(false))
            }
            K::RBrace => Side::flagged(kind, self.brackets.last().is_some_and(|o| o.record)),
            _ => Side::plain(kind),
        }
    }

    /// Bookkeeping after a code token was written.
    fn code(&mut self, side: Side) {
        use TokenKind as K;
        self.line_codes += 1;
        self.line_is_style = self.line_codes == 1 && side.kind == K::Kw(Keyword::Style);
        match side.kind {
            K::LParen | K::LBracket | K::LBrace => {
                if side.kind == K::LBrace {
                    self.brace += 1;
                }
                self.brackets.push(Open {
                    indent: self.line_indent,
                    record: side.kind == K::LBrace && side.flag,
                });
            }
            K::RParen | K::RBracket | K::RBrace => {
                self.brackets.pop();
            }
            _ => {}
        }
    }

    /// Indentation of a line whose first token is code.
    fn code_indent(&self, kind: TokenKind) -> usize {
        use TokenKind as K;
        match self.brackets.last() {
            None => self.depth * INDENT,
            Some(open) if matches!(kind, K::RParen | K::RBracket | K::RBrace) => open.indent,
            Some(open) => open.indent + INDENT,
        }
    }

    /// Indentation of a line whose first token is a comment.
    ///
    /// Inside a bracket it is that of an item. Otherwise the comment keeps its
    /// own level, rounded down, within the levels of the code around it: the
    /// level of the line before and that of the next line of code. Inside a
    /// CSS body it never leaves the body.
    fn comment_indent(&mut self, i: usize, gap: &str) -> usize {
        if let Some(open) = self.brackets.last() {
            return open.indent + INDENT;
        }
        let column = gap.rsplit('\n').next().map_or(0, str::len);
        let next = self.level_ahead(i);
        let mut low = self.depth.min(next);
        let high = self.depth.max(next);
        if let Some(level) = self.css_level {
            low = low.max(level);
        }
        (column / INDENT).clamp(low, high.max(low)) * INDENT
    }

    /// The block level of the next code token after token `i`.
    fn level_ahead(&mut self, i: usize) -> usize {
        use TokenKind as K;
        if let Some((end, level)) = self.ahead
            && i < end
        {
            return level;
        }
        let mut level = self.depth;
        let mut end = self.tokens.len();
        for (j, tok) in self.tokens.iter().enumerate().skip(i + 1) {
            match tok.kind {
                K::Indent => level += 1,
                K::Dedent => level = level.saturating_sub(1),
                K::Nl | K::Comment => {}
                _ => {
                    end = j;
                    break;
                }
            }
        }
        self.ahead = Some((end, level));
        level
    }

    fn push_spaces(&mut self, n: usize) {
        for _ in 0..n {
            self.out.push(' ');
        }
        self.width += n;
    }

    fn push_str(&mut self, s: &str) {
        self.out.push_str(s);
        self.width += s.chars().count();
    }

    /// Ends the current output line.
    fn end_line(&mut self) {
        if self.width > WIDTH {
            self.long_lines.push(self.line);
        }
        self.out.push('\n');
        self.width = 0;
        self.line += 1;
    }

    fn finish(mut self) -> Printed {
        if !self.out.is_empty() {
            self.end_line();
        }
        Printed {
            text: self.out,
            long_lines: self.long_lines,
        }
    }
}
