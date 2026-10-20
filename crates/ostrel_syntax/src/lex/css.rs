//! CSS mode of the lexer for `style` bodies (SYNTAX 3, B-F21).
//!
//! A logical line that holds only the keyword `style` (comments aside) starts
//! CSS mode for the block under it. The body ends at the first non blank line
//! that is not deeper than the `style` line, by indentation alone, also when
//! that line holds only a comment (D137); such a line is lexed as code again,
//! and the blocks of the body close on it with their DEDENTs.
//!
//! Inside the body:
//!
//! * Layout is unchanged: every physical line ends a logical line, and the
//!   indentation rules of the module documentation apply (E0001, E0002,
//!   E0013). Brackets and braces never continue a line, so `url(//x)` cannot
//!   swallow the lines after it.
//! * `/* */` is a comment ([`TokenKind::Comment`]) and may span lines. The
//!   lines inside it carry no layout, except that a line not deeper than the
//!   `style` line ends the comment (E0015) and the body (D137, D138). A
//!   comment that is not closed before the
//!   end of the body is E0015 at its `/*` and ends there. `//` is CSS text.
//! * Everything else is CSS text: one [`TokenKind::Css`] token per run between
//!   comments on a line. `;`, `$token`, `#id`, `@media` and quotes are not
//!   checked; the lexer does not validate CSS.
//! * A quoted CSS string (`"..."` or `'...'`) hides `/*` and the other quote
//!   kind; `\` hides the next character. Neither ever consumes a line end: a
//!   string still open at the line end simply ends there, as a bad string
//!   does in CSS.
//! * Faults that hide or disguise source are reported as in code: a raw bidi
//!   control character is E0011 everywhere (D53); a control character other
//!   than tab, including a lone carriage return, is E0008 in CSS text and
//!   strings, once per run; U+FEFF is E0008 in CSS text and allowed in CSS
//!   strings and comments. In comments only bidi characters (E0011) and a
//!   lone carriage return (E0008) are reported, as in `//` comments.

use super::{Lexer, LiteralScan, TokenKind, codes};

