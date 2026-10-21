//! The space between two tokens on one line.
//!
//! Every rule names the tokens it applies to; the first rule that applies
//! decides. The default is one space. Readability reasons:
//!
//! * No space before `,`, `)`, `]`, `.`, `:`, `?` and after `(`, `[`, `.`:
//!   punctuation belongs to its neighbour, as in most languages a reader
//!   knows (`f(a, b)`, `x.len`, `name: Text`, `Room?`).
//! * No space between a name or a closing bracket and the `(` or `[` that
//!   follows it: a call or an index reads as one unit (`r.members.add(me)`,
//!   `Set[User]`). After any other token the bracket opens a new value and
//!   gets a space (`= [1, 2]`, `if (a or b)`).
//! * No space around `..`: a range is one value (`1..40`).
//! * A record is written `Name { field: value }` and a set `{a, b}`: the
//!   spaces inside the braces mark the record, whose fields are separated by
//!   `:` (a brace group with a `:` at its own level is a record).
//! * No space inside an interpolation next to its braces (`"a {x} b"`), so
//!   the text of the string reads as it is shown.
//! * No space after a unary `-` and after `~` (`-1`, `~mine(c)`).
//! * One space before a comment that follows code.
//! * Inside a `style` body the gaps between CSS pieces and comments stay as
//!   they are (none or one space), because a comment between two CSS values
//!   can matter to CSS.
//!
//! Two tokens are never written without a space where the lexer would read
//! them as one token (for example `.` and `.`, or `?` and `?`).

use ostrel_syntax::lex::{Keyword, TokenKind};

/// A token as the spacing rules see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Side {
    /// The token kind.
    pub kind: TokenKind,
    /// For `-`: it is a unary minus. For `{` and `}`: the group is a record.
    pub flag: bool,
}

impl Side {
    /// A token without a flag.
    pub const fn plain(kind: TokenKind) -> Side {
        Side { kind, flag: false }
    }

    /// A token with a flag (see [`Side::flag`]).
    pub const fn flagged(kind: TokenKind, flag: bool) -> Side {
        Side { kind, flag }
    }
}

/// True if one space goes between `prev` and `next` on the same line of code.
pub fn space(prev: Side, next: Side) -> bool {
    use TokenKind as K;
    let rule = if next.kind == K::Comment {
        true
    } else if prev.kind == K::LBrace {
        prev.flag && next.kind != K::RBrace
    } else if next.kind == K::RBrace {
        next.flag
    } else if matches!(
        next.kind,
        K::Comma
            | K::RParen
            | K::RBracket
            | K::Dot
            | K::DotDot
            | K::Colon
            | K::Question
            | K::StrMid
            | K::StrTail
    ) || matches!(
        prev.kind,
        K::LParen | K::LBracket | K::Dot | K::DotDot | K::Tilde | K::StrHead | K::StrMid
    ) || (prev.kind == K::Minus && prev.flag)
    {
        false
    } else if matches!(next.kind, K::LParen | K::LBracket) {
        !matches!(prev.kind, K::Ident | K::RParen | K::RBracket)
    } else {
        true
    };
    rule || would_merge(prev.kind, next.kind)
}

/// True if a `-` after `before` is a unary minus. `None` is the start of a
/// logical line. After a value (a name, a literal, a closing bracket or a
/// value keyword) a `-` subtracts; after anything else it negates.
pub fn unary_minus(before: Option<TokenKind>) -> bool {
    use TokenKind as K;
    match before {
        None => true,
        Some(
            K::Ident
            | K::Int
            | K::Str
            | K::StrTail
            | K::RParen
            | K::RBracket
            | K::RBrace
            | K::Question
            | K::Css
            | K::Comment
            | K::Error,
        ) => false,
        Some(K::Kw(kw)) => !is_value_keyword(kw),
        Some(_) => true,
    }
}

/// Keywords that stand for a value.
fn is_value_keyword(kw: Keyword) -> bool {
    matches!(
        kw,
        Keyword::Me
            | Keyword::Now
            | Keyword::None
            | Keyword::Signed
            | Keyword::True
            | Keyword::False
    )
}

/// True if the text of `prev` directly followed by the text of `next` would
/// lex as other tokens.
fn would_merge(prev: TokenKind, next: TokenKind) -> bool {
    use TokenKind as K;
    matches!(
        (prev, next),
        (K::Dot | K::DotDot, K::Dot | K::DotDot)
            | (K::Question, K::Question | K::QuestionQuestion)
            | (K::Minus, K::Minus | K::Gt | K::Ge | K::Arrow)
            | (K::Slash, K::Slash | K::Star)
            | (
                K::Eq
                    | K::Lt
                    | K::Gt
                    | K::Plus
                    | K::Minus
                    | K::Star
                    | K::Slash
                    | K::Percent
                    | K::Colon,
                K::Eq | K::EqEq
            )
            | (K::Ident | K::Int | K::Kw(_), K::Ident | K::Int | K::Kw(_))
    )
}
