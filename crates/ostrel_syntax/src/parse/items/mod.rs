//! Declarations of SYNTAX 7 and the parts they share (WP T119, v0.2).
//!
//! # Grammar read here
//!
//! ```text
//! decl     = appDecl | enumDecl | dataDecl | varDecl | fnDecl | viewDecl | styleDecl ;
//! appDecl  = "app" Name NL [ INDENT { appItem NL } DEDENT ] ;
//! appItem  = "auth" name [ name ] | "home" Name ;
//! enumDecl = "enum" Name "=" name { "|" name } NL ;
//! varDecl  = "var" name ( ":" type [ "=" expr ] | "=" expr ) NL ;
//! styleDecl = "style" NL INDENT CssBody DEDENT ;
//! type     = Name [ "[" type { "," type } "]" ] [ "?" ] ;
//! ```
//!
//! `data` and its members are read in `data.rs`, functions in `func.rs`,
//! views in `view.rs` and the `source` of `for` and `drop` (queries, D69)
//! in `query.rs`. `extern` is v0.3 (D95) and stays E0100 in `mod.rs`.
//!
//! Every declaration with a block follows the recovery rule of the statement
//! parser: a fault in the head line is reported, the rest of the line is
//! skipped and the block below it is still parsed for its own diagnostics,
//! without building the declaration. A fault in one line of a block skips
//! that line (and a block below it) and keeps the other lines.
//!
//! A `style` body is kept as a source range (D95). Until the lexer has the
//! CSS mode of SYNTAX 3, the lexer may report faults inside it (for example
//! `;` or `$token`); the parser itself never reports anything in a `style`
//! body.

mod data;
mod func;
mod query;
mod view;

#[cfg(test)]
pub(super) mod dump;
#[cfg(test)]
mod tests;

use super::{Parser, Stop, codes, text_range};
use crate::ast::{
    AppDecl, AppItem, AppItemKind, EnumDecl, Ident, Item, StyleDecl, TextRange, TyId, VarDecl,
    VarInit,
};
use crate::lex::{Keyword, TokenKind};

/// A type as written, before it is added to the module.
///
/// Function signatures need the whole type before the parser knows whether
/// the function is an [`Item::Fn`] (types are single names) or an
/// [`Item::PlacedFn`] (types are arena nodes), so types are read into this
/// form first and added by [`Parser::add_type`] (D95).
#[derive(Clone, Debug)]
pub(super) struct TypeSyntax {
    name: Ident,
    args: Vec<TypeSyntax>,
    optional: bool,
    range: TextRange,
}

impl TypeSyntax {
    /// True for a type that is a single name, which a v0.1 signature can
    /// hold.
    fn is_plain(&self) -> bool {
        self.args.is_empty() && !self.optional
    }
}

impl<'a> Parser<'a> {
    // ----- types -----

    /// `type = Name [ "[" type { "," type } "]" ] [ "?" ]`. Each `[` is one
    /// nesting level, which bounds the recursion.
    pub(super) fn type_syntax(&mut self) -> Result<TypeSyntax, Stop> {
        if self.peek() != TokenKind::Ident {
            return Err(self.expected("a type name"));
        }
        let tok = self.bump();
        let name = self.ident(tok);
        let mut args = Vec::new();
        if self.peek() == TokenKind::LBracket {
            let open = self.bump();
            self.enter(open)?;
            let read = self.type_args();
            self.leave();
            args = read?;
        }
        let optional = self.peek() == TokenKind::Question;
        if optional {
            self.bump();
        }
        let range = self.range_from(tok.span.start);
        Ok(TypeSyntax {
            name,
            args,
            optional,
            range,
        })
    }

    /// The type arguments after `[`, up to and including the `]`.
    fn type_args(&mut self) -> Result<Vec<TypeSyntax>, Stop> {
        let mut args = Vec::new();
        loop {
            args.push(self.type_syntax()?);
            match self.peek() {
                TokenKind::Comma => {
                    self.bump();
                }
                TokenKind::RBracket => {
                    self.bump();
                    return Ok(args);
                }
                _ => return Err(self.expected("`,` or `]`")),
            }
        }
    }

    /// Adds a type read by [`Parser::type_syntax`] to the module, arguments
    /// first. The recursion is as deep as the brackets of the source, which
    /// the nesting limit bounds.
    pub(super) fn add_type(&mut self, ty: TypeSyntax) -> Result<TyId, Stop> {
        let mut args = Vec::with_capacity(ty.args.len());
        for arg in ty.args {
            args.push(self.add_type(arg)?);
        }
        let added = self.module.add_ty(ty.name, args, ty.optional, ty.range);
        self.tree(added)
    }

    /// Reads a type and adds it to the module.
    pub(super) fn ty(&mut self) -> Result<TyId, Stop> {
        let ty = self.type_syntax()?;
        self.add_type(ty)
    }

    // ----- shared helpers -----

    /// Adds a top level item.
    fn add_item(&mut self, item: Item) -> Result<(), Stop> {
        let added = self.module.add_item(item);
        self.tree(added)
    }

    /// The name of a declaration after its keyword `kw`.
    fn decl_name(&mut self, kw: Keyword) -> Result<Ident, Stop> {
        self.name(&format!("a name after `{}`", kw.as_str()))
    }