impl<L: LiteralScan + ?Sized> Lexer<'_, L> {
    /// Lexes one token in CSS mode at the current position, which holds
    /// neither a blank nor a line end.
    pub(super) fn css_token(&mut self) {
        if self.rest().starts_with("/*") {
            self.css_comment();
        } else {
            self.css_text();
        }
    }

    /// True if the physical line from the current position (after its
    /// indentation) holds only `/* */` comments and blanks, or a comment that
    /// is not closed.
    pub(super) fn css_comment_only_line(&self) -> bool {
        let mut at = self.pos;
        loop {
            let rest = self.src.get(at..).unwrap_or("");
            let text = rest.trim_start_matches([' ', '\t']);
            at += rest.len() - text.len();
            if text.is_empty() || text.starts_with('\n') || text.starts_with("\r\n") {
                return true;
            }
            if !text.starts_with("/*") {
                return false;
            }
            let (end, closed) = self.css_comment_end(at);
            if !closed {
                return true;
            }
            at = end;
        }
    }

    /// Where the `/*` comment at `start` ends: after its `*/` (`true`), or,
    /// when it is not closed, at the line end before the first line that ends
    /// the CSS body, or at the end of the input (`false`).
    ///
    /// The search never passes the end of the body, so it is linear in the
    /// length of the body however many comments are left open.
    fn css_comment_end(&self, start: usize) -> (usize, bool) {
        let style = self.css.unwrap_or(0);
        let len = self.src.len();
        let mut at = start + 2;
        loop {
            let rest = self.src.get(at..).unwrap_or("");
            let line_end = rest.find('\n').map_or(len, |i| at + i);
            if let Some(i) = self.src.get(at..line_end).and_then(|l| l.find("*/")) {
                return (at + i + 2, true);
            }
            if line_end >= len {
                return (len, false);
            }
            let next = line_end + 1;
            if ends_body(self.src.get(next..).unwrap_or(""), style) {
                let end = if self.byte_before(line_end) == Some(b'\r') {
                    line_end - 1
                } else {
                    line_end
                };
                return (end.max(start + 2), false);
            }
            at = next;
        }
    }

    /// A `/* */` comment, possibly over several lines.
    fn css_comment(&mut self) {
        let start = self.pos;
        let (end, closed) = self.css_comment_end(start);
        while self.pos < end {
            let Some(c) = self.peek() else { break };
            if codes::is_bidi_control(c) {
                self.bidi(c);
            } else if c == '\r' && self.peek_at(1) != Some('\n') {
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
        self.pos = end;
        if !closed {
            self.error(
                codes::UNTERMINATED_CSS_COMMENT,
                start,
                start + 2,
                codes::MSG_UNTERMINATED_CSS_COMMENT,
            );
        }
        self.push(TokenKind::Comment, start, end);
    }

    /// A run of CSS text up to the next comment or the line end.
    fn css_text(&mut self) {
        let start = self.pos;
        let mut end = start;
        let mut quote: Option<char> = None;
        let mut escaped = false;
        let mut in_fault = false;
        while let Some(c) = self.peek() {
            if c == '\n' || (c == '\r' && self.peek_at(1) == Some('\n')) {
                break;
            }
            if !escaped && quote.is_none() && c == '/' && self.peek_at(1) == Some('*') {
                break;
            }
            let at = self.pos;
            let fault = (c.is_control() && c != '\t') || (c == '\u{FEFF}' && quote.is_none());
            if codes::is_bidi_control(c) {
                self.bidi(c);
            } else if fault && !in_fault {
                self.error(
                    codes::UNEXPECTED_CHAR,
                    at,
                    at + c.len_utf8(),
                    codes::msg_unexpected_char(c),
                );
            }
            in_fault = fault;
            self.pos += c.len_utf8();
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if quote == Some(c) {
                quote = None;
            } else if quote.is_none() && matches!(c, '"' | '\'') {
                quote = Some(c);
            }
            if c != ' ' && c != '\t' {
                end = self.pos;
            }
        }
        if end > start {
            self.push_code(TokenKind::Css, start, end);
        }
    }

    fn rest(&self) -> &str {
        self.src.get(self.pos..).unwrap_or("")
    }
}

/// True if the physical line `line` (up to its line end) ends a CSS body whose
/// `style` line has the block column `style`: it is not blank and its column,
/// counted as in the layout rules, is not deeper.
fn ends_body(line: &str, style: u32) -> bool {
    let mut width: u32 = 0;
    let mut faulty = false;
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b' ' => width = width.saturating_add(1),
            b'\t' => {
                faulty = true;
                width = width.saturating_add(2);
            }
            b'\n' => return false,
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => return false,
            _ => {
                if faulty || !width.is_multiple_of(2) {
                    width = width.div_ceil(2).saturating_mul(2);
                }
                return width <= style;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::super::{Keyword, Token, TokenKind, codes, lex};
    use TokenKind::{Comment, Css, Dedent, Eof, Indent, Kw, Nl};
    use ostrel_core::{Code, Diagnostic, FileId};

    const STYLE: TokenKind = Kw(Keyword::Style);

    fn run(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
        lex(src, FileId::from_raw(0))
    }

    fn kinds(src: &str) -> Vec<TokenKind> {
        run(src).0.iter().map(|t| t.kind).collect()
    }

    /// Kind and source text of every token.
    fn pairs(src: &str) -> Vec<(TokenKind, &str)> {
        run(src)
            .0
            .iter()
            .map(|t| {
                let text = src
                    .get(t.span.start as usize..t.span.end as usize)
                    .unwrap_or("<bad span>");
                (t.kind, text)
            })
            .collect()
    }

    fn diags(src: &str) -> Vec<(Code, u32, u32)> {
        run(src)
            .1
            .iter()
            .map(|d| (d.code, d.span.start, d.span.end))
            .collect()
    }

    fn css_texts(src: &str) -> Vec<&str> {
        pairs(src)
            .into_iter()
            .filter(|(k, _)| *k == Css)
            .map(|(_, t)| t)
            .collect()
    }

    #[test]
    fn the_chat_examples_lex_without_diagnostics() {
        for src in [
            include_str!("../../../../examples/chat/chat.ostl"),
            include_str!("../../../../examples/chat-nostd/chat.ostl"),
        ] {
            let (tokens, diags) = run(src);
            assert!(diags.is_empty(), "{diags:?}");
            let at = tokens.iter().position(|t| t.kind == STYLE);
            let body = tokens
                .get(at.map_or(tokens.len(), |i| i + 1)..)
                .unwrap_or(&[]);
            assert!(!body.is_empty(), "the chat ends with `style`");
            for t in body {
                assert!(
                    matches!(t.kind, Css | Nl | Indent | Dedent | Eof),
                    "{t:?} in the style body"
                );
            }
            let last = body.iter().rev().find(|t| t.kind == Css);
            assert_eq!(
                last.map(|t| t.span.end as usize),
                Some(src.trim_end().len())
            );
        }
    }

    #[test]
    fn a_body_lexes_one_css_token_per_line_with_layout() {
        let src = "style\n  .mine {\n    background: $accentSoft;\n  }\n";
        assert_eq!(
            pairs(src),
            vec![
                (STYLE, "style"),
                (Nl, "\n"),
                (Indent, "  "),
                (Css, ".mine {"),
                (Nl, "\n"),
                (Indent, "    "),
                (Css, "background: $accentSoft;"),
                (Nl, "\n"),
                (Dedent, ""),
                (Css, "}"),
                (Nl, "\n"),
                (Dedent, ""),
                (Eof, ""),
            ]
        );
        assert!(diags(src).is_empty());
    }

    #[test]
    fn double_slash_is_css_text_so_a_url_survives() {
        let src =
            "style\n  .a {\n    background: url(//cdn.example/x.png);\n    color: red;\n  }\n";
        assert_eq!(
            css_texts(src),
            [
                ".a {",
                "background: url(//cdn.example/x.png);",
                "color: red;",
                "}"
            ]
        );
        assert!(diags(src).is_empty(), "{:?}", diags(src));
        // An unclosed `(` does not continue the line in CSS mode either.
        let src = "style\n  .a {\n    background: url(x;\n  }\nfn main()\n";
        assert!(diags(src).is_empty());
        assert!(kinds(src).contains(&Kw(Keyword::Fn)));
    }

    #[test]
    fn slash_star_comments_are_trivia_and_split_the_text() {
        let src = "style\n  .a { /* x */ color: red; } /* y */\n";
        assert_eq!(
            pairs(src).get(2..7).unwrap_or(&[]),
            [
                (Indent, "  "),
                (Css, ".a {"),
                (Comment, "/* x */"),
                (Css, "color: red; }"),
                (Comment, "/* y */"),
            ]
        );
        assert!(diags(src).is_empty());
    }

    #[test]
    fn comment_only_lines_carry_no_layout() {
        // A comment line at any column, over two lines, and between blocks.
        let src = "style\n  .a {\n /* odd\n   column */\n    color: red;\n      /* deep */\n  }\n";
        assert!(diags(src).is_empty(), "{:?}", diags(src));
        assert_eq!(
            kinds(src),
            vec![
                STYLE, Nl, Indent, Css, Nl, Comment, Indent, Css, Nl, Comment, Dedent, Css, Nl,
                Dedent, Eof
            ]
        );
    }

    #[test]
    fn text_after_a_multi_line_comment_continues_its_first_line() {
        let src = "style\n  /* a\n  */ .x {\n  }\n";
        assert_eq!(
            kinds(src),
            vec![STYLE, Nl, Indent, Comment, Css, Nl, Css, Nl, Dedent, Eof]
        );
        assert!(diags(src).is_empty());
    }

    #[test]
    fn the_body_ends_at_a_line_not_deeper_than_style() {
        // `//` after the body is an Ostrel comment again, `fn` a keyword.
        let src = "style\n  .a {\n  }\n// done\nfn main()\n";
        let p = pairs(src);
        assert!(p.contains(&(Comment, "// done")), "{p:?}");
        assert!(p.contains(&(Kw(Keyword::Fn), "fn")), "{p:?}");
        assert!(diags(src).is_empty());
        // `/* */` after the body is code, not a comment: `/` and `*` tokens.
        let src = "style\n  .a {\n  }\n/* x */\n";
        let k = kinds(src);
        assert!(
            k.contains(&TokenKind::Slash) && k.contains(&TokenKind::Star),
            "{k:?}"
        );
        // `;` after the body is E0009 again.
        let (_, d) = run("style\n  .a { color: red; }\nx;\n");
        assert_eq!(
            d.iter().map(|d| d.code).collect::<Vec<_>>(),
            [codes::SEMICOLON]
        );
    }

    /// The tokens from the first DEDENT on, as kind and text.
    fn after_first_dedent(src: &str) -> Vec<(TokenKind, &str)> {
        pairs(src)
            .into_iter()
            .skip_while(|(k, _)| *k != Dedent)
            .collect()
    }

    #[test]
    fn a_comment_line_that_ends_the_body_closes_its_blocks_at_once() {
        // `//` at the `style` column ends the body (D137), and the DEDENTs of
        // the body stand before that comment, so no deeper line after it can
        // be read as part of the body.
        let src = "style\n  .a {\n// note\n    color: red;\n  }\n";
        let k = kinds(src);
        assert_eq!(
            k.get(..7),
            Some(&[STYLE, Nl, Indent, Css, Nl, Dedent, Comment][..]),
            "{k:?}"
        );
        assert_eq!(css_texts(src), [".a {"]);
        let body_end = src.find("// note").unwrap_or(0) as u32;
        let d = diags(src);
        assert!(!d.is_empty(), "{d:?}");
        assert!(d.iter().all(|d| d.1 >= body_end), "{d:?}");
        // The same without `;`: no diagnostic needed, but still no CSS after
        // the comment and the body closed before it.
        let src = "style\n  .a {\n// note\n    color: red\n  }\n";
        assert_eq!(css_texts(src), [".a {"]);
        assert_eq!(
            after_first_dedent(src).get(..2),
            Some(&[(Dedent, ""), (Comment, "// note")][..])
        );
        // `/* */` at the `style` column is code after the body; its line
        // closes the body before its first token.
        let src = "style\n  .a {\n    color: red;\n/* section */\n    b: c;\n  }\n";
        let k = kinds(src);
        let slash = k.iter().position(|k| *k == TokenKind::Slash);
        let last_css = k.iter().rposition(|k| *k == Css);
        let dedent = k.iter().position(|k| *k == Dedent);
        assert!(
            matches!((last_css, dedent, slash), (Some(c), Some(d), Some(s)) if c < d && d < s),
            "{k:?}"
        );
        assert_eq!(css_texts(src), [".a {", "color: red;"]);
        let body_end = src.find("/* section").unwrap_or(0) as u32;
        assert!(diags(src).iter().all(|d| d.1 >= body_end));
        // Two open blocks close together, also on a faulty (odd) column,
        // which a comment line never reports.
        let src = "a\n  style\n    .x {\n      y: z;\n // odd\n  b\n";
        assert_eq!(
            after_first_dedent(src).get(..3),
            Some(&[(Dedent, ""), (Dedent, ""), (Comment, "// odd")][..])
        );
        assert!(diags(src).is_empty(), "{:?}", diags(src));
        // A comment line still deeper than `style` keeps the body open.
        let src = "style\n  .a {\n  /* k */\n    color: red;\n  }\n";
        assert_eq!(css_texts(src), [".a {", "color: red;", "}"]);
        assert!(diags(src).is_empty());
    }

    #[test]
    fn d138_examples_end_the_body_with_its_dedent() {
        // (a) `//` at column 0: DEDENT before it, the comment is code trivia,
        // the deeper `.b` line opens a new block outside the body.
        let src = "style\n  .a {}\n// rows\n  .b {}\n";
        assert_eq!(css_texts(src), [".a {}"]);
        assert_eq!(
            after_first_dedent(src).get(..2),
            Some(&[(Dedent, ""), (Comment, "// rows")][..])
        );
        // (b) `/* */` at column 0: DEDENT before it, then lexed as code.
        let src = "style\n  .a {}\n/* rows */\n  .b {}\n";
        assert_eq!(css_texts(src), [".a {}"]);
        assert_eq!(
            after_first_dedent(src).get(..2),
            Some(&[(Dedent, ""), (TokenKind::Slash, "/")][..])
        );
        // (c) an indented `/* */` is a CSS comment and the body goes on.
        let src = "style\n  .a {}\n  /* rows */\n  .b {}\n";
        assert_eq!(css_texts(src), [".a {}", ".b {}"]);
        assert!(kinds(src).contains(&Comment));
        assert!(diags(src).is_empty(), "{:?}", diags(src));
        assert_eq!(after_first_dedent(src), [(Dedent, ""), (Eof, "")]);
        // (d) an open `/*`, then `.x` at column 0: E0015 at the `/*`, DEDENT
        // before `.x`.
        let src = "style\n  .a {}\n  /* open\n.x\n";
        let at = src.find("/*").unwrap_or(0) as u32;
        assert_eq!(
            diags(src).first(),
            Some(&(codes::UNTERMINATED_CSS_COMMENT, at, at + 2))
        );
        assert_eq!(css_texts(src), [".a {}"]);
        assert_eq!(
            after_first_dedent(src).get(..2),
            Some(&[(Dedent, ""), (TokenKind::Dot, ".")][..])
        );
    }

    #[test]
    fn a_nested_style_body_ends_at_its_own_column() {
        let src = "a\n  style\n    .x {\n    }\n  b;\n";
        let at = src.find(';').unwrap_or(0) as u32;
        assert_eq!(diags(src), vec![(codes::SEMICOLON, at, at + 1)]);
        assert_eq!(css_texts(src), [".x {", "}"]);
    }

    #[test]
    fn only_a_line_of_just_style_starts_css_mode() {
        // A comment after `style` is allowed.
        assert_eq!(
            css_texts("style // theme\n  .a { b: c; }\n"),
            [".a { b: c; }"]
        );
        // `style` with more on its line, or without a deeper block, is code.
        for src in [
            "style x\n  .a { b: c; }\n",
            "x style\n  a;\n",
            "style\na;\n",
        ] {
            assert!(css_texts(src).is_empty(), "{src:?}");
            assert!(
                diags(src).iter().any(|d| d.0 == codes::SEMICOLON),
                "{src:?}"
            );
        }
        // A `//` comment line at the outer column keeps the body pending.
        assert_eq!(css_texts("style\n// c\n  .a { b: c; }\n"), [".a { b: c; }"]);
        // A deeper `//` line already belongs to the body and is CSS text.
        assert_eq!(
            css_texts("style\n  // c\n  .a { b: c; }\n"),
            ["// c", ".a { b: c; }"]
        );
        // `style` inside parentheses or a string is no `style` line.
        assert!(css_texts("f(\nstyle)\n  a { b; }\n").is_empty());
        assert!(css_texts("\"style\"\n  a { b; }\n").is_empty());
    }

    #[test]
    fn css_text_is_not_checked_beyond_hidden_source() {
        let src = "style\n  @media (max-width: 40rem) {\n    #id > .a::after { content: 'it''s'; }\n  }\n  .b { width: calc(100% - 2px) !important; }\n";
        assert!(diags(src).is_empty(), "{:?}", diags(src));
        assert_eq!(css_texts(src).len(), 4);
    }

    #[test]
    fn quotes_and_escapes_hide_comment_starts() {
        let src = "style\n  .a::after { content: \"/*\"; }\n  .b::after { content: '\"/*'; }\n  .c\\/* { x: y; }\n";
        assert!(diags(src).is_empty(), "{:?}", diags(src));
        assert!(!kinds(src).contains(&Comment));
        assert_eq!(css_texts(src).len(), 3);
    }

    #[test]
    fn an_open_css_string_or_escape_ends_at_the_line_end() {
        // Neither consumes the line end, so the layout of the next line holds.
        let src = "style\n  .a { content: \"x\n  }\n  .b { content: x\\\n  }\n";
        assert!(diags(src).is_empty(), "{:?}", diags(src));
        assert_eq!(
            css_texts(src),
            [".a { content: \"x", "}", ".b { content: x\\", "}"]
        );
        // A `/*` after the line end is a comment again.
        let src = "style\n  .a { content: \"x\n  /* c */\n  }\n";
        assert!(kinds(src).contains(&Comment));
    }

    #[test]
    fn unterminated_comment_is_e0015_and_ends_with_the_body() {
        let src = "style\n  .a {\n  } /* open\n  .b {}\nfn main()\n";
        let at = src.find("/*").unwrap_or(0) as u32;
        assert_eq!(
            diags(src),
            vec![(codes::UNTERMINATED_CSS_COMMENT, at, at + 2)]
        );
        let p = pairs(src);
        assert!(p.contains(&(Comment, "/* open\n  .b {}")), "{p:?}");
        assert!(p.contains(&(Kw(Keyword::Fn), "fn")), "{p:?}");
        // An interior line at the `style` column ends the body and the comment.
        let src = "style\n  /* a\nb */\n";
        let at = src.find("/*").unwrap_or(0) as u32;
        let d = diags(src);
        assert_eq!(
            d.first(),
            Some(&(codes::UNTERMINATED_CSS_COMMENT, at, at + 2))
        );
        // The ending line is code again (D137); the comment line opened no
        // block, so there is nothing to close.
        let p = pairs(src);
        assert!(p.contains(&(TokenKind::Ident, "b")), "{p:?}");
        assert!(
            !p.iter().any(|(k, _)| *k == Indent || *k == Dedent),
            "{p:?}"
        );
        // With a block open, the ending line closes it before its first token.
        let src = "style\n  .a {\n  /* a\nb */\n";
        assert_eq!(
            after_first_dedent(src).get(..2),
            Some(&[(Dedent, ""), (TokenKind::Ident, "b")][..])
        );
        // To the end of the input, also with CRLF; `/*/` does not close.
        for src in [
            "style\n  .a {} /*",
            "style\r\n  /* a\r\n  b\r\n",
            "style\n  /*/\n",
        ] {
            let d = diags(src);
            assert_eq!(d.len(), 1, "{src:?}: {d:?}");
            assert_eq!(
                d.first().map(|d| d.0),
                Some(codes::UNTERMINATED_CSS_COMMENT)
            );
        }
        assert_eq!(
            codes::MSG_UNTERMINATED_CSS_COMMENT,
            "unterminated `/*` comment in a `style` block; close it with `*/`"
        );
        assert_eq!(codes::UNTERMINATED_CSS_COMMENT.number(), 15);
    }

    #[test]
    fn hidden_source_is_reported_in_css_text_strings_and_comments() {
        // Bidi characters: E0011 each, in a comment, a string and text (D53).
        let src = "style\n  .a { /* \u{202E} */ content: \"\u{2066}\"; x\u{202A}: y; }\n";
        let d = diags(src);
        assert_eq!(d.len(), 3, "{d:?}");
        assert!(d.iter().all(|d| d.0 == codes::BIDI_CONTROL), "{d:?}");
        // Control characters: one E0008 per run; a lone CR too.
        let d = diags("style\n  .a { b: \u{0}\u{0}\u{1}; c: \"\u{7}\"; }\r\n  .b\r{}\n");
        assert_eq!(
            d.iter().map(|d| (d.0, d.2 - d.1)).collect::<Vec<_>>(),
            [(codes::UNEXPECTED_CHAR, 1); 3]
        );
        // U+FEFF: E0008 in text, allowed in strings and comments.
        assert_eq!(
            diags("style\n  .a { b: \"\u{FEFF}\"; } /* \u{FEFF} */\n"),
            vec![]
        );
        let d = diags("style\n  .a\u{FEFF} { }\n");
        assert_eq!(d, vec![(codes::UNEXPECTED_CHAR, 10, 13)]);
        // A lone CR in a comment is E0008, a tab in text is fine.
        assert_eq!(
            diags("style\n  /* a\rb */\n  .a {\tb: c; }\n"),
            vec![(codes::UNEXPECTED_CHAR, 12, 13)]
        );
    }

    #[test]
    fn indentation_rules_hold_in_the_body() {
        assert_eq!(
            diags("style\n  .a {\n\tb: c;\n  }\n").first().map(|d| d.0),
            Some(codes::TAB_INDENT)
        );
        assert_eq!(
            diags("style\n  .a {\n   b: c;\n  }\n").first().map(|d| d.0),
            Some(codes::ODD_INDENT)
        );
        assert_eq!(
            diags("style\n  .a {\n      b: c;\n  }\n")
                .first()
                .map(|d| d.0),
            Some(codes::DEEP_INDENT)
        );
    }

    #[test]
    fn layout_balances_and_spans_stay_on_char_boundaries() {
        for src in [
            "style\n  .a {\n    b: c;",
            "style\n  .a {\n    /* x\n",
            "style\n  /*",
            "style\n  \u{FC}\u{202E}/*\u{FEFF}*/\"\u{1F600}\n",
            "a\n  style\n    .b {\n      c: d;\n    }\n  e\nf\n",
            "style\r\n  .a {\r\n    b: c;\r\n  }\r\n",
            "style\n  .a {\n// note\n    color: red;\n  }\n",
            "a\n  style\n    .x {\n      y: z;\n/* a */\n    b\n",
            "style\n  /* a\n// b\n    c\n",
        ] {
            let (tokens, _) = run(src);
            let opened = tokens.iter().filter(|t| t.kind == Indent).count();
            let closed = tokens.iter().filter(|t| t.kind == Dedent).count();
            assert_eq!(opened, closed, "{src:?}");
            assert_eq!(tokens.last().map(|t| t.kind), Some(Eof), "{src:?}");
            for t in &tokens {
                let (s, e) = (t.span.start as usize, t.span.end as usize);
                assert!(s <= e && src.get(s..e).is_some(), "{src:?}: {t:?}");
            }
        }
    }

    #[test]
    fn unclosed_comments_stay_linear() {
        // Every line opens a comment that the body end closes: one search per
        // body, not one per line.
        let source = |n: usize| {
            let mut s = String::from("style\n");
            for _ in 0..n {
                s.push_str("  /* a\n  .b { c: d; } /*\n");
            }
            s
        };
        crate::test_support::assert_linear(4_000, source, |src| run(src));
    }
}
