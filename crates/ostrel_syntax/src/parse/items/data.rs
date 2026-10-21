//! `data` declarations, their fields, rules and checks (SYNTAX 4.2 to 4.4).
//!
//! ```text
//! dataDecl = "data" Name NL INDENT { member NL } DEDENT ;
//! member   = field | rule | "check" expr ;
//! field    = name ":" type [ "=" expr ] [ mode ] [ "merge" name ] ;
//! mode     = "fixed" | "server" | "serial" [ "per" name ] ;
//! rule     = verb [ name ] "if" expr ;
//! verb     = "see" | "make" | "edit" | "drop" ;
//! ```

use crate::ast::{DataDecl, FieldDecl, FieldMode, Item, Member, MemberKind, RuleDecl, RuleVerb};
use crate::lex::{Keyword, TokenKind};
use crate::parse::{Parser, Stop, codes, text_range};

impl<'a> Parser<'a> {
    /// `data Name` and its members. Without a block it is reported once and
    /// no item is built.
    pub(in crate::parse) fn data_decl(&mut self) -> Result<(), Stop> {
        let kw = self.bump();
        let name = self.head(|p| p.decl_name(Keyword::Data))?;
        if self.peek() != TokenKind::Indent {
            if let Some(name) = &name {
                let what = format!(
                    "an indented block with the fields and rules of `{}`",
                    name.text
                );
                self.expected(&what);
            }
            return Ok(());
        }
        let members = self.decl_lines(Parser::member)?;
        let Some(name) = name else {
            return Ok(());
        };
        let end = self.end_of(members.last().map(|m| m.range).or(Some(name.range)));
        let decl = DataDecl {
            name,
            members: members.into_boxed_slice(),
            range: text_range(kw.span.start, end),
        };
        self.add_item(Item::Data(Box::new(decl)))
    }

    /// One `member` line, without its line end.
    fn member(&mut self) -> Result<Member, Stop> {
        let tok = self.current();
        let verb = match tok.kind {
            TokenKind::Kw(Keyword::See) => Some(RuleVerb::See),
            TokenKind::Kw(Keyword::Make) => Some(RuleVerb::Make),
            TokenKind::Kw(Keyword::Edit) => Some(RuleVerb::Edit),
            TokenKind::Kw(Keyword::Drop) => Some(RuleVerb::Drop),
            _ => None,
        };
        let kind = if let Some(verb) = verb {
            self.bump();
            MemberKind::Rule(self.rule(verb)?)
        } else {
            match tok.kind {
                TokenKind::Kw(Keyword::Check) => {
                    self.bump();
                    MemberKind::Check(self.expr()?)
                }
                TokenKind::Ident => MemberKind::Field(Box::new(self.field()?)),
                TokenKind::Kw(kw) if self.nth(1) == TokenKind::Colon => {
                    let msg = codes::msg_keyword_as_name(kw);
                    return Err(self.report(codes::KEYWORD_AS_NAME, tok, msg));
                }
                _ => {
                    return Err(self
                        .expected("a field `name: Type`, a rule such as `see if ...` or `check`"));
                }
            }
        };
        Ok(Member {
            kind,
            range: self.range_from(tok.span.start),
        })
    }

    /// `[ name ] "if" expr` after a rule verb.
    fn rule(&mut self, verb: RuleVerb) -> Result<RuleDecl, Stop> {
        let field = if self.peek() == TokenKind::Ident {
            Some(self.name("a field name")?)
        } else {
            None
        };
        if self.peek() != TokenKind::Kw(Keyword::If) {
            let rule = match &field {
                Some(field) => format!("{} {}", verb.as_str(), field.text),
                None => verb.as_str().to_string(),
            };
            return Err(self.expected(&format!("`if` and a condition after `{rule}`")));
        }
        self.bump();
        let cond = self.expr()?;
        Ok(RuleDecl { verb, field, cond })
    }

    /// `name ":" type [ "=" expr ] [ mode ] [ "merge" name ]`.
    fn field(&mut self) -> Result<FieldDecl, Stop> {
        let name = self.name("a field name")?;
        if self.peek() != TokenKind::Colon {
            let what = format!("`:` and a type after field `{}`", name.text);
            return Err(self.expected(&what));
        }
        self.bump();
        let ty = self.ty()?;
        let default = if self.peek() == TokenKind::Eq {
            self.bump();
            Some(self.expr()?)
        } else {
            None
        };
        let mode = match self.peek() {
            TokenKind::Kw(Keyword::Fixed) => {
                self.bump();
                Some(FieldMode::Fixed)
            }
            TokenKind::Kw(Keyword::Server) => {
                self.bump();
                Some(FieldMode::Server)
            }
            TokenKind::Kw(Keyword::Serial) => {
                self.bump();
                let per = if self.peek() == TokenKind::Kw(Keyword::Per) {
                    self.bump();
                    Some(self.name("a field name after `per`")?)
                } else {
                    None
                };
                Some(FieldMode::Serial { per })
            }
            _ => None,
        };
        let merge = if self.peek() == TokenKind::Kw(Keyword::Merge) {
            self.bump();
            Some(self.name("a merge strategy such as `text` after `merge`")?)
        } else {
            None
        };
        Ok(FieldDecl {
            name,
            ty,
            default,
            mode,
            merge,
        })
    }
}