    /// The lines of an indented block below a declaration head: `INDENT`
    /// was not consumed yet. `line` reads one line up to, not including,
    /// its line end; a fault in it skips the line. Returns what the lines
    /// gave, in source order.
    fn decl_lines<T>(
        &mut self,
        line: fn(&mut Parser<'a>) -> Result<T, Stop>,
    ) -> Result<Vec<T>, Stop> {
        let indent = self.bump();
        self.enter(indent)?;
        let mut out = Vec::new();
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
                TokenKind::Indent => {
                    let tok = self.current();
                    self.report(codes::UNEXPECTED_INDENT, tok, codes::MSG_UNEXPECTED_INDENT);
                    self.skip_block();
                }
                _ => {
                    let read = self
                        .check_unclosed()
                        .and_then(|()| line(self))
                        .and_then(|value| self.line_end().map(|()| value));
                    match read {
                        Ok(value) => out.push(value),
                        Err(Stop::Line) => self.recover(),
                        Err(Stop::File) => {
                            self.leave();
                            return Err(Stop::File);
                        }
                    }
                }
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.leave();
        Ok(out)
    }

    /// The end of the range of a declaration whose last part ends at `end`,
    /// or at the last consumed token if it has no parts.
    fn end_of(&self, end: Option<TextRange>) -> u32 {
        end.map_or(self.prev_end(), TextRange::end)
    }

    // ----- app -----

    /// `app Name` and its optional block of `auth` and `home` lines.
    pub(super) fn app_decl(&mut self) -> Result<(), Stop> {
        let kw = self.bump();
        let name = self.head(|p| p.decl_name(Keyword::App))?;
        let items = if self.peek() == TokenKind::Indent {
            self.decl_lines(Parser::app_item)?
        } else {
            Vec::new()
        };
        let Some(name) = name else {
            return Ok(());
        };
        let end = self.end_of(items.last().map(|i| i.range).or(Some(name.range)));
        let decl = AppDecl {
            name,
            items: items.into_boxed_slice(),
            range: text_range(kw.span.start, end),
        };
        self.add_item(Item::App(Box::new(decl)))
    }

    /// `appItem = "auth" name [ name ] | "home" Name`.
    fn app_item(&mut self) -> Result<AppItem, Stop> {
        let tok = self.current();
        let kind = match tok.kind {
            TokenKind::Kw(Keyword::Auth) => {
                self.bump();
                let scheme = self.name("an auth scheme such as `paseto` after `auth`")?;
                let mode = if self.peek() == TokenKind::Ident {
                    Some(self.name("a registration mode")?)
                } else {
                    None
                };
                AppItemKind::Auth { scheme, mode }
            }
            TokenKind::Kw(Keyword::Home) => {
                self.bump();
                AppItemKind::Home(self.name("a view name after `home`")?)
            }
            _ => return Err(self.expected("`auth` or `home` in an `app` declaration")),
        };
        Ok(AppItem {
            kind,
            range: self.range_from(tok.span.start),
        })
    }

    // ----- enum -----

    /// `enum Name = a | b | c` on one line.
    pub(super) fn enum_decl(&mut self) -> Result<(), Stop> {
        let kw = self.bump();
        let name = self.decl_name(Keyword::Enum)?;
        if self.peek() != TokenKind::Eq {
            let what = format!("`=` and the variants after `enum {}`", name.text);
            return Err(self.expected(&what));
        }
        self.bump();
        let mut variants = vec![self.name("a variant name")?];
        while self.peek() == TokenKind::Pipe {
            self.bump();
            variants.push(self.name("a variant name after `|`")?);
        }
        let range = self.range_from(kw.span.start);
        self.line_end()?;
        let decl = EnumDecl {
            name,
            variants: variants.into_boxed_slice(),
            range,
        };
        self.add_item(Item::Enum(Box::new(decl)))
    }

    // ----- var -----

    /// `var name: Type [= value]` or `var name = value`, client state.
    pub(super) fn var_decl(&mut self) -> Result<(), Stop> {
        let kw = self.bump();
        self.check_assign_operator()?;
        let name = self.decl_name(Keyword::Var)?;
        let init = match self.peek() {
            TokenKind::Colon => {
                self.bump();
                let ty = self.ty()?;
                let value = if self.peek() == TokenKind::Eq {
                    self.bump();
                    Some(self.expr()?)
                } else {
                    None
                };
                VarInit::Typed { ty, value }
            }
            TokenKind::Eq => {
                self.bump();
                VarInit::Inferred(self.expr()?)
            }
            _ => {
                let what = format!(
                    "`:` and a type, or `=` and a value, after `var {}`",
                    name.text
                );
                return Err(self.expected(&what));
            }
        };
        let range = self.range_from(kw.span.start);
        self.line_end()?;
        let decl = VarDecl { name, init, range };
        self.add_item(Item::Var(Box::new(decl)))
    }

    // ----- style -----

    /// `style` and its indented CSS body, kept as a source range. Nothing in
    /// the body is reported by the parser.
    pub(super) fn style_decl(&mut self) -> Result<(), Stop> {
        let kw = self.bump();
        let head_ok = self.head(|_| Ok(()))?.is_some();
        if self.peek() != TokenKind::Indent {
            if head_ok {
                self.expected("an indented CSS block after `style`");
            }
            return Ok(());
        }
        let indent = self.current();
        let mut end = indent.span.end;
        let mut level: u32 = 0;
        loop {
            let tok = self.current();
            match tok.kind {
                TokenKind::Indent => level += 1,
                TokenKind::Dedent => {
                    level = level.saturating_sub(1);
                    if level == 0 {
                        self.bump();
                        break;
                    }
                }
                TokenKind::Eof => break,
                TokenKind::Nl => {}
                _ => end = end.max(tok.span.end),
            }
            self.bump();
        }
        if !head_ok {
            return Ok(());
        }
        let decl = StyleDecl {
            body: text_range(indent.span.end, end),
            range: text_range(kw.span.start, end),
        };
        self.add_item(Item::Style(Box::new(decl)))
    }
}
