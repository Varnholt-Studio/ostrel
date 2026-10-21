//! Views, elements, attributes and handlers (SYNTAX 4.8, 7, G16).
//!
//! ```text
//! viewDecl = "view" Name [ params ] NL INDENT { node } DEDENT ;
//! node     = element | "if" cond NL nodes [ "else" NL nodes ]
//!          | "for" name "in" source NL nodes ;
//! nodes    = INDENT { node } DEDENT ;
//! element  = ( name | Name ) [ value ] { attr } [ handler ] NL
//!            [ INDENT { node | handler NL } DEDENT ] ;
//! value    = expr ;                    (* not taken if next is name ":" , LL(2) *)
//! attr     = "~" name [ "(" expr ")" ] | name ":" expr ;
//! handler  = "on" name ( expr [ "=" expr ] ) ;
//! ```
//!
//! Element names, attribute names and events are not checked here; sema
//! validates them against the declared elements (B-F5).

use crate::ast::{
    Attr, AttrKind, Element, ElementChild, Handler, Ident, Item, TextRange, TypedParam, ViewDecl,
    ViewFor, ViewIf, ViewNodeId, ViewNodeKind,
};
use crate::lex::{Keyword, TokenKind};
use crate::parse::{Parser, Stop, codes, text_range};

/// The head of a view: its name and its parameters, `None` without
/// parentheses.
type ViewHead = (Ident, Option<Box<[TypedParam]>>);

impl<'a> Parser<'a> {
    /// `view Name [(params)]` and its nodes.
    pub(in crate::parse) fn view_decl(&mut self) -> Result<(), Stop> {
        let kw = self.bump();
        let head = self.head(Parser::view_head)?;
        if self.peek() != TokenKind::Indent {
            if let Some((name, _)) = &head {
                let what = format!("an indented block with the elements of `{}`", name.text);
                self.expected(&what);
            }
            return Ok(());
        }
        let nodes = self.view_nodes()?;
        let Some((name, params)) = head else {
            return Ok(());
        };
        let last = nodes.last().and_then(|id| self.node_range(*id));
        let end = self.end_of(last.or(Some(name.range)));
        let decl = ViewDecl {
            name,
            params,
            nodes: nodes.into_boxed_slice(),
            range: text_range(kw.span.start, end),
        };
        self.add_item(Item::View(Box::new(decl)))
    }

    /// `Name [ "(" [ name ":" type { "," name ":" type } ] ")" ]`.
    fn view_head(&mut self) -> Result<ViewHead, Stop> {
        let name = self.decl_name(Keyword::View)?;
        if self.peek() != TokenKind::LParen {
            return Ok((name, None));
        }
        let open = self.bump();
        self.enter(open)?;
        let params = self.view_params();
        self.leave();
        Ok((name, Some(params?.into_boxed_slice())))
    }

