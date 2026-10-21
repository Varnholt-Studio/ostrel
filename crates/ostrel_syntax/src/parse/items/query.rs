//! The `source` of `for` and `drop`, and queries (SYNTAX 4.5, G14, D69).
//!
//! ```text
//! source = query | expr ;
//! query  = name [ "where" expr ] [ "sort" name [ "desc" ] ] [ ( "limit" | "last" ) expr ] ;
//! ```
//!
//! The lexer has one identifier kind, so the parser cannot tell a type from
//! a value (D69, ARCHITECTURE 3.5). A bare name in a source position, that
//! is a name that is not continued as an expression, is always read as an
//! [`ExprKind::Query`] with or without clauses; sema decides whether it
//! names a data type, an enum or a value. Anything else is an ordinary
//! expression.

use crate::ast::{ExprId, ExprKind, Query, SortKey, Take, TakeKind};
use crate::lex::{Keyword, TokenKind};
use crate::parse::{Parser, Stop, text_range};

/// Tokens after a bare name that end the name as an expression, so the name
/// is the head of a query. `:` and `{` end a head line with their own
/// diagnostic (E0023, E0022).
fn ends_query_head(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Nl
            | TokenKind::Eof
            | TokenKind::Dedent
            | TokenKind::Colon
            | TokenKind::LBrace
            | TokenKind::Kw(Keyword::Where | Keyword::Sort | Keyword::Limit | Keyword::Last)
    )
}

/// True for the keywords that start a query clause.
fn is_clause(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Kw(Keyword::Where | Keyword::Sort | Keyword::Limit | Keyword::Last)
    )
}

impl<'a> Parser<'a> {
    /// `source = query | expr`.
    pub(in crate::parse) fn source(&mut self) -> Result<ExprId, Stop> {
        if self.peek() == TokenKind::Ident && ends_query_head(self.nth(1)) {
            return self.query();
        }
        let value = self.expr()?;
        if is_clause(self.peek()) {
            return Err(
                self.expected("the end of the line (query clauses follow only a bare type name)")
            );
        }
        Ok(value)
    }

    /// A query: its head and the clauses in the order `where`, `sort`,
    /// `limit` or `last`.
    fn query(&mut self) -> Result<ExprId, Stop> {
        let head_tok = self.bump();
        let head = self.ident(head_tok);
        let filter = if self.peek() == TokenKind::Kw(Keyword::Where) {
            self.bump();
            Some(self.expr()?)
        } else {
            None
        };
        let sort = if self.peek() == TokenKind::Kw(Keyword::Sort) {
            let kw = self.bump();
            let field = self.name("a field name after `sort`")?;
            let desc = self.peek() == TokenKind::Kw(Keyword::Desc);
            if desc {
                self.bump();
            }
            let range = self.range_from(kw.span.start);
            Some(SortKey { field, desc, range })
        } else {
            None
        };
        let take = match self.peek() {
            TokenKind::Kw(kw @ (Keyword::Limit | Keyword::Last)) => {
                let tok = self.bump();
                let kind = if kw == Keyword::Limit {
                    TakeKind::Limit
                } else {
                    TakeKind::Last
                };
                let count = self.expr()?;
                let range = self.range_from(tok.span.start);
                Some(Take { kind, count, range })
            }
            _ => None,
        };
        if is_clause(self.peek()) {
            return Err(self.expected(
                "the end of the query (its clauses come in the order `where`, `sort`, \
                 then `limit` or `last`, each at most once)",
            ));
        }
        let range = text_range(head_tok.span.start, self.prev_end());
        let query = Query {
            head,
            filter,
            sort,
            take,
        };
        self.add_expr(ExprKind::Query(Box::new(query)), range)
    }
}
