//! Tokens produced by the lexer.

use ostrel_core::Span;

/// One token: its kind and the bytes of the source it covers.
///
/// A token carries no decoded value. The parser reads names, integer values and
/// string text from the source slice of [`Token::span`]; this keeps a token at
/// 16 bytes (ARCHITECTURE 3.4 allows at most 24).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Token {
    /// What the token is.
    pub kind: TokenKind,
    /// The source bytes of the token. Layout tokens have the spans described
    /// at [`TokenKind`].
    pub span: Span,
}

impl Token {
    /// Creates a token.
    pub const fn new(kind: TokenKind, span: Span) -> Self {
        Token { kind, span }
    }

    /// True for tokens the parser skips and the formatter keeps (comments).
    pub const fn is_trivia(self) -> bool {
        matches!(self.kind, TokenKind::Comment)
    }
}

/// The kind of a [`Token`].
///
/// String literals (G1) are split at their interpolations, so the tokens of an
/// interpolated expression appear between the string pieces:
///
/// | Source | Tokens |
/// |---|---|
/// | `"plain"` | `Str` |
/// | `"a {x} b"` | `StrHead` (`"a {`), `Ident` (`x`), `StrTail` (`} b"`) |
/// | `"{x}{y}"` | `StrHead` (`"{`), `Ident`, `StrMid` (`}{`), `Ident`, `StrTail` (`}"`) |
///
/// A string literal with a structural fault (E0003, E0006, E0007) is exactly one
/// [`TokenKind::Error`] token from its opening quote to where the lexer resumed,
/// and no piece of it is emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// End of a logical line. Span: the line end (`\n` or `\r\n`), or an
    /// empty span at the end of the input when the last line has no line end.
    Nl,
    /// The line opens a block: it is deeper than the line above. Always one
    /// token, however deep the line is (D68). Span: the indentation.
    Indent,
    /// One block closed; one token per closed block. Span: empty, at the start
    /// of the first token of the line (or at the end of the input).
    Dedent,
    /// End of the input. Always the last token. Span: empty, at the end.
    Eof,
    /// A `//` comment up to, not including, the line end. Trivia.
    Comment,
    /// An identifier: an ASCII letter followed by ASCII letters, digits and `_`
    /// (SPEC 12.2), that is not a keyword.
    Ident,
    /// A decimal integer literal (G9).
    Int,
    /// A string literal without interpolation, quotes included.
    Str,
    /// The start of an interpolated string, from `"` up to and including the `{`
    /// that opens the first interpolation.
    StrHead,
    /// Text between two interpolations, from the closing `}` up to and including
    /// the next opening `{`.
    StrMid,
    /// The end of an interpolated string, from the closing `}` up to and
    /// including the closing `"`.
    StrTail,
    /// A keyword of SYNTAX 6 (G4).
    Kw(Keyword),
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `{` outside string text.
    LBrace,
    /// `}` outside string text, that does not close an interpolation.
    RBrace,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `..`
    DotDot,
    /// `:`
    Colon,
    /// `->`
    Arrow,
    /// `?`
    Question,
    /// `??`
    QuestionQuestion,
    /// `|`
    Pipe,
    /// `~`
    Tilde,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `=`
    Eq,
    /// `==`
    EqEq,
    /// `!=`
    BangEq,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// Source the lexer rejected. A diagnostic for it has already been
    /// reported, so later stages never report another one for this token.
    Error,
}

macro_rules! keywords {
    ($($variant:ident = $text:literal,)*) => {
        /// The 45 keywords of SYNTAX 6 (G4). All of them are reserved from v0.1 on.
        ///
        /// Contextual words (`new`, `added`, `removed`, `it`, `js`, `client`) are
        /// not keywords; they are [`TokenKind::Ident`] tokens.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Keyword {
            $(
                #[doc = concat!("`", $text, "`")]
                $variant,
            )*
        }

        impl Keyword {
            /// Every keyword, in the order of SYNTAX 6.
            pub const ALL: &'static [Keyword] = &[$(Keyword::$variant,)*];

            /// The keyword spelled `text`, if any.
            pub fn from_text(text: &str) -> Option<Keyword> {
                match text {
                    $($text => Some(Keyword::$variant),)*
                    _ => None,
                }
            }

            /// The spelling of the keyword.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Keyword::$variant => $text,)*
                }
            }
        }
    };
}

keywords! {
    App = "app",
    Auth = "auth",
    Home = "home",
    Data = "data",
    Enum = "enum",
    Check = "check",
    Merge = "merge",
    Fixed = "fixed",
    Server = "server",
    Serial = "serial",
    Per = "per",
    Var = "var",
    Let = "let",
    Fn = "fn",
    Call = "call",
    As = "as",
    System = "system",
    Return = "return",
    If = "if",
    Else = "else",
    For = "for",
    In = "in",
    Where = "where",
    Sort = "sort",
    Desc = "desc",
    Limit = "limit",
    Last = "last",
    See = "see",
    Make = "make",
    Edit = "edit",
    Drop = "drop",
    View = "view",
    Style = "style",
    Extern = "extern",
    On = "on",
    And = "and",
    Or = "or",
    Not = "not",
    Me = "me",
    Now = "now",
    None = "none",
    Signed = "signed",
    True = "true",
    False = "false",
    Catch = "catch",
}
