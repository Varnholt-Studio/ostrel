//! Function declarations (SYNTAX 4.6, 7 `fnDecl`, D95).
//!
//! ```text
//! fnDecl = [ "server" | "client" ] "fn" name params [ "->" type ] [ "as" "system" ] NL
//!          INDENT [ "call" "if" expr NL ] { stmt } DEDENT ;
//! params = "(" [ name ":" type { "," name ":" type } ] ")" ;
//! ```
//!
//! A function becomes an [`Item::Fn`] or an [`Item::PlacedFn`]. The choice is
//! made in exactly one place, [`Parser::add_function`] (D95, condition 1).

use super::TypeSyntax;
use crate::ast::{
    BlockId, CallRule, FnDecl, Ident, Item, Param, PlacedFnDecl, Placement, TextRange, TypeRef,
    TypedParam,
};
use crate::lex::{Keyword, TokenKind};
use crate::parse::{Parser, Stop, text_range};

/// A function head as written, before the parser knows which item it
/// becomes.
struct FnHead {
    placement: Placement,
    start: u32,
    name: Ident,
    params: Vec<ParamSyntax>,
    result: Option<TypeSyntax>,
    elevated: Option<TextRange>,
}

/// One parameter `name: type` as written.
struct ParamSyntax {
    name: Ident,
    ty: TypeSyntax,
}

impl<'a> Parser<'a> {
    /// `fnDecl`. A fault in the head is reported, and the body below it is
    /// still parsed (with its `call` rule) for its own diagnostics.
    pub(in crate::parse) fn fn_decl(&mut self) -> Result<(), Stop> {
        let head = self.head(Parser::fn_head)?;
        if head.is_none() && self.peek() != TokenKind::Indent {
            return Ok(());
        }
        let what = head.as_ref().map_or("of the function".to_string(), |h| {
            format!("of `{}`", h.name.text)
        });
        let mut call_rule = None;
        let body = if self.peek() == TokenKind::Indent {
            Some(self.block_with(Some(&mut call_rule))?)
        } else {
            self.expected(&format!("an indented block as the body {what}"));
            None
        };
        match (head, body) {
            (Some(head), Some(body)) => self.add_function(head, call_rule, body),
            _ => Ok(()),
        }
    }

    /// `[ "server" | "client" ] "fn" name params [ "->" type ] [ "as" "system" ]`.
    fn fn_head(&mut self) -> Result<FnHead, Stop> {
        let start = self.current().span.start;
        let placement = match self.peek() {
            TokenKind::Kw(Keyword::Server) => {
                self.bump();
                Placement::Server
            }
            TokenKind::Ident if self.is_word(self.current(), "client") => {
                self.bump();
                Placement::Client
            }
            _ => Placement::Inferred,
        };
        // The caller dispatched on `fn`, `server fn` or `client fn`.
        self.bump();
        let name = self.name("a function name after `fn`")?;
        if self.peek() != TokenKind::LParen {
            return Err(self.expected("`(` after the function name"));
        }
        let open = self.bump();
        self.enter(open)?;
        let params = self.fn_params();
        self.leave();
        let params = params?;
        let result = if self.peek() == TokenKind::Arrow {
            self.bump();
            Some(self.type_syntax()?)
        } else {
            None
        };
        let elevated = if self.peek() == TokenKind::Kw(Keyword::As) {
            let as_tok = self.bump();
            if self.peek() != TokenKind::Kw(Keyword::System) {
                return Err(self.expected("`system` after `as`"));
            }
            let system = self.bump();
            Some(text_range(as_tok.span.start, system.span.end))
        } else {
            None
        };
        Ok(FnHead {
            placement,
            start,
            name,
            params,
            result,
            elevated,
        })
    }

    /// The parameters after `(`, up to and including the `)`.
    fn fn_params(&mut self) -> Result<Vec<ParamSyntax>, Stop> {
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
            let ty = self.type_syntax()?;
            params.push(ParamSyntax { name, ty });
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

    /// `"call" "if" expr`, the first line of a function body; the line end
    /// is left for the caller.
    pub(in crate::parse) fn call_rule(&mut self) -> Result<CallRule, Stop> {
        let kw = self.bump();
        if self.peek() != TokenKind::Kw(Keyword::If) {
            return Err(self.expected("`if` and a condition after `call`"));
        }
        self.bump();
        let cond = self.expr()?;
        let range = self.range_from(kw.span.start);
        self.line_end()?;
        Ok(CallRule { cond, range })
    }

    /// Adds a parsed function as [`Item::Fn`] or [`Item::PlacedFn`]. This is
    /// the only place that decides between the two (D95, condition 1).
    ///
    /// A function is an [`Item::Fn`] exactly when none of these holds: it has
    /// a placement keyword (`server` or `client`), it is elevated
    /// (`as system`), it has a `call` rule, or a parameter or its result has
    /// a type beyond a single name (`Set[T]`, `T?`). Otherwise it is an
    /// [`Item::PlacedFn`], so the v0.1 passes keep seeing every function they
    /// can handle.
    fn add_function(
        &mut self,
        head: FnHead,
        call_rule: Option<CallRule>,
        body: BlockId,
    ) -> Result<(), Stop> {
        let end = self
            .module
            .block(body)
            .map_or(self.prev_end(), |b| b.range.end());
        let range = text_range(head.start, end);
        let plain = head.placement == Placement::Inferred
            && head.elevated.is_none()
            && call_rule.is_none()
            && head.params.iter().all(|p| p.ty.is_plain())
            && head.result.as_ref().is_none_or(TypeSyntax::is_plain);
        let item = if plain {
            let params = head
                .params
                .into_iter()
                .map(|p| Param::new(p.name, type_ref(p.ty)))
                .collect::<Vec<_>>();
            Item::Fn(Box::new(FnDecl {
                name: head.name,
                params: params.into_boxed_slice(),
                result: head.result.map(type_ref),
                body,
                range,
            }))
        } else {
            let mut params = Vec::with_capacity(head.params.len());
            for param in head.params {
                let param_range = param.name.range.cover(param.ty.range);
                let ty = self.add_type(param.ty)?;
                params.push(TypedParam {
                    name: param.name,
                    ty,
                    range: param_range,
                });
            }
            let result = match head.result {
                Some(result) => Some(self.add_type(result)?),
                None => None,
            };
            Item::PlacedFn(Box::new(PlacedFnDecl {
                placement: head.placement,
                name: head.name,
                params: params.into_boxed_slice(),
                result,
                elevated: head.elevated,
                call_rule,
                body,
                range,
            }))
        };
        self.add_item(item)
    }
}

/// The v0.1 form of a type that is a single name.
fn type_ref(ty: TypeSyntax) -> TypeRef {
    TypeRef {
        name: ty.name,
        range: ty.range,
    }
}