    /// The parameters of a view after `(`, up to and including the `)`.
    fn view_params(&mut self) -> Result<Vec<TypedParam>, Stop> {
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
            let ty = self.ty()?;
            let range = text_range(name.range.start(), self.prev_end());
            params.push(TypedParam { name, ty, range });
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

    /// The source range of a view node.
    fn node_range(&self, id: ViewNodeId) -> Option<TextRange> {
        self.module.view_node(id).map(|n| n.range)
    }

    /// `nodes = INDENT { node } DEDENT`; the current token is the `INDENT`.
    fn view_nodes(&mut self) -> Result<Vec<ViewNodeId>, Stop> {
        let lines = self.view_lines(false)?;
        Ok(lines
            .into_iter()
            .filter_map(|line| match line {
                ElementChild::Node(id) => Some(id),
                ElementChild::Handler(_) => None,
            })
            .collect())
    }

    /// An indented block of view lines; the current token is the `INDENT`.
    /// Below an element (`handlers`), a line may also be an `on` handler.
    fn view_lines(&mut self, handlers: bool) -> Result<Vec<ElementChild>, Stop> {
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
                _ => match self.view_line(handlers) {
                    Ok(Some(line)) => out.push(line),
                    Ok(None) => {}
                    Err(Stop::Line) => self.recover(),
                    Err(Stop::File) => {
                        self.leave();
                        return Err(Stop::File);
                    }
                },
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.leave();
        Ok(out)
    }

    /// One line of a view block: a node, or a handler below an element.
    /// `Ok(None)` means a fault was reported and recovered from.
    fn view_line(&mut self, handlers: bool) -> Result<Option<ElementChild>, Stop> {
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
        let node = match tok.kind {
            TokenKind::Kw(Keyword::On) if handlers => {
                let handler = self.handler()?;
                self.line_end()?;
                return Ok(Some(ElementChild::Handler(handler)));
            }
            TokenKind::Kw(Keyword::On) => {
                return Err(self
                    .expected("an element, `if` or `for` (an `on` line stands below an element)"));
            }
            TokenKind::Kw(Keyword::If) => self.view_if()?,
            TokenKind::Kw(Keyword::For) => self.view_for()?,
            TokenKind::Ident => self.element()?,
            _ => return Err(self.expected("an element, `if` or `for`")),
        };
        Ok(node.map(ElementChild::Node))
    }

    /// An element line and the lines indented below it. A fault in the
    /// element line is reported and its children are still parsed.
    fn element(&mut self) -> Result<Option<ViewNodeId>, Stop> {
        let start = self.current().span.start;
        let head = self.head(Parser::element_head)?;
        let children = if self.peek() == TokenKind::Indent {
            self.view_lines(true)?
        } else {
            Vec::new()
        };
        let Some((mut element, head_end)) = head else {
            return Ok(None);
        };
        let end = match children.last() {
            Some(ElementChild::Node(id)) => self.node_range(*id).map_or(head_end, |r| r.end()),
            Some(ElementChild::Handler(handler)) => handler.range.end(),
            None => head_end,
        };
        element.children = children.into_boxed_slice();
        let kind = ViewNodeKind::Element(Box::new(element));
        let added = self.module.add_view_node(kind, text_range(start, end));
        self.tree(added).map(Some)
    }

    /// `name [ value ] { attr } [ handler ]` without its line end; returns
    /// the element without children and the end of the line's last token.
    fn element_head(&mut self) -> Result<(Element, u32), Stop> {
        self.check_assign_operator()?;
        let tok = self.bump();
        let name = self.ident(tok);
        let value = if self.starts_value() {
            Some(self.expr()?)
        } else {
            None
        };
        let mut attrs = Vec::new();
        loop {
            let at = self.current();
            let kind = match at.kind {
                TokenKind::Tilde => {
                    self.bump();
                    let name = self.style_tag()?;
                    let cond = if self.peek() == TokenKind::LParen {
                        Some(self.style_cond()?)
                    } else {
                        None
                    };
                    AttrKind::Style { name, cond }
                }
                TokenKind::Ident if self.nth(1) == TokenKind::Colon => {
                    self.bump();
                    let name = self.ident(at);
                    self.bump();
                    let value = self.expr()?;
                    AttrKind::Named { name, value }
                }
                _ => break,
            };
            let range = self.range_from(at.span.start);
            attrs.push(Attr { kind, range });
        }
        let handler = if self.peek() == TokenKind::Kw(Keyword::On) {
            Some(self.handler()?)
        } else {
            None
        };
        let element = Element {
            name,
            value,
            attrs: attrs.into_boxed_slice(),
            handler,
            children: Box::default(),
        };
        Ok((element, self.prev_end()))
    }

    /// True if the element value starts here. It does not if the line ends,
    /// an attribute (`name:`, LL(2), or `~`) or a handler follows, or the
    /// line ends in `:` or `{` (reported as a block head).
    fn starts_value(&self) -> bool {
        match self.peek() {
            TokenKind::Nl
            | TokenKind::Eof
            | TokenKind::Dedent
            | TokenKind::Tilde
            | TokenKind::Colon
            | TokenKind::Kw(Keyword::On) => false,
            TokenKind::LBrace => !matches!(self.nth(1), TokenKind::Nl | TokenKind::Eof),
            TokenKind::Ident => self.nth(1) != TokenKind::Colon,
            _ => true,
        }
    }

    /// The name after `~`. It names a CSS class, never a declaration, so a
    /// keyword is accepted too (`row ~app` in SYNTAX 5.2).
    ///
    /// ASSUMPTION (T119): SYNTAX 7 writes `"~" name`; the example of SYNTAX
    /// 5.2 and `examples/chat-nostd` use the keyword `app` as a style tag, so
    /// the sigil makes any word a tag name.
    fn style_tag(&mut self) -> Result<Ident, Stop> {
        let tok = self.current();
        match tok.kind {
            TokenKind::Ident | TokenKind::Kw(_) => {
                self.bump();
                Ok(self.ident(tok))
            }
            _ => Err(self.expected("a style tag name after `~`")),
        }
    }

    /// `"(" expr ")"` after a style tag.
    fn style_cond(&mut self) -> Result<crate::ast::ExprId, Stop> {
        let open = self.bump();
        self.enter(open)?;
        let cond = self.expr().and_then(|cond| {
            if self.peek() == TokenKind::RParen {
                self.bump();
                Ok(cond)
            } else {
                Err(self.expected("`)`"))
            }
        });
        self.leave();
        cond
    }

    /// `"on" name expr [ "=" expr ]`.
    fn handler(&mut self) -> Result<Handler, Stop> {
        self.check_assign_operator()?;
        let kw = self.bump();
        let event = self.name("an event name such as `click` after `on`")?;
        if matches!(
            self.peek(),
            TokenKind::Nl | TokenKind::Eof | TokenKind::Dedent
        ) {
            let what = format!("an action after `on {}`", event.text);
            return Err(self.expected(&what));
        }
        let action = self.expr()?;
        let assign = if self.peek() == TokenKind::Eq {
            self.bump();
            Some(self.expr()?)
        } else {
            None
        };
        Ok(Handler {
            event,
            action,
            assign,
            range: self.range_from(kw.span.start),
        })
    }

    /// `if cond NL nodes [ else NL nodes ]` in a view.
    fn view_if(&mut self) -> Result<Option<ViewNodeId>, Stop> {
        let kw = self.bump();
        let cond = self.head(Parser::cond)?;
        let then_nodes = self.nodes_after(cond.is_some(), "after `if`")?;
        let else_nodes = if self.peek() == TokenKind::Kw(Keyword::Else) {
            self.view_else()?
        } else {
            None
        };
        let (Some(cond), Some(then_nodes)) = (cond, then_nodes) else {
            return Ok(None);
        };
        let last = else_nodes
            .as_ref()
            .and_then(|nodes| nodes.last())
            .or(then_nodes.last())
            .and_then(|id| self.node_range(*id));
        let end = self.end_of(last);
        let node = ViewIf {
            cond,
            then_nodes: then_nodes.into_boxed_slice(),
            else_nodes: else_nodes.map(Vec::into_boxed_slice),
        };
        let kind = ViewNodeKind::If(Box::new(node));
        let added = self
            .module
            .add_view_node(kind, text_range(kw.span.start, end));
        self.tree(added).map(Some)
    }

    /// `else` and its nodes in a view. `else if` on one line is E0024, and
    /// the `if` is read as the only node of the `else` part, as in a
    /// function body.
    fn view_else(&mut self) -> Result<Option<Vec<ViewNodeId>>, Stop> {
        let else_tok = self.bump();
        let tok = self.current();
        if tok.kind == TokenKind::Kw(Keyword::If) {
            self.report(codes::ELSE_IF_ONE_LINE, tok, codes::MSG_ELSE_IF_ONE_LINE);
            self.enter(else_tok)?;
            let inner = self.view_if();
            self.leave();
            return Ok(inner?.map(|id| vec![id]));
        }
        let head_ok = match self.head_end() {
            Ok(()) => true,
            Err(Stop::File) => return Err(Stop::File),
            Err(Stop::Line) => {
                self.skip_line();
                false
            }
        };
        self.nodes_after(head_ok, "after `else`")
    }

    /// The nodes below a head line. A missing block is reported once, and
    /// only after a head without fault.
    fn nodes_after(&mut self, head_ok: bool, what: &str) -> Result<Option<Vec<ViewNodeId>>, Stop> {
        if self.peek() == TokenKind::Indent {
            return self.view_nodes().map(Some);
        }
        if head_ok {
            self.expected(&format!("an indented block {what}"));
        }
        Ok(None)
    }

    /// `for name in source NL nodes` in a view.
    fn view_for(&mut self) -> Result<Option<ViewNodeId>, Stop> {
        let kw = self.bump();
        let head = self.head(Parser::for_head)?;
        let nodes = self.nodes_after(head.is_some(), "as the body of `for`")?;
        let (Some((name, source)), Some(nodes)) = (head, nodes) else {
            return Ok(None);
        };
        let end = self.end_of(nodes.last().and_then(|id| self.node_range(*id)));
        let node = ViewFor {
            name,
            source,
            nodes: nodes.into_boxed_slice(),
        };
        let kind = ViewNodeKind::For(Box::new(node));
        let added = self
            .module
            .add_view_node(kind, text_range(kw.span.start, end));
        self.tree(added).map(Some)
    }
}
