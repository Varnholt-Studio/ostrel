//! Syntax tree of the Ostrel programming language (SYNTAX 4 and 7,
//! ARCHITECTURE 3.4).
//!
//! The v0.1 slice covers `fn` declarations with typed parameters and an
//! optional result type, the statements `let`, `if`/`else`, `return` and
//! expression statements, and the expressions of the v0.1 subset: integer,
//! boolean and string literals with interpolation, names, unary and binary
//! operators, calls and parentheses.
//!
//! The v0.2 items (SYNTAX 4.1 to 4.8) are added on top of it: `app`, `enum`,
//! `data` with fields, `merge`, rules and `check`, `var`, placed functions
//! with `call` rules and `as system`, queries (D69), `view` with elements and
//! `style`, the statements `if let`, `for`, `drop` and assignment, and the
//! expressions of SYNTAX 7 that the v0.1 subset leaves out. See
//! [the v0.2 section](#v02-items) below.
//!
//! The tree grows by addition; the node enums are `#[non_exhaustive]`, so
//! passes in other crates keep a fallback arm that reports
//! `E0100 not available in v0.1` for constructs they do not know yet.
//!
//! # Storage
//!
//! All nodes of one file live in a [`Module`]. Expressions, statements,
//! blocks, v0.2 types and view nodes are stored in arenas and referenced by
//! `u32` ids ([`ExprId`], [`StmtId`], [`BlockId`], [`TyId`], [`ViewNodeId`]).
//! The arena enforces three invariants when a node is added, so every later
//! pass can rely on them:
//!
//! * The structure is a tree: a child id must already exist in this module
//!   and may be attached to exactly one parent. Children are always added
//!   before their parent, so the tree has no cycles. Ids are not tied to
//!   their module: an id from another module is only detected when its index
//!   does not exist here, so never mix ids of two modules.
//! * The number of nodes stays at or below [`Limits::max_nodes`].
//! * The height stays at or below [`Limits::max_height`], so recursive passes
//!   over the tree are bounded (D54).
//!
//! # Counting
//!
//! Height is the number of nodes on the longest path from the module root to
//! a leaf; a leaf has height 1. The module root itself is part of that path.
//! The node count includes the module root, every item, parameter, type
//! reference, block, statement and expression. Tokens and trivia never count.
//!
//! The v0.2 parts follow the same rule. Every arena entry counts once. Parts
//! that are stored inline in their parent but stand in a list or carry their
//! own range also count once each and form one level of height between the
//! parent and their children: app items, enum variants, data members,
//! parameters of placed functions and views, the `call` rule, attributes,
//! handlers, map entries, field initializers of `make`, `catch` kinds, the
//! `sort` key and the `limit`/`last` clause of a query. So a list that holds
//! only names, such as the variants of an enum, is bounded by the node limit
//! as well.
//!
//! # Ranges
//!
//! Nodes carry a [`TextRange`] of byte offsets into the source text of their
//! file. The file itself is known by the caller that owns the module.
//!
//! # Constructors for tests
//!
//! Besides the general `add_*` methods used by the parser, [`Module`] offers
//! short constructors such as [`Module::int`] or [`Module::binary`]. They give
//! every node an empty range at offset 0 and let other teams build trees
//! without the parser.

use std::fmt;

/// Default upper bound for the number of nodes in one file (D54).
pub const MAX_NODES: u32 = 1_000_000;

/// Default upper bound for the height of the syntax tree of one file (D54).
pub const MAX_HEIGHT: u32 = 2_048;

/// Limits a [`Module`] enforces while nodes are added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Highest allowed number of nodes, module root included.
    pub max_nodes: u32,
    /// Highest allowed tree height, module root included.
    pub max_height: u32,
}

impl Limits {
    /// The limits of D54: [`MAX_NODES`] and [`MAX_HEIGHT`].
    pub const DEFAULT: Limits = Limits {
        max_nodes: MAX_NODES,
        max_height: MAX_HEIGHT,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Limits::DEFAULT
    }
}

/// Why a node could not be added to a [`Module`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AstError {
    /// The node would raise the node count above [`Limits::max_nodes`].
    NodeLimit,
    /// The node would raise the tree height above [`Limits::max_height`].
    HeightLimit,
    /// A child id does not belong to this module.
    UnknownId,
    /// A child is already attached to another parent.
    AlreadyAttached,
}

impl fmt::Display for AstError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            AstError::NodeLimit => "syntax tree node limit exceeded",
            AstError::HeightLimit => "syntax tree height limit exceeded",
            AstError::UnknownId => "node id does not belong to this module",
            AstError::AlreadyAttached => "node is already attached to a parent",
        };
        f.write_str(text)
    }
}

impl std::error::Error for AstError {}

/// A half open range `start..end` of byte offsets into one source file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextRange {
    start: u32,
    end: u32,
}

impl TextRange {
    /// Creates the range `start..end`. Returns `None` if `end < start`.
    pub fn new(start: u32, end: u32) -> Option<TextRange> {
        (start <= end).then_some(TextRange { start, end })
    }

    /// Creates an empty range at `offset`.
    pub fn empty(offset: u32) -> TextRange {
        TextRange {
            start: offset,
            end: offset,
        }
    }

    /// First byte offset of the range.
    pub fn start(self) -> u32 {
        self.start
    }

    /// Byte offset just after the range.
    pub fn end(self) -> u32 {
        self.end
    }

    /// Length in bytes.
    pub fn len(self) -> u32 {
        self.end - self.start
    }

    /// True if the range covers no bytes.
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The smallest range that covers both `self` and `other`.
    pub fn cover(self, other: TextRange) -> TextRange {
        TextRange {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u32);

        impl $name {
            /// Position in the arena, usable as an index into side tables.
            pub fn index(self) -> usize {
                self.0 as usize
            }

            /// The raw id.
            pub fn as_u32(self) -> u32 {
                self.0
            }
        }
    };
}

id_type!(
    /// Id of an [`Expr`] in its [`Module`].
    ExprId
);
id_type!(
    /// Id of a [`Stmt`] in its [`Module`].
    StmtId
);
id_type!(
    /// Id of a [`Block`] in its [`Module`].
    BlockId
);
id_type!(
    /// Id of a [`Ty`] in its [`Module`].
    TyId
);
id_type!(
    /// Id of a [`ViewNode`] in its [`Module`].
    ViewNodeId
);

/// A name as written in the source, for example `fib` or `Int`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Ident {
    /// The characters of the name.
    pub text: Box<str>,
    /// Where the name stands in the source.
    pub range: TextRange,
}

impl Ident {
    /// Creates a name at `range`.
    pub fn new(text: &str, range: TextRange) -> Ident {
        Ident {
            text: text.into(),
            range,
        }
    }

    /// Creates a name with an empty range at offset 0, for tests.
    pub fn bare(text: &str) -> Ident {
        Ident::new(text, TextRange::default())
    }
}

/// A type as written in the source. In v0.1 a type is a single name such as
/// `Int`, `Text` or `Bool`; whether the name is known is decided by the
/// checker.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypeRef {
    /// The type name.
    pub name: Ident,
    /// Where the type stands in the source.
    pub range: TextRange,
}

impl TypeRef {
    /// Creates a type reference whose range is the range of its name.
    pub fn named(name: Ident) -> TypeRef {
        let range = name.range;
        TypeRef { name, range }
    }
}

/// One parameter `name: Type` of a function.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Param {
    /// The parameter name.
    pub name: Ident,
    /// The declared type.
    pub ty: TypeRef,
    /// From the start of the name to the end of the type.
    pub range: TextRange,
}

impl Param {
    /// Creates a parameter whose range covers its name and its type.
    pub fn new(name: Ident, ty: TypeRef) -> Param {
        let range = name.range.cover(ty.range);
        Param { name, ty, range }
    }
}

/// A top level declaration.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Item {
    /// `fn name(params) -> Type` with a body. Boxed so that an item stays
    /// within the node size budget of ARCHITECTURE 3.4.
    Fn(Box<FnDecl>),
    /// `app Name` with its `auth` and `home` lines (SYNTAX 4.1).
    App(Box<AppDecl>),
    /// `enum Name = a | b` (SYNTAX 7).
    Enum(Box<EnumDecl>),
    /// `data Name` with fields, rules and checks (SYNTAX 4.2 to 4.4).
    Data(Box<DataDecl>),
    /// `var name: Type = value`, client state (SYNTAX 4.6).
    Var(Box<VarDecl>),
    /// A function declaration that does not fit [`FnDecl`]: it has a
    /// placement keyword, `as system`, a `call` rule, or a parameter or result
    /// type beyond a single name (SYNTAX 4.6). A declaration that fits
    /// [`FnDecl`] is always stored as [`Item::Fn`], so the v0.1 passes keep
    /// seeing every function they can handle.
    PlacedFn(Box<PlacedFnDecl>),
    /// `view Name(params)` with its element tree (SYNTAX 4.8).
    View(Box<ViewDecl>),
    /// `style` with a CSS body (SYNTAX 4.8).
    Style(Box<StyleDecl>),
}

/// A function declaration: `fn name(a: Int, b: Int) -> Int` and its body.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FnDecl {
    /// The function name.
    pub name: Ident,
    /// Parameters in source order; may be empty.
    pub params: Box<[Param]>,
    /// The result type; `None` if the function returns nothing.
    pub result: Option<TypeRef>,
    /// The indented body.
    pub body: BlockId,
    /// From the `fn` keyword to the end of the body.
    pub range: TextRange,
}

/// An indented sequence of statements.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Block {
    /// Statements in source order; may be empty (SYNTAX 7 allows a block
    /// without statements, and trees built by hand may have one too).
    pub stmts: Box<[StmtId]>,
    /// From the first to the last statement; for an empty block an empty
    /// range at the position where the block would start.
    pub range: TextRange,
    height: u32,
}

/// A statement with its source range.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Stmt {
    /// What kind of statement this is.
    pub kind: StmtKind,
    /// From the first keyword or expression to the end of the statement.
    pub range: TextRange,
    height: u32,
}

/// The statements of the v0.1 slice.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StmtKind {
    /// `let name = value`
    Let {
        /// The bound name.
        name: Ident,
        /// The bound value.
        value: ExprId,
    },
    /// `if cond` with a block and an optional `else` block.
    If {
        /// The condition.
        cond: ExprId,
        /// The block run when the condition holds.
        then_block: BlockId,
        /// The block after `else`, if any.
        else_block: Option<BlockId>,
    },
    /// `return value`. The grammar requires a value (SYNTAX 7).
    Return {
        /// The returned value.
        value: ExprId,
    },
    /// An expression on its own line, for example a call.
    Expr(ExprId),
    /// `if let name = value` with a block and an optional `else` block. The
    /// block runs with `name` bound when `value` is not `none` (SYNTAX 4.10).
    IfLet {
        /// The bound name.
        name: Ident,
        /// The optional value that is unwrapped.
        value: ExprId,
        /// The block run when the value is present.
        then_block: BlockId,
        /// The block after `else`, if any.
        else_block: Option<BlockId>,
    },
    /// `for name in source` with its body. `source` is an
    /// [`ExprKind::Query`] or any other expression (SYNTAX 4.5, D69).
    For {
        /// The loop variable.
        name: Ident,
        /// What is iterated.
        source: ExprId,
        /// The loop body.
        body: BlockId,
    },
    /// `drop source`: deletes one record or every record of a query.
    Drop {
        /// A record expression or an [`ExprKind::Query`].
        source: ExprId,
    },
    /// `target = value`. Which targets are assignable is checked in sema
    /// (SYNTAX 7).
    Assign {
        /// The assigned place, for example `x` or `r.name`.
        target: ExprId,
        /// The new value.
        value: ExprId,
    },
}

/// An expression with its source range.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Expr {
    /// What kind of expression this is.
    pub kind: ExprKind,
    /// The source text of the whole expression.
    pub range: TextRange,
    height: u32,
}

/// The expressions of the v0.1 slice.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ExprKind {
    /// An integer literal. Literals are written without sign, so the value is
    /// never negative; the lexer rejects values above 2^53 minus 1 (G9).
    Int(i64),
    /// `true` or `false`.
    Bool(bool),
    /// A string literal, split into text and interpolations (G1).
    Str(Box<[StrPart]>),
    /// A use of a name, for example a local or a function.
    Name(Ident),
    /// A prefix operator applied to one operand.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        operand: ExprId,
    },
    /// A binary operator. Chains are left associative: `a - b - c` is
    /// `(a - b) - c`.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// The left operand.
        lhs: ExprId,
        /// The right operand.
        rhs: ExprId,
    },
    /// A call `callee(args)`.
    Call {
        /// The called expression; a name in v0.1.
        callee: ExprId,
        /// Arguments in source order; may be empty.
        args: Box<[ExprId]>,
    },
    /// An expression in parentheses. Kept so that the formatter and
    /// diagnostics see what the source says.
    Paren(ExprId),
    /// One of the value keywords `none`, `me`, `now` and `signed`.
    Keyword(KeywordValue),
    /// Field or property access `base.name`, for example `room.members` or
    /// `text.len`.
    Field {
        /// The accessed value.
        base: ExprId,
        /// The field or property name.
        name: Ident,
    },
    /// Index access `base[index]`, for example `t.roles[me]`.
    Index {
        /// The indexed value.
        base: ExprId,
        /// The key or position.
        index: ExprId,
    },
    /// Membership `element in collection`, or `element not in collection`
    /// when `negated` is true. Like the comparisons it does not chain.
    In {
        /// The searched value.
        element: ExprId,
        /// The set, map, list or range searched in.
        collection: ExprId,
        /// True for `not in`.
        negated: bool,
    },
    /// The inclusive range `lo..hi` (B-F14).
    Range {
        /// The first value.
        lo: ExprId,
        /// The last value, included.
        hi: ExprId,
    },
    /// `value catch Kind, Kind`: names the error kinds of a remote call that
    /// a following `??` may turn into its fallback (G11). The parser never
    /// builds an empty list; a tree built by hand may have one.
    Catch {
        /// The remote call or value.
        value: ExprId,
        /// The named error kinds in source order.
        kinds: Box<[Ident]>,
    },
    /// `value ?? fallback`. Chains are left associative.
    Fallback {
        /// The optional or remote value.
        value: ExprId,
        /// The value used when `value` is `none` or failed with a named kind.
        fallback: ExprId,
    },
    /// A set literal `{a, b}` or `{}` (G2).
    Set(Box<[ExprId]>),
    /// A list literal `[a, b]` or `[]` (G2).
    List(Box<[ExprId]>),
    /// A map literal `[k: v]`, or `[:]` when empty (G2).
    Map(Box<[MapEntry]>),
    /// A record value `make Name { f: e }`. Boxed so that an expression stays
    /// within the node size budget.
    Make(Box<MakeExpr>),
    /// A query head with optional clauses (SYNTAX 4.5, D69). The parser builds
    /// it for every bare name in a `source` position, with or without
    /// clauses; sema decides whether the name is a data type, an enum or a
    /// value. Boxed so that an expression stays within the node size budget.
    Query(Box<Query>),
}

/// One piece of a string literal.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum StrPart {
    /// Literal text with escapes already resolved; never empty when built by
    /// the parser.
    Text(Box<str>),
    /// An interpolation `{expr}`.
    Interp(ExprId),
}

/// Prefix operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `-x`
    Neg,
    /// `not x`
    Not,
}

/// Binary operators of the v0.1 slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`, truncates toward zero.
    Div,
    /// `%`, takes the sign of the dividend.
    Rem,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `and`, short circuit.
    And,
    /// `or`, short circuit.
    Or,
}

impl BinaryOp {
    /// The operator as written in the source.
    pub fn as_str(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Rem => "%",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::And => "and",
            BinaryOp::Or => "or",
        }
    }
}

impl UnaryOp {
    /// The operator as written in the source.
    pub fn as_str(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Not => "not",
        }
    }
}

// # v0.2 items
//
// Everything below this line up to `Module` was added for v0.2 (SYNTAX 4.1
// to 4.8, EBNF 7). The v0.1 nodes above are unchanged.

/// Height of a node whose tallest child has height `child`.
fn above(child: u32) -> Result<u32, AstError> {
    child.checked_add(1).ok_or(AstError::HeightLimit)
}

/// A type as written in a v0.2 position: `Name`, `Name[Arg, Arg]` and either
/// of them followed by `?` (SYNTAX 7 `type`). v0.1 function signatures keep
/// using [`TypeRef`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Ty {
    /// The type name, for example `Set` or `Room`.
    pub name: Ident,
    /// Bracketed type arguments in source order; empty without brackets.
    pub args: Box<[TyId]>,
    /// True if the type ends in `?`.
    pub optional: bool,
    /// From the name to the closing `]` or the `?`.
    pub range: TextRange,
    height: u32,
}

/// The value keywords of SYNTAX 6 that stand for a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeywordValue {
    /// `none`, the absent optional value.
    NoneValue,
    /// `me`, the signed principal.
    Me,
    /// `now`, the current time.
    Now,
    /// `signed`, true if a principal is signed in.
    Signed,
}

impl KeywordValue {
    /// The keyword as written in the source.
    pub fn as_str(self) -> &'static str {
        match self {
            KeywordValue::NoneValue => "none",
            KeywordValue::Me => "me",
            KeywordValue::Now => "now",
            KeywordValue::Signed => "signed",
        }
    }
}

/// One `key: value` pair of a map literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MapEntry {
    /// The key.
    pub key: ExprId,
    /// The value.
    pub value: ExprId,
}

/// One `name: value` pair of a `make` expression.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FieldInit {
    /// The field name.
    pub name: Ident,
    /// The field value.
    pub value: ExprId,
    /// From the name to the end of the value.
    pub range: TextRange,
}

impl FieldInit {
    /// Creates a field initializer with an empty range, for tests.
    pub fn bare(name: &str, value: ExprId) -> FieldInit {
        FieldInit {
            name: Ident::bare(name),
            value,
            range: TextRange::default(),
        }
    }
}

/// `make Name { f: e, ... }`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MakeExpr {
    /// The data type of the new record.
    pub ty: Ident,
    /// Field values in source order; may be empty. Duplicates and unknown
    /// fields are reported by sema.
    pub fields: Box<[FieldInit]>,
}

/// A query `Head [where e] [sort f [desc]] [limit n | last n]` (SYNTAX 4.5).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Query {
    /// The bare name the query starts with; sema decides what it names (D69).
    pub head: Ident,
    /// The `where` condition, if any.
    pub filter: Option<ExprId>,
    /// The `sort` key, if any.
    pub sort: Option<SortKey>,
    /// The `limit` or `last` clause, if any.
    pub take: Option<Take>,
}

/// `sort field [desc]`. The tie break by `id` is implied (SYNTAX 4.5).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SortKey {
    /// The sorted field.
    pub field: Ident,
    /// True for `desc`.
    pub desc: bool,
    /// From `sort` to the field or `desc`.
    pub range: TextRange,
}

/// Which rows a `limit` or `last` clause keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TakeKind {
    /// `limit n`: the first n rows in sort order.
    Limit,
    /// `last n`: the last n rows, still in sort order (B2-14).
    Last,
}

impl TakeKind {
    /// The keyword as written in the source.
    pub fn as_str(self) -> &'static str {
        match self {
            TakeKind::Limit => "limit",
            TakeKind::Last => "last",
        }
    }
}

/// `limit n` or `last n`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Take {
    /// `limit` or `last`.
    pub kind: TakeKind,
    /// The number of rows.
    pub count: ExprId,
    /// From the keyword to the end of the count.
    pub range: TextRange,
}

/// The condition of a view `if`: an expression or `let name = value`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Cond {
    /// `if expr`
    Expr(ExprId),
    /// `if let name = value`
    Let {
        /// The bound name.
        name: Ident,
        /// The optional value that is unwrapped.
        value: ExprId,
    },
}

/// One line of an `app` declaration.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AppItem {
    /// What the line says.
    pub kind: AppItemKind,
    /// The whole line without its line end.
    pub range: TextRange,
}

/// The lines an `app` declaration may contain (SYNTAX 7 `appItem`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AppItemKind {
    /// `auth scheme [mode]`, for example `auth paseto open`. Scheme and mode
    /// are ordinary names validated in sema (SYNTAX 6).
    Auth {
        /// The scheme, `paseto` in v0.3.
        scheme: Ident,
        /// The registration mode, `open` or `invite`.
        mode: Option<Ident>,
    },
    /// `home Name`, the view shown first.
    Home(Ident),
}

impl AppItem {
    /// `auth scheme [mode]` with an empty range, for tests.
    pub fn auth(scheme: &str, mode: Option<&str>) -> AppItem {
        AppItem {
            kind: AppItemKind::Auth {
                scheme: Ident::bare(scheme),
                mode: mode.map(Ident::bare),
            },
            range: TextRange::default(),
        }
    }

    /// `home Name` with an empty range, for tests.
    pub fn home(view: &str) -> AppItem {
        AppItem {
            kind: AppItemKind::Home(Ident::bare(view)),
            range: TextRange::default(),
        }
    }
}

/// `app Name` and its lines (SYNTAX 4.1). Repeated lines are kept in source
/// order so that sema can report them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AppDecl {
    /// The app name.
    pub name: Ident,
    /// The indented lines; may be empty.
    pub items: Box<[AppItem]>,
    /// From `app` to the end of the last line.
    pub range: TextRange,
}

/// `enum Name = a | b | c`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EnumDecl {
    /// The enum name.
    pub name: Ident,
    /// Variants in declaration order. The grammar requires at least one;
    /// duplicates are reported by sema.
    pub variants: Box<[Ident]>,
    /// From `enum` to the last variant.
    pub range: TextRange,
}

/// `data Name` with its members (SYNTAX 4.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DataDecl {
    /// The data type name.
    pub name: Ident,
    /// Fields, rules and checks in source order.
    pub members: Box<[Member]>,
    /// From `data` to the end of the last member.
    pub range: TextRange,
}

/// One line of a `data` declaration.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Member {
    /// What the line declares.
    pub kind: MemberKind,
    /// The whole line without its line end.
    pub range: TextRange,
}

/// The members of SYNTAX 7 `member`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MemberKind {
    /// A field.
    Field(FieldDecl),
    /// A rule `verb [field] if cond`.
    Rule(RuleDecl),
    /// `check cond`, an invariant.
    Check(ExprId),
}

impl Member {
    /// A field member with an empty range, for tests.
    pub fn field(field: FieldDecl) -> Member {
        Member {
            kind: MemberKind::Field(field),
            range: TextRange::default(),
        }
    }

    /// `verb [field] if cond` with an empty range, for tests.
    pub fn rule(verb: RuleVerb, field: Option<&str>, cond: ExprId) -> Member {
        Member {
            kind: MemberKind::Rule(RuleDecl {
                verb,
                field: field.map(Ident::bare),
                cond,
            }),
            range: TextRange::default(),
        }
    }

    /// `check cond` with an empty range, for tests.
    pub fn check(cond: ExprId) -> Member {
        Member {
            kind: MemberKind::Check(cond),
            range: TextRange::default(),
        }
    }
}

/// `name: Type [= default] [mode] [merge strategy]` (SYNTAX 4.2, 4.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FieldDecl {
    /// The field name.
    pub name: Ident,
    /// The declared type.
    pub ty: TyId,
    /// The default value after `=`, if any.
    pub default: Option<ExprId>,
    /// The write mode, if any.
    pub mode: Option<FieldMode>,
    /// The name after `merge`, for example `text`; validated in sema.
    pub merge: Option<Ident>,
}

impl FieldDecl {
    /// A field without default, mode or merge strategy.
    pub fn new(name: &str, ty: TyId) -> FieldDecl {
        FieldDecl {
            name: Ident::bare(name),
            ty,
            default: None,
            mode: None,
            merge: None,
        }
    }
}

/// Field write modes (SYNTAX 4.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FieldMode {
    /// `fixed`: immutable after `make`.
    Fixed,
    /// `server`: only server code writes it.
    Server,
    /// `serial [per field]`: a server assigned number.
    Serial {
        /// The field after `per`, if any.
        per: Option<Ident>,
    },
}

/// The verbs of a data rule (SYNTAX 4.4). `call` rules belong to functions,
/// see [`CallRule`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuleVerb {
    /// `see`
    See,
    /// `make`
    Make,
    /// `edit`
    Edit,
    /// `drop`
    Drop,
}

impl RuleVerb {
    /// The verb as written in the source.
    pub fn as_str(self) -> &'static str {
        match self {
            RuleVerb::See => "see",
            RuleVerb::Make => "make",
            RuleVerb::Edit => "edit",
            RuleVerb::Drop => "drop",
        }
    }
}

/// `verb [field] if cond`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RuleDecl {
    /// The guarded operation.
    pub verb: RuleVerb,
    /// The field of a field rule, for example `members` in
    /// `edit members if ...`; `None` for a record rule.
    pub field: Option<Ident>,
    /// The condition after `if`.
    pub cond: ExprId,
}

/// How a `var` gets its type and value. The grammar needs a type, a value
/// or both, so a declaration with neither cannot be built.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum VarInit {
    /// `var name: Type [= value]`
    Typed {
        /// The declared type.
        ty: TyId,
        /// The initial value, if any.
        value: Option<ExprId>,
    },
    /// `var name = value`
    Inferred(ExprId),
}

/// `var name ...`, client state (SYNTAX 4.6).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VarDecl {
    /// The variable name.
    pub name: Ident,
    /// Type and initial value.
    pub init: VarInit,
    /// From `var` to the end of the line.
    pub range: TextRange,
}

/// Where a function runs (SYNTAX 4.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Placement {
    /// No keyword: the compiler places the function.
    Inferred,
    /// `server fn`
    Server,
    /// `client fn`
    Client,
}

impl Placement {
    /// The keyword as written in the source; empty for [`Placement::Inferred`].
    pub fn as_str(self) -> &'static str {
        match self {
            Placement::Inferred => "",
            Placement::Server => "server",
            Placement::Client => "client",
        }
    }
}

/// One parameter `name: Type` of a placed function or a view.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypedParam {
    /// The parameter name.
    pub name: Ident,
    /// The declared type.
    pub ty: TyId,
    /// From the start of the name to the end of the type.
    pub range: TextRange,
}

impl TypedParam {
    /// A parameter with an empty range, for tests.
    pub fn bare(name: &str, ty: TyId) -> TypedParam {
        TypedParam {
            name: Ident::bare(name),
            ty,
            range: TextRange::default(),
        }
    }
}

/// `call if cond` as the first body line of a function (SYNTAX 4.6, D19).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CallRule {
    /// The condition after `if`.
    pub cond: ExprId,
    /// From `call` to the end of the condition.
    pub range: TextRange,
}

/// A function with placement, `call` rule or v0.2 types (SYNTAX 4.6). The
/// `call` rule is kept apart from the body statements.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PlacedFnDecl {
    /// `server`, `client` or no keyword.
    pub placement: Placement,
    /// The function name.
    pub name: Ident,
    /// Parameters in source order; may be empty.
    pub params: Box<[TypedParam]>,
    /// The result type; `None` if the function returns nothing.
    pub result: Option<TyId>,
    /// The range of `as system` if the function is elevated (G13, D34).
    /// Whether elevation is allowed here is decided in sema.
    pub elevated: Option<TextRange>,
    /// The `call` rule, if any.
    pub call_rule: Option<CallRule>,
    /// The statements after the `call` rule.
    pub body: BlockId,
    /// From the first keyword to the end of the body.
    pub range: TextRange,
}

/// `view Name [(params)]` and its nodes (SYNTAX 4.8).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ViewDecl {
    /// The view name.
    pub name: Ident,
    /// `None` without parentheses, `Some` with them, even when empty, so the
    /// formatter keeps what the source says.
    pub params: Option<Box<[TypedParam]>>,
    /// Top level nodes in source order.
    pub nodes: Box<[ViewNodeId]>,
    /// From `view` to the end of the last node.
    pub range: TextRange,
}

/// `style` with its CSS body (SYNTAX 3, 4.8). The body is lexed in CSS mode
/// and kept as source text; the formatter owns its layout.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StyleDecl {
    /// The CSS text between the indent and the dedent.
    pub body: TextRange,
    /// From `style` to the end of the body.
    pub range: TextRange,
}

/// A node of a view tree with its source range.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ViewNode {
    /// What kind of node this is.
    pub kind: ViewNodeKind,
    /// From the first token of the node to the end of its last child.
    pub range: TextRange,
    height: u32,
}

/// The nodes of SYNTAX 7 `node`. Each payload is boxed so that a view node
/// stays within the node size budget.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ViewNodeKind {
    /// An element line with its children.
    Element(Box<Element>),
    /// `if cond` with nodes and optional `else` nodes.
    If(Box<ViewIf>),
    /// `for name in source` with nodes.
    For(Box<ViewFor>),
}

/// `element [value] {attr} [handler]` and its indented children
/// (SYNTAX 4.8). The element name is not checked by the parser (B-F5).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Element {
    /// The element name, for example `row` or a view name.
    pub name: Ident,
    /// The value after the name, if any.
    pub value: Option<ExprId>,
    /// Attributes in source order.
    pub attrs: Box<[Attr]>,
    /// The `on` handler at the end of the element line, if any.
    pub handler: Option<Handler>,
    /// Child nodes and handler lines in source order.
    pub children: Box<[ElementChild]>,
}

/// A line indented below an element.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ElementChild {
    /// A child node.
    Node(ViewNodeId),
    /// An `on` line.
    Handler(Handler),
}

/// An attribute of an element.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Attr {
    /// What the attribute says.
    pub kind: AttrKind,
    /// From the name or `~` to the end of the value or condition.
    pub range: TextRange,
}

/// The attributes of SYNTAX 7 `attr`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AttrKind {
    /// `name: value`. Names are validated in sema (B-F5).
    Named {
        /// The attribute name.
        name: Ident,
        /// The value.
        value: ExprId,
    },
    /// `~name` or `~name(cond)`: a style tag.
    Style {
        /// The style tag name.
        name: Ident,
        /// The condition in parentheses, if any.
        cond: Option<ExprId>,
    },
}

impl Attr {
    /// `name: value` with an empty range, for tests.
    pub fn named(name: &str, value: ExprId) -> Attr {
        Attr {
            kind: AttrKind::Named {
                name: Ident::bare(name),
                value,
            },
            range: TextRange::default(),
        }
    }

    /// `~name` or `~name(cond)` with an empty range, for tests.
    pub fn style(name: &str, cond: Option<ExprId>) -> Attr {
        Attr {
            kind: AttrKind::Style {
                name: Ident::bare(name),
                cond,
            },
            range: TextRange::default(),
        }
    }
}

/// `on event action` or `on event target = value` (SYNTAX 7 `handler`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Handler {
    /// The event name, for example `click` or `submit`.
    pub event: Ident,
    /// The action expression, or the assigned place when `assign` is set.
    pub action: ExprId,
    /// The value after `=`, if the handler is an assignment.
    pub assign: Option<ExprId>,
    /// From `on` to the end of the action.
    pub range: TextRange,
}

impl Handler {
    /// A handler with an empty range, for tests.
    pub fn bare(event: &str, action: ExprId, assign: Option<ExprId>) -> Handler {
        Handler {
            event: Ident::bare(event),
            action,
            assign,
            range: TextRange::default(),
        }
    }
}

/// `if cond` in a view.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ViewIf {
    /// The condition.
    pub cond: Cond,
    /// Nodes shown when the condition holds.
    pub then_nodes: Box<[ViewNodeId]>,
    /// Nodes after `else`, if any.
    pub else_nodes: Option<Box<[ViewNodeId]>>,
}

/// `for name in source` in a view; a query source is live (SYNTAX 4.5).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ViewFor {
    /// The loop variable.
    pub name: Ident,
    /// What is iterated.
    pub source: ExprId,
    /// Nodes shown per item.
    pub nodes: Box<[ViewNodeId]>,
}

/// The syntax tree of one source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    items: Vec<Item>,
    exprs: Vec<Expr>,
    stmts: Vec<Stmt>,
    blocks: Vec<Block>,
    expr_attached: Vec<bool>,
    stmt_attached: Vec<bool>,
    block_attached: Vec<bool>,
    tys: Vec<Ty>,
    ty_attached: Vec<bool>,
    view_nodes: Vec<ViewNode>,
    view_node_attached: Vec<bool>,
    nodes: u32,
    height: u32,
    limits: Limits,
}

/// Children marked as attached while a node is being added, so the marks
/// can be cleared if the node is rejected.
#[derive(Default)]
struct Attached {
    exprs: Vec<u32>,
    stmts: Vec<u32>,
    blocks: Vec<u32>,
    tys: Vec<u32>,
    view_nodes: Vec<u32>,
}

impl Default for Module {
    fn default() -> Self {
        Module::new()
    }
}

impl Module {
    /// An empty module with the default [`Limits`].
    pub fn new() -> Module {
        Module::with_limits(Limits::DEFAULT)
    }

    /// An empty module with the given limits. The empty module already counts
    /// its root: one node, height 1. A `max_nodes` of 0 or a `max_height`
    /// below 2 is accepted but leaves no room for any further node, so every
    /// `add_*` call fails.
    pub fn with_limits(limits: Limits) -> Module {
        Module {
            items: Vec::new(),
            exprs: Vec::new(),
            stmts: Vec::new(),
            blocks: Vec::new(),
            expr_attached: Vec::new(),
            stmt_attached: Vec::new(),
            block_attached: Vec::new(),
            tys: Vec::new(),
            ty_attached: Vec::new(),
            view_nodes: Vec::new(),
            view_node_attached: Vec::new(),
            nodes: 1,
            height: 1,
            limits,
        }
    }

    /// The limits this module enforces.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Top level declarations in source order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Number of nodes, module root included.
    pub fn node_count(&self) -> u32 {
        self.nodes
    }

    /// Height of the tree, module root included.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The expression with id `id`, or `None` if the id is not from this
    /// module.
    pub fn expr(&self, id: ExprId) -> Option<&Expr> {
        self.exprs.get(id.index())
    }

    /// The statement with id `id`, or `None` if the id is not from this
    /// module.
    pub fn stmt(&self, id: StmtId) -> Option<&Stmt> {
        self.stmts.get(id.index())
    }

    /// The block with id `id`, or `None` if the id is not from this module.
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.get(id.index())
    }

    /// Height of the subtree rooted at expression `id`.
    pub fn expr_height(&self, id: ExprId) -> Option<u32> {
        self.expr(id).map(|e| e.height)
    }

    /// Height of the subtree rooted at statement `id`.
    pub fn stmt_height(&self, id: StmtId) -> Option<u32> {
        self.stmt(id).map(|s| s.height)
    }

    /// Height of the subtree rooted at block `id`.
    pub fn block_height(&self, id: BlockId) -> Option<u32> {
        self.block(id).map(|b| b.height)
    }

    /// The type with id `id`, or `None` if the id is not from this module.
    pub fn ty(&self, id: TyId) -> Option<&Ty> {
        self.tys.get(id.index())
    }

    /// The view node with id `id`, or `None` if the id is not from this
    /// module.
    pub fn view_node(&self, id: ViewNodeId) -> Option<&ViewNode> {
        self.view_nodes.get(id.index())
    }

    /// Height of the subtree rooted at type `id`.
    pub fn ty_height(&self, id: TyId) -> Option<u32> {
        self.ty(id).map(|t| t.height)
    }

    /// Height of the subtree rooted at view node `id`.
    pub fn view_node_height(&self, id: ViewNodeId) -> Option<u32> {
        self.view_node(id).map(|n| n.height)
    }

    // Limit and tree checks shared by all `add_*` methods.

    fn reserve_nodes(&self, extra: usize) -> Result<u32, AstError> {
        let extra = u32::try_from(extra).map_err(|_| AstError::NodeLimit)?;
        let total = self.nodes.checked_add(extra).ok_or(AstError::NodeLimit)?;
        if total > self.limits.max_nodes {
            return Err(AstError::NodeLimit);
        }
        Ok(total)
    }

    /// Checks a subtree height. The module root sits above every node, so a
    /// node of height `h` makes the whole tree at least `h + 1` high.
    fn check_height(&self, height: u32) -> Result<(), AstError> {
        match height.checked_add(1) {
            Some(total) if total <= self.limits.max_height => Ok(()),
            _ => Err(AstError::HeightLimit),
        }
    }

    fn next_id(len: usize) -> Result<u32, AstError> {
        u32::try_from(len).map_err(|_| AstError::NodeLimit)
    }

    // Attaching children. Each child is marked as attached as soon as it is
    // checked, so a repeated child is found in O(1) and adding a node is
    // linear in its number of children. If the node is rejected later, the
    // marks recorded in `Attached` are cleared again.

    fn attach_expr(&mut self, id: ExprId, att: &mut Attached) -> Result<u32, AstError> {
        let height = self.expr_height(id).ok_or(AstError::UnknownId)?;
        let flag = self
            .expr_attached
            .get_mut(id.index())
            .ok_or(AstError::UnknownId)?;
        if *flag {
            return Err(AstError::AlreadyAttached);
        }
        *flag = true;
        att.exprs.push(id.0);
        Ok(height)
    }

    fn attach_stmt(&mut self, id: StmtId, att: &mut Attached) -> Result<u32, AstError> {
        let height = self.stmt_height(id).ok_or(AstError::UnknownId)?;
        let flag = self
            .stmt_attached
            .get_mut(id.index())
            .ok_or(AstError::UnknownId)?;
        if *flag {
            return Err(AstError::AlreadyAttached);
        }
        *flag = true;
        att.stmts.push(id.0);
        Ok(height)
    }

    fn attach_block(&mut self, id: BlockId, att: &mut Attached) -> Result<u32, AstError> {
        let height = self.block_height(id).ok_or(AstError::UnknownId)?;
        let flag = self
            .block_attached
            .get_mut(id.index())
            .ok_or(AstError::UnknownId)?;
        if *flag {
            return Err(AstError::AlreadyAttached);
        }
        *flag = true;
        att.blocks.push(id.0);
        Ok(height)
    }

    fn attach_ty(&mut self, id: TyId, att: &mut Attached) -> Result<u32, AstError> {
        let height = self.ty_height(id).ok_or(AstError::UnknownId)?;
        let flag = self
            .ty_attached
            .get_mut(id.index())
            .ok_or(AstError::UnknownId)?;
        if *flag {
            return Err(AstError::AlreadyAttached);
        }
        *flag = true;
        att.tys.push(id.0);
        Ok(height)
    }

    fn attach_view_node(&mut self, id: ViewNodeId, att: &mut Attached) -> Result<u32, AstError> {
        let height = self.view_node_height(id).ok_or(AstError::UnknownId)?;
        let flag = self
            .view_node_attached
            .get_mut(id.index())
            .ok_or(AstError::UnknownId)?;
        if *flag {
            return Err(AstError::AlreadyAttached);
        }
        *flag = true;
        att.view_nodes.push(id.0);
        Ok(height)
    }

    /// Attaches every expression of `ids` and returns the largest height,
    /// 0 for none.
    fn attach_exprs(
        &mut self,
        ids: impl IntoIterator<Item = ExprId>,
        att: &mut Attached,
    ) -> Result<u32, AstError> {
        let mut max = 0;
        for id in ids {
            max = max.max(self.attach_expr(id, att)?);
        }
        Ok(max)
    }

    /// Attaches `id` if present; an absent child has height 0.
    fn attach_opt_expr(&mut self, id: Option<ExprId>, att: &mut Attached) -> Result<u32, AstError> {
        match id {
            Some(id) => self.attach_expr(id, att),
            None => Ok(0),
        }
    }

    /// Attaches every view node of `ids` and returns the largest height.
    fn attach_view_nodes(
        &mut self,
        ids: &[ViewNodeId],
        att: &mut Attached,
    ) -> Result<u32, AstError> {
        let mut max = 0;
        for id in ids {
            max = max.max(self.attach_view_node(*id, att)?);
        }
        Ok(max)
    }

    /// Clears the marks of a rejected node, so its children stay free.
    fn detach(&mut self, att: &Attached) {
        let tables = [
            (&mut self.expr_attached, &att.exprs),
            (&mut self.stmt_attached, &att.stmts),
            (&mut self.block_attached, &att.blocks),
            (&mut self.ty_attached, &att.tys),
            (&mut self.view_node_attached, &att.view_nodes),
        ];
        for (flags, ids) in tables {
            for id in ids {
                if let Some(flag) = flags.get_mut(*id as usize) {
                    *flag = false;
                }
            }
        }
    }

    /// Runs `add` and clears the attach marks it set if it fails.
    fn transact<T>(
        &mut self,
        add: impl FnOnce(&mut Module, &mut Attached) -> Result<T, AstError>,
    ) -> Result<T, AstError> {
        let mut att = Attached::default();
        let result = add(self, &mut att);
        if result.is_err() {
            self.detach(&att);
        }
        result
    }

    /// Adds an expression and returns its id. All child ids must come from
    /// this module and must not have a parent yet. On error the module is
    /// unchanged.
    pub fn add_expr(&mut self, kind: ExprKind, range: TextRange) -> Result<ExprId, AstError> {
        self.transact(|m, att| {
            let (child_height, extra) = m.attach_expr_children(&kind, att)?;
            let height = above(child_height)?;
            m.check_height(height)?;
            let nodes = m.reserve_nodes(extra.checked_add(1).ok_or(AstError::NodeLimit)?)?;
            let id = Self::next_id(m.exprs.len())?;
            m.exprs.push(Expr {
                kind,
                range,
                height,
            });
            m.expr_attached.push(false);
            m.nodes = nodes;
            m.height = m.height.max(height + 1);
            Ok(ExprId(id))
        })
    }

    /// Attaches the children of an expression of kind `kind`. Returns the
    /// height of the tallest child, inline parts included, and the number of
    /// inline parts that count as nodes.
    fn attach_expr_children(
        &mut self,
        kind: &ExprKind,
        att: &mut Attached,
    ) -> Result<(u32, usize), AstError> {
        let m = self;
        let child_height = match kind {
            ExprKind::Int(_) | ExprKind::Bool(_) | ExprKind::Name(_) | ExprKind::Keyword(_) => 0,
            ExprKind::Str(parts) => {
                let mut max = 0;
                for part in parts.iter() {
                    if let StrPart::Interp(id) = part {
                        max = max.max(m.attach_expr(*id, att)?);
                    }
                }
                max
            }
            ExprKind::Unary { operand, .. } => m.attach_expr(*operand, att)?,
            ExprKind::Binary { lhs, rhs, .. } => {
                let l = m.attach_expr(*lhs, att)?;
                let r = m.attach_expr(*rhs, att)?;
                l.max(r)
            }
            ExprKind::Call { callee, args } => {
                let mut max = m.attach_expr(*callee, att)?;
                for arg in args.iter() {
                    max = max.max(m.attach_expr(*arg, att)?);
                }
                max
            }
            ExprKind::Paren(inner) => m.attach_expr(*inner, att)?,
            ExprKind::Field { base, .. } => m.attach_expr(*base, att)?,
            ExprKind::Index { base, index } => m.attach_exprs([*base, *index], att)?,
            ExprKind::In {
                element,
                collection,
                ..
            } => m.attach_exprs([*element, *collection], att)?,
            ExprKind::Range { lo, hi } => m.attach_exprs([*lo, *hi], att)?,
            ExprKind::Catch { value, kinds } => {
                let value = m.attach_expr(*value, att)?;
                let kinds_height = u32::from(!kinds.is_empty());
                return Ok((value.max(kinds_height), kinds.len()));
            }
            ExprKind::Fallback { value, fallback } => m.attach_exprs([*value, *fallback], att)?,
            ExprKind::Set(items) | ExprKind::List(items) => {
                m.attach_exprs(items.iter().copied(), att)?
            }
            ExprKind::Map(entries) => {
                let mut max = 0;
                for entry in entries.iter() {
                    let inner = m.attach_exprs([entry.key, entry.value], att)?;
                    max = max.max(above(inner)?);
                }
                return Ok((max, entries.len()));
            }
            ExprKind::Make(make) => {
                let mut max = 0;
                for init in make.fields.iter() {
                    max = max.max(above(m.attach_expr(init.value, att)?)?);
                }
                return Ok((max, make.fields.len()));
            }
            ExprKind::Query(query) => {
                let mut max = m.attach_opt_expr(query.filter, att)?;
                let mut extra = 0;
                if query.sort.is_some() {
                    max = max.max(1);
                    extra += 1;
                }
                if let Some(take) = &query.take {
                    max = max.max(above(m.attach_expr(take.count, att)?)?);
                    extra += 1;
                }
                return Ok((max, extra));
            }
        };
        Ok((child_height, 0))
    }

    /// Adds a statement and returns its id. All child ids must come from
    /// this module and must not have a parent yet. On error the module is
    /// unchanged.
    pub fn add_stmt(&mut self, kind: StmtKind, range: TextRange) -> Result<StmtId, AstError> {
        self.transact(|m, att| {
            let child_height = match &kind {
                StmtKind::Let { value, .. } => m.attach_expr(*value, att)?,
                StmtKind::Return { value } => m.attach_expr(*value, att)?,
                StmtKind::Expr(value) => m.attach_expr(*value, att)?,
                StmtKind::If {
                    cond,
                    then_block,
                    else_block,
                } => {
                    let mut max = m.attach_expr(*cond, att)?;
                    max = max.max(m.attach_block(*then_block, att)?);
                    if let Some(else_block) = else_block {
                        max = max.max(m.attach_block(*else_block, att)?);
                    }
                    max
                }
                StmtKind::IfLet {
                    value,
                    then_block,
                    else_block,
                    ..
                } => {
                    let mut max = m.attach_expr(*value, att)?;
                    max = max.max(m.attach_block(*then_block, att)?);
                    if let Some(else_block) = else_block {
                        max = max.max(m.attach_block(*else_block, att)?);
                    }
                    max
                }
                StmtKind::For { source, body, .. } => {
                    let source = m.attach_expr(*source, att)?;
                    source.max(m.attach_block(*body, att)?)
                }
                StmtKind::Drop { source } => m.attach_expr(*source, att)?,
                StmtKind::Assign { target, value } => m.attach_exprs([*target, *value], att)?,
            };
            let height = child_height.checked_add(1).ok_or(AstError::HeightLimit)?;
            m.check_height(height)?;
            let nodes = m.reserve_nodes(1)?;
            let id = Self::next_id(m.stmts.len())?;
            m.stmts.push(Stmt {
                kind,
                range,
                height,
            });
            m.stmt_attached.push(false);
            m.nodes = nodes;
            m.height = m.height.max(height + 1);
            Ok(StmtId(id))
        })
    }

    /// Adds a block of statements and returns its id. An empty block has
    /// height 1. On error the module is unchanged.
    pub fn add_block(&mut self, stmts: Vec<StmtId>, range: TextRange) -> Result<BlockId, AstError> {
        self.transact(|m, att| {
            let mut max = 0;
            for stmt in &stmts {
                max = max.max(m.attach_stmt(*stmt, att)?);
            }
            let height = max.checked_add(1).ok_or(AstError::HeightLimit)?;
            m.check_height(height)?;
            let nodes = m.reserve_nodes(1)?;
            let id = Self::next_id(m.blocks.len())?;
            m.blocks.push(Block {
                stmts: stmts.into_boxed_slice(),
                range,
                height,
            });
            m.block_attached.push(false);
            m.nodes = nodes;
            m.height = m.height.max(height + 1);
            Ok(BlockId(id))
        })
    }

    /// Appends a function declaration as the next top level item. Counts one
    /// node for the item, two per parameter (the parameter and its type) and
    /// one for the result type. On error the module is unchanged.
    pub fn add_fn(&mut self, decl: FnDecl) -> Result<(), AstError> {
        self.add_item(Item::Fn(Box::new(decl)))
    }

    /// Appends any top level item. Counts one node for the item plus its
    /// inline parts as described in the [module documentation](self). All
    /// child ids must come from this module and must not have a parent yet.
    /// On error the module is unchanged.
    pub fn add_item(&mut self, item: Item) -> Result<(), AstError> {
        self.transact(|m, att| {
            let (child_height, extra) = m.attach_item_children(&item, att)?;
            let height = above(child_height)?;
            m.check_height(height)?;
            let nodes = m.reserve_nodes(extra.checked_add(1).ok_or(AstError::NodeLimit)?)?;
            m.items.push(item);
            m.nodes = nodes;
            m.height = m.height.max(height + 1);
            Ok(())
        })
    }

    /// Attaches the children of `item`. Returns the height of the tallest
    /// child, inline parts included, and the number of inline parts that
    /// count as nodes.
    fn attach_item_children(
        &mut self,
        item: &Item,
        att: &mut Attached,
    ) -> Result<(u32, usize), AstError> {
        let m = self;
        match item {
            Item::Fn(decl) => {
                let body = m.attach_block(decl.body, att)?;
                // A parameter has its type below it, so it is two levels high.
                let params_height = if decl.params.is_empty() { 0 } else { 2 };
                let result_height = u32::from(decl.result.is_some());
                let extra = decl
                    .params
                    .len()
                    .checked_mul(2)
                    .and_then(|n| n.checked_add(usize::from(decl.result.is_some())))
                    .ok_or(AstError::NodeLimit)?;
                Ok((body.max(params_height).max(result_height), extra))
            }
            Item::App(app) => Ok((u32::from(!app.items.is_empty()), app.items.len())),
            Item::Enum(decl) => Ok((u32::from(!decl.variants.is_empty()), decl.variants.len())),
            Item::Data(decl) => {
                let mut max = 0;
                for member in decl.members.iter() {
                    let inner = match &member.kind {
                        MemberKind::Field(field) => {
                            let ty = m.attach_ty(field.ty, att)?;
                            ty.max(m.attach_opt_expr(field.default, att)?)
                        }
                        MemberKind::Rule(rule) => m.attach_expr(rule.cond, att)?,
                        MemberKind::Check(cond) => m.attach_expr(*cond, att)?,
                    };
                    max = max.max(above(inner)?);
                }
                Ok((max, decl.members.len()))
            }
            Item::Var(decl) => {
                let height = match &decl.init {
                    VarInit::Typed { ty, value } => {
                        let ty = m.attach_ty(*ty, att)?;
                        ty.max(m.attach_opt_expr(*value, att)?)
                    }
                    VarInit::Inferred(value) => m.attach_expr(*value, att)?,
                };
                Ok((height, 0))
            }
            Item::PlacedFn(decl) => {
                let mut max = m.attach_block(decl.body, att)?;
                max = max.max(m.attach_params(&decl.params, att)?);
                if let Some(result) = decl.result {
                    max = max.max(m.attach_ty(result, att)?);
                }
                if let Some(rule) = &decl.call_rule {
                    max = max.max(above(m.attach_expr(rule.cond, att)?)?);
                }
                let extra = decl
                    .params
                    .len()
                    .checked_add(usize::from(decl.call_rule.is_some()))
                    .ok_or(AstError::NodeLimit)?;
                Ok((max, extra))
            }
            Item::View(decl) => {
                let params: &[TypedParam] = decl.params.as_deref().unwrap_or(&[]);
                let mut max = m.attach_params(params, att)?;
                max = max.max(m.attach_view_nodes(&decl.nodes, att)?);
                Ok((max, params.len()))
            }
            Item::Style(_) => Ok((0, 0)),
        }
    }

    /// Attaches the types of `params`; each parameter is one level above its
    /// type.
    fn attach_params(
        &mut self,
        params: &[TypedParam],
        att: &mut Attached,
    ) -> Result<u32, AstError> {
        let mut max = 0;
        for param in params {
            max = max.max(above(self.attach_ty(param.ty, att)?)?);
        }
        Ok(max)
    }

    /// Adds a type `Name[args]?` and returns its id. All argument ids must
    /// come from this module and must not have a parent yet. On error the
    /// module is unchanged.
    pub fn add_ty(
        &mut self,
        name: Ident,
        args: Vec<TyId>,
        optional: bool,
        range: TextRange,
    ) -> Result<TyId, AstError> {
        self.transact(|m, att| {
            let mut max = 0;
            for arg in &args {
                max = max.max(m.attach_ty(*arg, att)?);
            }
            let height = above(max)?;
            m.check_height(height)?;
            let nodes = m.reserve_nodes(1)?;
            let id = Self::next_id(m.tys.len())?;
            m.tys.push(Ty {
                name,
                args: args.into_boxed_slice(),
                optional,
                range,
                height,
            });
            m.ty_attached.push(false);
            m.nodes = nodes;
            m.height = m.height.max(height + 1);
            Ok(TyId(id))
        })
    }

    /// Adds a view node and returns its id. All child ids must come from
    /// this module and must not have a parent yet. On error the module is
    /// unchanged.
    pub fn add_view_node(
        &mut self,
        kind: ViewNodeKind,
        range: TextRange,
    ) -> Result<ViewNodeId, AstError> {
        self.transact(|m, att| {
            let (child_height, extra) = m.attach_view_children(&kind, att)?;
            let height = above(child_height)?;
            m.check_height(height)?;
            let nodes = m.reserve_nodes(extra.checked_add(1).ok_or(AstError::NodeLimit)?)?;
            let id = Self::next_id(m.view_nodes.len())?;
            m.view_nodes.push(ViewNode {
                kind,
                range,
                height,
            });
            m.view_node_attached.push(false);
            m.nodes = nodes;
            m.height = m.height.max(height + 1);
            Ok(ViewNodeId(id))
        })
    }

    fn attach_view_children(
        &mut self,
        kind: &ViewNodeKind,
        att: &mut Attached,
    ) -> Result<(u32, usize), AstError> {
        let m = self;
        match kind {
            ViewNodeKind::Element(element) => {
                let mut max = m.attach_opt_expr(element.value, att)?;
                let mut extra = element.attrs.len();
                for attr in element.attrs.iter() {
                    let inner = match &attr.kind {
                        AttrKind::Named { value, .. } => m.attach_expr(*value, att)?,
                        AttrKind::Style { cond, .. } => m.attach_opt_expr(*cond, att)?,
                    };
                    max = max.max(above(inner)?);
                }
                if let Some(handler) = &element.handler {
                    max = max.max(m.attach_handler(handler, att)?);
                    extra += 1;
                }
                for child in element.children.iter() {
                    let height = match child {
                        ElementChild::Node(id) => m.attach_view_node(*id, att)?,
                        ElementChild::Handler(handler) => {
                            extra += 1;
                            m.attach_handler(handler, att)?
                        }
                    };
                    max = max.max(height);
                }
                Ok((max, extra))
            }
            ViewNodeKind::If(node) => {
                let mut max = match &node.cond {
                    Cond::Expr(cond) => m.attach_expr(*cond, att)?,
                    Cond::Let { value, .. } => m.attach_expr(*value, att)?,
                };
                max = max.max(m.attach_view_nodes(&node.then_nodes, att)?);
                if let Some(else_nodes) = &node.else_nodes {
                    max = max.max(m.attach_view_nodes(else_nodes, att)?);
                }
                Ok((max, 0))
            }
            ViewNodeKind::For(node) => {
                let source = m.attach_expr(node.source, att)?;
                Ok((source.max(m.attach_view_nodes(&node.nodes, att)?), 0))
            }
        }
    }

    /// Attaches the expressions of a handler; the handler is one level above
    /// them.
    fn attach_handler(&mut self, handler: &Handler, att: &mut Attached) -> Result<u32, AstError> {
        let action = self.attach_expr(handler.action, att)?;
        let assign = self.attach_opt_expr(handler.assign, att)?;
        above(action.max(assign))
    }

    // Constructors for tests and tools. Every node gets an empty range at
    // offset 0.

    /// Adds an integer literal.
    pub fn int(&mut self, value: i64) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Int(value), TextRange::default())
    }

    /// Adds `true` or `false`.
    pub fn bool(&mut self, value: bool) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Bool(value), TextRange::default())
    }

    /// Adds a string literal without interpolation.
    pub fn text(&mut self, value: &str) -> Result<ExprId, AstError> {
        let parts: Box<[StrPart]> = if value.is_empty() {
            Box::new([])
        } else {
            Box::new([StrPart::Text(value.into())])
        };
        self.add_expr(ExprKind::Str(parts), TextRange::default())
    }

    /// Adds a string literal made of the given parts.
    pub fn string(&mut self, parts: Vec<StrPart>) -> Result<ExprId, AstError> {
        self.add_expr(
            ExprKind::Str(parts.into_boxed_slice()),
            TextRange::default(),
        )
    }

    /// Adds a use of the name `name`.
    pub fn name(&mut self, name: &str) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Name(Ident::bare(name)), TextRange::default())
    }

    /// Adds a prefix operator expression.
    pub fn unary(&mut self, op: UnaryOp, operand: ExprId) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Unary { op, operand }, TextRange::default())
    }

    /// Adds a binary operator expression.
    pub fn binary(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Binary { op, lhs, rhs }, TextRange::default())
    }

    /// Adds a call of the function named `callee`.
    pub fn call(&mut self, callee: &str, args: Vec<ExprId>) -> Result<ExprId, AstError> {
        let callee = self.name(callee)?;
        self.add_expr(
            ExprKind::Call {
                callee,
                args: args.into_boxed_slice(),
            },
            TextRange::default(),
        )
    }

    /// Adds `(inner)`.
    pub fn paren(&mut self, inner: ExprId) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Paren(inner), TextRange::default())
    }

    /// Adds `let name = value`.
    pub fn let_stmt(&mut self, name: &str, value: ExprId) -> Result<StmtId, AstError> {
        let kind = StmtKind::Let {
            name: Ident::bare(name),
            value,
        };
        self.add_stmt(kind, TextRange::default())
    }

    /// Adds `return value`.
    pub fn return_stmt(&mut self, value: ExprId) -> Result<StmtId, AstError> {
        self.add_stmt(StmtKind::Return { value }, TextRange::default())
    }

    /// Adds an expression statement.
    pub fn expr_stmt(&mut self, value: ExprId) -> Result<StmtId, AstError> {
        self.add_stmt(StmtKind::Expr(value), TextRange::default())
    }

    /// Adds `if cond` with its blocks.
    pub fn if_stmt(
        &mut self,
        cond: ExprId,
        then_block: BlockId,
        else_block: Option<BlockId>,
    ) -> Result<StmtId, AstError> {
        let kind = StmtKind::If {
            cond,
            then_block,
            else_block,
        };
        self.add_stmt(kind, TextRange::default())
    }

    /// Adds a block of statements.
    pub fn block_of(&mut self, stmts: Vec<StmtId>) -> Result<BlockId, AstError> {
        self.add_block(stmts, TextRange::default())
    }

    /// Appends `fn name(params) -> result` with the given body. Parameters
    /// are pairs of name and type name, for example `("n", "Int")`.
    pub fn fn_item(
        &mut self,
        name: &str,
        params: &[(&str, &str)],
        result: Option<&str>,
        body: BlockId,
    ) -> Result<(), AstError> {
        let params = params
            .iter()
            .map(|(name, ty)| Param::new(Ident::bare(name), TypeRef::named(Ident::bare(ty))))
            .collect();
        self.add_fn(FnDecl {
            name: Ident::bare(name),
            params,
            result: result.map(|ty| TypeRef::named(Ident::bare(ty))),
            body,
            range: TextRange::default(),
        })
    }

    // Constructors for the v0.2 items, again with empty ranges.

    /// Adds the type `name[args]`, followed by `?` if `optional`.
    pub fn ty_of(&mut self, name: &str, args: Vec<TyId>, optional: bool) -> Result<TyId, AstError> {
        self.add_ty(Ident::bare(name), args, optional, TextRange::default())
    }

    /// Adds the type `name` without arguments.
    pub fn named_ty(&mut self, name: &str) -> Result<TyId, AstError> {
        self.ty_of(name, Vec::new(), false)
    }

    /// Adds `none`, `me`, `now` or `signed`.
    pub fn keyword(&mut self, value: KeywordValue) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Keyword(value), TextRange::default())
    }

    /// Adds `base.name`.
    pub fn field(&mut self, base: ExprId, name: &str) -> Result<ExprId, AstError> {
        let kind = ExprKind::Field {
            base,
            name: Ident::bare(name),
        };
        self.add_expr(kind, TextRange::default())
    }

    /// Adds `base[index]`.
    pub fn index(&mut self, base: ExprId, index: ExprId) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Index { base, index }, TextRange::default())
    }

    /// Adds `element in collection`, or `not in` if `negated`.
    pub fn in_expr(
        &mut self,
        element: ExprId,
        collection: ExprId,
        negated: bool,
    ) -> Result<ExprId, AstError> {
        let kind = ExprKind::In {
            element,
            collection,
            negated,
        };
        self.add_expr(kind, TextRange::default())
    }

    /// Adds `lo..hi`.
    pub fn range(&mut self, lo: ExprId, hi: ExprId) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Range { lo, hi }, TextRange::default())
    }

    /// Adds `value catch Kind, Kind`.
    pub fn catch(&mut self, value: ExprId, kinds: &[&str]) -> Result<ExprId, AstError> {
        let kinds = kinds.iter().map(|k| Ident::bare(k)).collect();
        self.add_expr(ExprKind::Catch { value, kinds }, TextRange::default())
    }

    /// Adds `value ?? fallback`.
    pub fn fallback(&mut self, value: ExprId, fallback: ExprId) -> Result<ExprId, AstError> {
        self.add_expr(ExprKind::Fallback { value, fallback }, TextRange::default())
    }

    /// Adds the set literal `{items}`.
    pub fn set(&mut self, items: Vec<ExprId>) -> Result<ExprId, AstError> {
        self.add_expr(
            ExprKind::Set(items.into_boxed_slice()),
            TextRange::default(),
        )
    }

    /// Adds the list literal `[items]`.
    pub fn list(&mut self, items: Vec<ExprId>) -> Result<ExprId, AstError> {
        self.add_expr(
            ExprKind::List(items.into_boxed_slice()),
            TextRange::default(),
        )
    }

    /// Adds the map literal `[k: v, ...]`.
    pub fn map(&mut self, entries: Vec<(ExprId, ExprId)>) -> Result<ExprId, AstError> {
        let entries = entries
            .into_iter()
            .map(|(key, value)| MapEntry { key, value })
            .collect();
        self.add_expr(ExprKind::Map(entries), TextRange::default())
    }

    /// Adds `make ty { name: value, ... }`.
    pub fn make(&mut self, ty: &str, fields: Vec<(&str, ExprId)>) -> Result<ExprId, AstError> {
        let fields = fields
            .into_iter()
            .map(|(name, value)| FieldInit::bare(name, value))
            .collect();
        let make = MakeExpr {
            ty: Ident::bare(ty),
            fields,
        };
        self.add_expr(ExprKind::Make(Box::new(make)), TextRange::default())
    }

    /// Adds the query `head [where filter] [sort field [desc]] [limit|last n]`.
    pub fn query(
        &mut self,
        head: &str,
        filter: Option<ExprId>,
        sort: Option<(&str, bool)>,
        take: Option<(TakeKind, ExprId)>,
    ) -> Result<ExprId, AstError> {
        let query = Query {
            head: Ident::bare(head),
            filter,
            sort: sort.map(|(field, desc)| SortKey {
                field: Ident::bare(field),
                desc,
                range: TextRange::default(),
            }),
            take: take.map(|(kind, count)| Take {
                kind,
                count,
                range: TextRange::default(),
            }),
        };
        self.add_expr(ExprKind::Query(Box::new(query)), TextRange::default())
    }

    /// Adds `if let name = value` with its blocks.
    pub fn if_let_stmt(
        &mut self,
        name: &str,
        value: ExprId,
        then_block: BlockId,
        else_block: Option<BlockId>,
    ) -> Result<StmtId, AstError> {
        let kind = StmtKind::IfLet {
            name: Ident::bare(name),
            value,
            then_block,
            else_block,
        };
        self.add_stmt(kind, TextRange::default())
    }

    /// Adds `for name in source` with its body.
    pub fn for_stmt(
        &mut self,
        name: &str,
        source: ExprId,
        body: BlockId,
    ) -> Result<StmtId, AstError> {
        let kind = StmtKind::For {
            name: Ident::bare(name),
            source,
            body,
        };
        self.add_stmt(kind, TextRange::default())
    }

    /// Adds `drop source`.
    pub fn drop_stmt(&mut self, source: ExprId) -> Result<StmtId, AstError> {
        self.add_stmt(StmtKind::Drop { source }, TextRange::default())
    }

    /// Adds `target = value`.
    pub fn assign_stmt(&mut self, target: ExprId, value: ExprId) -> Result<StmtId, AstError> {
        self.add_stmt(StmtKind::Assign { target, value }, TextRange::default())
    }

    /// Adds an element node.
    pub fn element(
        &mut self,
        name: &str,
        value: Option<ExprId>,
        attrs: Vec<Attr>,
        handler: Option<Handler>,
        children: Vec<ElementChild>,
    ) -> Result<ViewNodeId, AstError> {
        let element = Element {
            name: Ident::bare(name),
            value,
            attrs: attrs.into_boxed_slice(),
            handler,
            children: children.into_boxed_slice(),
        };
        self.add_view_node(
            ViewNodeKind::Element(Box::new(element)),
            TextRange::default(),
        )
    }

    /// Adds a view `if` node.
    pub fn view_if(
        &mut self,
        cond: Cond,
        then_nodes: Vec<ViewNodeId>,
        else_nodes: Option<Vec<ViewNodeId>>,
    ) -> Result<ViewNodeId, AstError> {
        let node = ViewIf {
            cond,
            then_nodes: then_nodes.into_boxed_slice(),
            else_nodes: else_nodes.map(Vec::into_boxed_slice),
        };
        self.add_view_node(ViewNodeKind::If(Box::new(node)), TextRange::default())
    }

    /// Adds a view `for` node.
    pub fn view_for(
        &mut self,
        name: &str,
        source: ExprId,
        nodes: Vec<ViewNodeId>,
    ) -> Result<ViewNodeId, AstError> {
        let node = ViewFor {
            name: Ident::bare(name),
            source,
            nodes: nodes.into_boxed_slice(),
        };
        self.add_view_node(ViewNodeKind::For(Box::new(node)), TextRange::default())
    }

    /// Appends `app name` with its lines.
    pub fn app_item(&mut self, name: &str, items: Vec<AppItem>) -> Result<(), AstError> {
        self.add_item(Item::App(Box::new(AppDecl {
            name: Ident::bare(name),
            items: items.into_boxed_slice(),
            range: TextRange::default(),
        })))
    }

    /// Appends `enum name = a | b`.
    pub fn enum_item(&mut self, name: &str, variants: &[&str]) -> Result<(), AstError> {
        self.add_item(Item::Enum(Box::new(EnumDecl {
            name: Ident::bare(name),
            variants: variants.iter().map(|v| Ident::bare(v)).collect(),
            range: TextRange::default(),
        })))
    }

    /// Appends `data name` with its members.
    pub fn data_item(&mut self, name: &str, members: Vec<Member>) -> Result<(), AstError> {
        self.add_item(Item::Data(Box::new(DataDecl {
            name: Ident::bare(name),
            members: members.into_boxed_slice(),
            range: TextRange::default(),
        })))
    }

    /// Appends `var name ...`.
    pub fn var_item(&mut self, name: &str, init: VarInit) -> Result<(), AstError> {
        self.add_item(Item::Var(Box::new(VarDecl {
            name: Ident::bare(name),
            init,
            range: TextRange::default(),
        })))
    }

    /// Appends a view.
    pub fn view_item(
        &mut self,
        name: &str,
        params: Option<Vec<TypedParam>>,
        nodes: Vec<ViewNodeId>,
    ) -> Result<(), AstError> {
        self.add_item(Item::View(Box::new(ViewDecl {
            name: Ident::bare(name),
            params: params.map(Vec::into_boxed_slice),
            nodes: nodes.into_boxed_slice(),
            range: TextRange::default(),
        })))
    }

    /// Appends `style` whose CSS body covers `body`.
    pub fn style_item(&mut self, body: TextRange) -> Result<(), AstError> {
        self.add_item(Item::Style(Box::new(StyleDecl { body, range: body })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type R = Result<(), AstError>;

    #[test]
    fn node_sizes_stay_within_d54_budget() {
        assert!(std::mem::size_of::<Expr>() <= 64);
        assert!(std::mem::size_of::<Stmt>() <= 64);
        assert!(std::mem::size_of::<Block>() <= 64);
        assert!(std::mem::size_of::<Item>() <= 64);
        assert!(std::mem::size_of::<Param>() <= 64);
        assert!(std::mem::size_of::<TypeRef>() <= 64);
    }

    #[test]
    fn empty_module_counts_its_root() {
        let m = Module::new();
        assert_eq!(m.node_count(), 1);
        assert_eq!(m.height(), 1);
        assert!(m.items().is_empty());
        assert_eq!(m.limits(), Limits::DEFAULT);
    }

    /// Builds the `fib` program of SYNTAX 4.11 with the test constructors.
    #[test]
    fn builds_fib_example() -> R {
        let mut m = Module::new();
        // fn fib(n: Int) -> Int
        let n = m.name("n")?;
        let two = m.int(2)?;
        let cond = m.binary(BinaryOp::Lt, n, two)?;
        let n2 = m.name("n")?;
        let ret_n = m.return_stmt(n2)?;
        let then_block = m.block_of(vec![ret_n])?;
        let if_stmt = m.if_stmt(cond, then_block, None)?;
        let n3 = m.name("n")?;
        let one = m.int(1)?;
        let sub1 = m.binary(BinaryOp::Sub, n3, one)?;
        let call1 = m.call("fib", vec![sub1])?;
        let n4 = m.name("n")?;
        let two2 = m.int(2)?;
        let sub2 = m.binary(BinaryOp::Sub, n4, two2)?;
        let call2 = m.call("fib", vec![sub2])?;
        let sum = m.binary(BinaryOp::Add, call1, call2)?;
        let ret = m.return_stmt(sum)?;
        let body = m.block_of(vec![if_stmt, ret])?;
        m.fn_item("fib", &[("n", "Int")], Some("Int"), body)?;

        // fn main()
        let twenty = m.int(20)?;
        let fib20 = m.call("fib", vec![twenty])?;
        let let_x = m.let_stmt("x", fib20)?;
        let x = m.name("x")?;
        let s = m.string(vec![StrPart::Text("fib(20) = ".into()), StrPart::Interp(x)])?;
        let print = m.call("print", vec![s])?;
        let print_stmt = m.expr_stmt(print)?;
        let main_body = m.block_of(vec![let_x, print_stmt])?;
        m.fn_item("main", &[], None, main_body)?;

        assert_eq!(m.items().len(), 2);
        let Some(Item::Fn(fib)) = m.items().first() else {
            return Err(AstError::UnknownId);
        };
        assert_eq!(&*fib.name.text, "fib");
        assert_eq!(fib.params.len(), 1);
        assert_eq!(fib.result.as_ref().map(|t| &*t.name.text), Some("Int"));
        let body = m.block(fib.body).ok_or(AstError::UnknownId)?;
        assert_eq!(body.stmts.len(), 2);
        // module > fn > block > return > add > call > sub > name
        assert_eq!(m.height(), 8);
        Ok(())
    }

    #[test]
    fn heights_follow_children() -> R {
        let mut m = Module::new();
        let a = m.int(1)?;
        assert_eq!(m.expr_height(a), Some(1));
        let b = m.int(2)?;
        let sum = m.binary(BinaryOp::Add, a, b)?;
        assert_eq!(m.expr_height(sum), Some(2));
        let p = m.paren(sum)?;
        let neg = m.unary(UnaryOp::Neg, p)?;
        assert_eq!(m.expr_height(neg), Some(4));
        let stmt = m.expr_stmt(neg)?;
        assert_eq!(m.stmt_height(stmt), Some(5));
        let empty = m.block_of(vec![])?;
        assert_eq!(m.block_height(empty), Some(1));
        assert_eq!(m.height(), 6);
        Ok(())
    }

    #[test]
    fn empty_string_has_no_parts() -> R {
        let mut m = Module::new();
        let s = m.text("")?;
        let expr = m.expr(s).ok_or(AstError::UnknownId)?;
        assert_eq!(expr.kind, ExprKind::Str(Box::new([])));
        assert_eq!(m.expr_height(s), Some(1));
        Ok(())
    }

    #[test]
    fn node_limit_is_exact() -> R {
        // Root plus three nodes fit; the fourth node is rejected.
        let mut m = Module::with_limits(Limits {
            max_nodes: 4,
            max_height: MAX_HEIGHT,
        });
        m.int(1)?;
        m.int(2)?;
        m.int(3)?;
        assert_eq!(m.node_count(), 4);
        assert_eq!(m.int(4), Err(AstError::NodeLimit));
        assert_eq!(m.node_count(), 4);
        Ok(())
    }

    #[test]
    fn node_limit_counts_params_and_types() -> R {
        // Root, block, then fn (1) + param (2) + result (1) = 6 nodes.
        let mut m = Module::with_limits(Limits {
            max_nodes: 5,
            max_height: MAX_HEIGHT,
        });
        let body = m.block_of(vec![])?;
        assert_eq!(
            m.fn_item("f", &[("a", "Int")], Some("Int"), body),
            Err(AstError::NodeLimit)
        );
        assert!(m.items().is_empty());
        // The rejected item did not attach its body; a smaller fn may use it.
        m.fn_item("f", &[], None, body)?;
        assert_eq!(m.node_count(), 3);
        Ok(())
    }

    #[test]
    fn zero_node_limit_rejects_everything() {
        let mut m = Module::with_limits(Limits {
            max_nodes: 0,
            max_height: MAX_HEIGHT,
        });
        assert_eq!(m.int(1), Err(AstError::NodeLimit));
    }

    /// Builds `1 + 1 + ... ` with `operands` operands as a left leaning chain.
    fn chain(m: &mut Module, operands: u32) -> Result<ExprId, AstError> {
        let mut acc = m.int(1)?;
        for _ in 1..operands {
            let rhs = m.int(1)?;
            acc = m.binary(BinaryOp::Add, acc, rhs)?;
        }
        Ok(acc)
    }

    #[test]
    fn height_limit_is_exact() -> R {
        // With limit 10 the root leaves room for an expression of height 9.
        let limits = Limits {
            max_nodes: MAX_NODES,
            max_height: 10,
        };
        let mut m = Module::with_limits(limits);
        let e = chain(&mut m, 9)?;
        assert_eq!(m.expr_height(e), Some(9));
        assert_eq!(m.height(), 10);

        let mut m = Module::with_limits(limits);
        assert_eq!(chain(&mut m, 10), Err(AstError::HeightLimit));
        Ok(())
    }

    #[test]
    fn d54_chain_cases_with_default_limits() -> R {
        // chain_below: 2 000 operands in `let x = ...` inside fn main.
        let mut m = Module::new();
        let e = chain(&mut m, 2_000)?;
        let s = m.let_stmt("x", e)?;
        let b = m.block_of(vec![s])?;
        m.fn_item("main", &[], None, b)?;
        assert_eq!(m.height(), 2_004);

        // chain_above: 2 100 operands never fit.
        let mut m = Module::new();
        assert_eq!(chain(&mut m, 2_100), Err(AstError::HeightLimit));
        Ok(())
    }

    #[test]
    fn rejects_foreign_ids() -> R {
        let mut other = Module::new();
        other.int(1)?;
        let foreign = other.int(2)?;
        let mut m = Module::new();
        let a = m.int(1)?;
        assert_eq!(
            m.binary(BinaryOp::Add, a, foreign),
            Err(AstError::UnknownId)
        );
        // The failed call must not attach `a`.
        let b = m.int(3)?;
        m.binary(BinaryOp::Add, a, b)?;
        Ok(())
    }

    #[test]
    fn rejects_shared_children() -> R {
        let mut m = Module::new();
        let a = m.int(1)?;
        assert_eq!(
            m.binary(BinaryOp::Add, a, a),
            Err(AstError::AlreadyAttached)
        );
        let b = m.int(2)?;
        m.binary(BinaryOp::Add, a, b)?;
        assert_eq!(m.unary(UnaryOp::Neg, a), Err(AstError::AlreadyAttached));

        let s = m.expr_stmt(b).err();
        assert_eq!(s, Some(AstError::AlreadyAttached));

        let c = m.int(3)?;
        let stmt = m.expr_stmt(c)?;
        m.block_of(vec![stmt])?;
        assert_eq!(m.block_of(vec![stmt]), Err(AstError::AlreadyAttached));

        let blk = m.block_of(vec![])?;
        let cond = m.bool(true)?;
        assert_eq!(
            m.if_stmt(cond, blk, Some(blk)),
            Err(AstError::AlreadyAttached)
        );
        m.fn_item("f", &[], None, blk)?;
        assert_eq!(
            m.fn_item("g", &[], None, blk),
            Err(AstError::AlreadyAttached)
        );
        Ok(())
    }

    #[test]
    fn string_interpolations_must_be_distinct() -> R {
        let mut m = Module::new();
        let x = m.name("x")?;
        assert_eq!(
            m.string(vec![StrPart::Interp(x), StrPart::Interp(x)]),
            Err(AstError::AlreadyAttached)
        );
        let s = m.string(vec![StrPart::Text("a".into()), StrPart::Interp(x)])?;
        assert_eq!(m.expr_height(s), Some(2));
        Ok(())
    }

    /// Builds a `main` whose body has `wide` statements and ends in a call
    /// with `wide` arguments, and returns the module.
    fn wide_module(wide: usize) -> Result<Module, AstError> {
        let mut m = Module::new();
        let args = (0..wide).map(|_| m.int(1)).collect::<Result<Vec<_>, _>>()?;
        let call = m.call("f", args)?;
        let stmts = (0..wide)
            .map(|i| {
                let e = m.int(i as i64)?;
                m.expr_stmt(e)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let last = m.expr_stmt(call)?;
        let mut all = stmts;
        all.push(last);
        let body = m.block_of(all)?;
        m.fn_item("main", &[], None, body)?;
        Ok(m)
    }

    #[test]
    fn wide_call_and_block_count_their_nodes() -> R {
        const WIDE: usize = 200_000;
        let m = wide_module(WIDE)?;
        assert_eq!(
            m.node_count(),
            1 + 2 * WIDE as u32 + 2 + WIDE as u32 + 1 + 1 + 1
        );
        Ok(())
    }

    /// Wide nodes must be linear in their number of children (AC-04). With
    /// the old quadratic check 200 000 children took several seconds. The
    /// check compares two sizes instead of using a time limit, so it holds
    /// under any machine load (D74).
    #[test]
    fn wide_call_and_block_are_linear() {
        crate::test_support::assert_linear(8_000, |n| n, |&n| wide_module(n).ok());
    }

    #[test]
    fn rejected_wide_node_releases_all_children() -> R {
        let mut m = Module::new();
        let args = (0..1_000)
            .map(|_| m.int(1))
            .collect::<Result<Vec<_>, _>>()?;
        let mut dup = args.clone();
        dup.extend(args.first().copied());
        assert_eq!(m.call("f", dup), Err(AstError::AlreadyAttached));
        // Every argument is free again and can be used once.
        m.call("f", args)?;

        // A rejection after all children were marked also rolls back.
        let mut small = Module::with_limits(Limits {
            max_nodes: 3,
            max_height: MAX_HEIGHT,
        });
        let a = small.int(1)?;
        let b = small.int(2)?;
        assert_eq!(small.binary(BinaryOp::Add, a, b), Err(AstError::NodeLimit));
        let s = small.add_stmt(StmtKind::Expr(a), TextRange::default());
        assert_eq!(s, Err(AstError::NodeLimit));
        let mut wide = Module::with_limits(Limits {
            max_nodes: MAX_NODES,
            max_height: 3,
        });
        let x = wide.int(1)?;
        let neg = wide.unary(UnaryOp::Neg, x)?;
        assert_eq!(wide.expr_stmt(neg), Err(AstError::HeightLimit));
        // `neg` was released by the rejected statement.
        assert_eq!(wide.paren(neg), Err(AstError::HeightLimit));
        assert_eq!(
            wide.add_block(vec![], TextRange::default()).map(|_| ()),
            Ok(())
        );
        Ok(())
    }

    #[test]
    fn text_range_rules() {
        assert_eq!(TextRange::new(3, 2), None);
        let r = TextRange::new(2, 5);
        assert_eq!(r.map(TextRange::len), Some(3));
        assert!(TextRange::empty(7).is_empty());
        let a = TextRange::empty(10);
        let b = TextRange::new(2, 4).unwrap_or_default();
        let c = a.cover(b);
        assert_eq!((c.start(), c.end()), (2, 10));
    }

    #[test]
    fn operator_spelling() {
        assert_eq!(BinaryOp::Rem.as_str(), "%");
        assert_eq!(BinaryOp::Or.as_str(), "or");
        assert_eq!(UnaryOp::Not.as_str(), "not");
    }
}

#[cfg(test)]
mod v02_tests {
    use super::*;

    type R = Result<(), AstError>;

    fn limited(max_nodes: u32, max_height: u32) -> Module {
        Module::with_limits(Limits {
            max_nodes,
            max_height,
        })
    }

    /// True if every arena entry has a parent, so a finished program is one
    /// connected tree below the module root and its items.
    fn all_attached(m: &Module) -> bool {
        // Blocks of functions and the nodes of views hang from items, which
        // are not arena entries; they are attached through `add_item`.
        [
            &m.expr_attached,
            &m.stmt_attached,
            &m.block_attached,
            &m.ty_attached,
            &m.view_node_attached,
        ]
        .iter()
        .all(|flags| flags.iter().all(|f| *f))
    }

    #[test]
    fn v02_node_sizes_stay_within_d54_budget() {
        assert!(std::mem::size_of::<Ty>() <= 64);
        assert!(std::mem::size_of::<ViewNode>() <= 64);
        assert!(std::mem::size_of::<Expr>() <= 64);
        assert!(std::mem::size_of::<Stmt>() <= 64);
        assert!(std::mem::size_of::<Item>() <= 64);
    }

    #[test]
    fn keyword_spelling() {
        assert_eq!(KeywordValue::NoneValue.as_str(), "none");
        assert_eq!(KeywordValue::Signed.as_str(), "signed");
        assert_eq!(RuleVerb::Drop.as_str(), "drop");
        assert_eq!(TakeKind::Last.as_str(), "last");
        assert_eq!(Placement::Server.as_str(), "server");
        assert_eq!(Placement::Inferred.as_str(), "");
    }

    /// Builds `data Room` of SYNTAX 5.1 without the rules `make` and `edit`:
    /// `name: Text`, `members: Set[User] = {me}`,
    /// `check name.len in 1..40`, `see if signed`. Returns the members.
    fn room_members(m: &mut Module) -> Result<Vec<Member>, AstError> {
        let text = m.named_ty("Text")?;
        let user = m.named_ty("User")?;
        let set_user = m.ty_of("Set", vec![user], false)?;
        let me = m.keyword(KeywordValue::Me)?;
        let default = m.set(vec![me])?;
        let name = m.name("name")?;
        let len = m.field(name, "len")?;
        let one = m.int(1)?;
        let forty = m.int(40)?;
        let bounds = m.range(one, forty)?;
        let check = m.in_expr(len, bounds, false)?;
        let signed = m.keyword(KeywordValue::Signed)?;
        let mut members_field = FieldDecl::new("members", set_user);
        members_field.default = Some(default);
        Ok(vec![
            Member::field(FieldDecl::new("name", text)),
            Member::field(members_field),
            Member::check(check),
            Member::rule(RuleVerb::See, None, signed),
        ])
    }

    #[test]
    fn data_counts_members_types_and_exprs() -> R {
        let mut m = Module::new();
        let members = room_members(&mut m)?;
        // root 1, types 3, `{me}` 2, check 6, `signed` 1
        assert_eq!(m.node_count(), 13);
        m.data_item("Room", members)?;
        // item 1 plus 4 members
        assert_eq!(m.node_count(), 18);
        // root > data > check member > in > field > name
        assert_eq!(m.height(), 6);
        assert!(all_attached(&m));
        Ok(())
    }

    #[test]
    fn rejected_data_releases_types_and_exprs() -> R {
        let mut m = limited(17, MAX_HEIGHT);
        let mut members = room_members(&mut m)?;
        assert_eq!(
            m.data_item("Room", members.clone()),
            Err(AstError::NodeLimit)
        );
        assert_eq!(m.node_count(), 13);
        assert!(m.items().is_empty());
        // Without the rule the item needs 4 nodes and fits exactly; the types
        // and expressions of the rejected item were released.
        members.pop();
        m.data_item("Room", members)?;
        assert_eq!(m.node_count(), 17);
        assert_eq!(m.items().len(), 1);
        Ok(())
    }

    #[test]
    fn shared_and_foreign_types_are_rejected() -> R {
        let mut m = Module::new();
        let text = m.named_ty("Text")?;
        let both = vec![
            Member::field(FieldDecl::new("a", text)),
            Member::field(FieldDecl::new("b", text)),
        ];
        assert_eq!(m.data_item("D", both), Err(AstError::AlreadyAttached));
        assert_eq!(
            m.ty_of("List", vec![text, text], false),
            Err(AstError::AlreadyAttached)
        );

        let mut other = Module::new();
        other.named_ty("Int")?;
        let foreign = other.named_ty("Int")?;
        assert_eq!(
            m.ty_of("Set", vec![foreign], true),
            Err(AstError::UnknownId)
        );
        // `text` is still free after all three rejections.
        let list = m.ty_of("List", vec![text], true)?;
        assert_eq!(m.ty(list).map(|t| t.optional), Some(true));
        assert_eq!(m.ty_height(list), Some(2));
        Ok(())
    }

    #[test]
    fn type_height_limit_is_exact() -> R {
        // Limit 6: the root leaves room for a type of height 5.
        let mut m = limited(MAX_NODES, 6);
        let mut ty = m.named_ty("Int")?;
        for _ in 0..4 {
            ty = m.ty_of("List", vec![ty], false)?;
        }
        assert_eq!(m.ty_height(ty), Some(5));
        assert_eq!(m.height(), 6);
        assert_eq!(m.ty_of("List", vec![ty], false), Err(AstError::HeightLimit));
        // A field puts the type one level lower: member and item add two.
        let mut m = limited(MAX_NODES, 6);
        let mut ty = m.named_ty("Int")?;
        for _ in 0..2 {
            ty = m.ty_of("List", vec![ty], false)?;
        }
        m.data_item("D", vec![Member::field(FieldDecl::new("f", ty))])?;
        assert_eq!(m.height(), 6);
        let mut m = limited(MAX_NODES, 6);
        let mut ty = m.named_ty("Int")?;
        for _ in 0..3 {
            ty = m.ty_of("List", vec![ty], false)?;
        }
        let field = vec![Member::field(FieldDecl::new("f", ty))];
        assert_eq!(m.data_item("D", field), Err(AstError::HeightLimit));
        Ok(())
    }

    #[test]
    fn name_lists_count_against_the_node_limit() -> R {
        // Root, item and three variants.
        let mut m = limited(5, MAX_HEIGHT);
        m.enum_item("Status", &["todo", "doing", "done"])?;
        assert_eq!(m.node_count(), 5);
        assert_eq!(m.height(), 3);
        let mut m = limited(4, MAX_HEIGHT);
        assert_eq!(
            m.enum_item("Status", &["todo", "doing", "done"]),
            Err(AstError::NodeLimit)
        );
        // An empty enum, which only a tree built by hand can have, is a leaf.
        m.enum_item("Empty", &[])?;
        assert_eq!(m.height(), 2);

        // `catch` kinds count as well; an empty list adds no level.
        let mut m = Module::new();
        let x = m.name("x")?;
        let c = m.catch(x, &["Denied", "Invalid"])?;
        assert_eq!(m.node_count(), 5);
        assert_eq!(m.expr_height(c), Some(2));
        let y = m.name("y")?;
        let empty = m.catch(y, &[])?;
        assert_eq!(m.expr_height(empty), Some(2));
        assert_eq!(m.node_count(), 7);
        Ok(())
    }

    #[test]
    fn app_lines_are_kept_in_order() -> R {
        let mut m = Module::new();
        let lines = vec![
            AppItem::auth("paseto", Some("open")),
            AppItem::home("Main"),
            AppItem::auth("paseto", None),
        ];
        m.app_item("Chat", lines)?;
        assert_eq!(m.node_count(), 5);
        let Some(Item::App(app)) = m.items().first() else {
            return Err(AstError::UnknownId);
        };
        assert_eq!(app.items.len(), 3);
        assert!(matches!(
            &app.items.get(2).map(|i| &i.kind),
            Some(AppItemKind::Auth { mode: None, .. })
        ));
        // `app Name` without lines.
        m.app_item("Bare", vec![])?;
        assert_eq!(m.node_count(), 6);
        Ok(())
    }

    #[test]
    fn query_counts_its_clauses() -> R {
        // Message where room == r sort made last 200
        let mut m = Module::new();
        let room = m.name("room")?;
        let r = m.name("r")?;
        let filter = m.binary(BinaryOp::Eq, room, r)?;
        let n = m.int(200)?;
        let q = m.query(
            "Message",
            Some(filter),
            Some(("made", false)),
            Some((TakeKind::Last, n)),
        )?;
        // root, 3 filter exprs, 200, query, sort key, take clause
        assert_eq!(m.node_count(), 8);
        // query > take > 200 and query > `==` > name: both height 3
        assert_eq!(m.expr_height(q), Some(3));

        // A bare head, as D69 builds it for `for x in items`, is a leaf.
        let bare = m.query("items", None, None, None)?;
        assert_eq!(m.expr_height(bare), Some(1));
        assert_eq!(m.node_count(), 9);
        Ok(())
    }

    #[test]
    fn literals_make_and_fallback() -> R {
        let mut m = Module::new();
        let empty_set = m.set(vec![])?;
        let empty_list = m.list(vec![])?;
        let empty_map = m.map(vec![])?;
        for id in [empty_set, empty_list, empty_map] {
            assert_eq!(m.expr_height(id), Some(1));
        }
        let k = m.text("a")?;
        let v = m.int(1)?;
        let map = m.map(vec![(k, v)])?;
        // map > entry > key
        assert_eq!(m.expr_height(map), Some(3));
        let it = m.name("it")?;
        let make = m.make("Room", vec![("name", it)])?;
        assert_eq!(m.expr_height(make), Some(3));
        // 3 empty literals, key, value, map, entry, `it`, make, init
        assert_eq!(m.node_count(), 11);

        let x = m.name("x")?;
        assert_eq!(
            m.make("Room", vec![("a", x), ("b", x)]),
            Err(AstError::AlreadyAttached)
        );
        assert_eq!(m.map(vec![(x, x)]), Err(AstError::AlreadyAttached));
        let f = m.call("f", vec![x])?;
        let caught = m.catch(f, &["Denied"])?;
        let y = m.name("y")?;
        let fb = m.fallback(caught, y)?;
        assert!(matches!(
            m.expr(fb).map(|e| &e.kind),
            Some(ExprKind::Fallback { .. })
        ));
        let none = m.keyword(KeywordValue::NoneValue)?;
        let a = m.name("a")?;
        let not_in = m.in_expr(a, none, true)?;
        let base = m.name("t")?;
        let roles = m.field(base, "roles")?;
        let me = m.keyword(KeywordValue::Me)?;
        let idx = m.index(roles, me)?;
        assert_eq!(m.expr_height(idx), Some(3));
        assert_eq!(m.expr_height(not_in), Some(2));
        Ok(())
    }

    #[test]
    fn v02_statements() -> R {
        let mut m = Module::new();
        // if let r = current / r.name = "x" / else / drop r2
        let current = m.name("current")?;
        let r = m.name("r")?;
        let target = m.field(r, "name")?;
        let value = m.text("x")?;
        let assign = m.assign_stmt(target, value)?;
        let then_block = m.block_of(vec![assign])?;
        let r2 = m.name("r2")?;
        let drop = m.drop_stmt(r2)?;
        let else_block = m.block_of(vec![drop])?;
        let if_let = m.if_let_stmt("r", current, then_block, Some(else_block))?;
        // if let > block > assign > field > name
        assert_eq!(m.stmt_height(if_let), Some(5));

        let source = m.query("Message", None, None, None)?;
        let msg = m.name("m")?;
        let print = m.call("print", vec![msg])?;
        let body_stmt = m.expr_stmt(print)?;
        let body = m.block_of(vec![body_stmt])?;
        let for_stmt = m.for_stmt("m", source, body)?;
        assert_eq!(m.stmt_height(for_stmt), Some(5));

        // A block may not be shared between the branches.
        let cond = m.name("o")?;
        let blk = m.block_of(vec![])?;
        assert_eq!(
            m.if_let_stmt("v", cond, blk, Some(blk)),
            Err(AstError::AlreadyAttached)
        );
        let main = m.block_of(vec![if_let, for_stmt])?;
        m.fn_item("main", &[], None, main)?;
        // `cond` and `blk` stay free from the rejected statement.
        let cond_stmt = m.expr_stmt(cond)?;
        let b = m.block_of(vec![cond_stmt])?;
        m.if_let_stmt("v", cond, blk, None).err();
        m.fn_item("g", &[], None, b)?;
        Ok(())
    }

    #[test]
    fn element_counts_attrs_and_handlers() -> R {
        // button r.name selected: r == current on click current = r
        let mut m = Module::new();
        let r = m.name("r")?;
        let label = m.field(r, "name")?;
        let r2 = m.name("r")?;
        let cur = m.name("current")?;
        let selected = m.binary(BinaryOp::Eq, r2, cur)?;
        let target = m.name("current")?;
        let r3 = m.name("r")?;
        let button = m.element(
            "button",
            Some(label),
            vec![Attr::named("selected", selected)],
            Some(Handler::bare("click", target, Some(r3))),
            vec![],
        )?;
        // 7 expressions, attr, handler, element, root
        assert_eq!(m.node_count(), 11);
        // element > attr > `==` > name
        assert_eq!(m.view_node_height(button), Some(4));

        // A handler line below an element counts like an inline one.
        let it = m.name("it")?;
        let make = m.make("Room", vec![("name", it)])?;
        let hint = m.text("New room")?;
        let entry = m.element(
            "entry",
            None,
            vec![Attr::named("hint", hint), Attr::style("wide", None)],
            None,
            vec![ElementChild::Handler(Handler::bare("submit", make, None))],
        )?;
        // element > handler > make > init > `it`
        assert_eq!(m.view_node_height(entry), Some(5));
        // `it`, make, init, hint, 2 attrs, handler, element
        assert_eq!(m.node_count(), 19);

        let col = m.element(
            "col",
            None,
            vec![],
            None,
            vec![ElementChild::Node(button), ElementChild::Node(entry)],
        )?;
        assert_eq!(m.view_node_height(col), Some(6));
        assert_eq!(
            m.element("row", None, vec![], None, vec![ElementChild::Node(button)]),
            Err(AstError::AlreadyAttached)
        );
        m.view_item("Main", None, vec![col])?;
        assert!(all_attached(&m));
        Ok(())
    }

    #[test]
    fn view_nodes_reject_sharing_and_foreign_ids() -> R {
        let mut m = Module::new();
        let a = m.element("text", None, vec![], None, vec![])?;
        let cond = m.bool(true)?;
        assert_eq!(
            m.view_if(Cond::Expr(cond), vec![a], Some(vec![a])),
            Err(AstError::AlreadyAttached)
        );
        assert_eq!(
            m.view_item("V", Some(vec![]), vec![a, a]),
            Err(AstError::AlreadyAttached)
        );
        let mut other = Module::new();
        other.element("text", None, vec![], None, vec![])?;
        let foreign = other.element("text", None, vec![], None, vec![])?;
        let source = m.query("Room", None, None, None)?;
        assert_eq!(
            m.view_for("r", source, vec![foreign]),
            Err(AstError::UnknownId)
        );
        // Everything above was released again.
        let node = m.view_for("r", source, vec![a])?;
        let if_node = m.view_if(Cond::Expr(cond), vec![node], None)?;
        assert_eq!(m.view_node_height(if_node), Some(3));
        m.view_item("V", Some(vec![]), vec![if_node])?;
        assert!(all_attached(&m));
        Ok(())
    }

    #[test]
    fn view_height_limit_is_exact() -> R {
        // Limit 5: root, view item, then nodes of height 3 at most.
        let build = |m: &mut Module, depth: u32| -> R {
            let mut node = m.element("text", None, vec![], None, vec![])?;
            for _ in 1..depth {
                node = m.element("col", None, vec![], None, vec![ElementChild::Node(node)])?;
            }
            m.view_item("V", None, vec![node])
        };
        let mut m = limited(MAX_NODES, 5);
        build(&mut m, 3)?;
        assert_eq!(m.height(), 5);
        let mut m = limited(MAX_NODES, 5);
        assert_eq!(build(&mut m, 4), Err(AstError::HeightLimit));
        Ok(())
    }

    /// `server fn closeDone` of SYNTAX 4.6.
    #[test]
    fn builds_elevated_server_fn() -> R {
        let mut m = Module::new();
        let team = m.named_ty("Team")?;
        let int = m.named_ty("Int")?;
        // call if t.roles[me] == admin
        let t = m.name("t")?;
        let roles = m.field(t, "roles")?;
        let me = m.keyword(KeywordValue::Me)?;
        let role = m.index(roles, me)?;
        let admin = m.name("admin")?;
        let call_cond = m.binary(BinaryOp::Eq, role, admin)?;
        // drop Issue where team == t and status == done and changed < now - days.days
        let team_name = m.name("team")?;
        let t2 = m.name("t")?;
        let c1 = m.binary(BinaryOp::Eq, team_name, t2)?;
        let status = m.name("status")?;
        let done = m.name("done")?;
        let c2 = m.binary(BinaryOp::Eq, status, done)?;
        let both = m.binary(BinaryOp::And, c1, c2)?;
        let changed = m.name("changed")?;
        let now = m.keyword(KeywordValue::Now)?;
        let days = m.name("days")?;
        let span = m.field(days, "days")?;
        let limit = m.binary(BinaryOp::Sub, now, span)?;
        let c3 = m.binary(BinaryOp::Lt, changed, limit)?;
        let filter = m.binary(BinaryOp::And, both, c3)?;
        let query = m.query("Issue", Some(filter), None, None)?;
        let drop = m.drop_stmt(query)?;
        let body = m.block_of(vec![drop])?;
        let before = m.node_count();
        m.add_item(Item::PlacedFn(Box::new(PlacedFnDecl {
            placement: Placement::Server,
            name: Ident::bare("closeDone"),
            params: Box::new([TypedParam::bare("t", team), TypedParam::bare("days", int)]),
            result: None,
            elevated: None,
            call_rule: Some(CallRule {
                cond: call_cond,
                range: TextRange::default(),
            }),
            body,
            range: TextRange::default(),
        })))?;
        // item, 2 params, call rule
        assert_eq!(m.node_count(), before + 4);
        // fn > block > drop > query > and > `<` > `-` > field > name
        assert_eq!(m.height(), 10);
        assert!(all_attached(&m));
        Ok(())
    }

    #[test]
    fn rejected_placed_fn_releases_its_parts() -> R {
        let mut m = Module::new();
        let ty = m.named_ty("Int")?;
        let cond = m.keyword(KeywordValue::Signed)?;
        let body = m.block_of(vec![])?;
        let decl = |params: Vec<TypedParam>, body| PlacedFnDecl {
            placement: Placement::Client,
            name: Ident::bare("f"),
            params: params.into_boxed_slice(),
            result: Some(ty),
            elevated: Some(TextRange::default()),
            call_rule: Some(CallRule {
                cond,
                range: TextRange::default(),
            }),
            body,
            range: TextRange::default(),
        };
        // The result type is also used by the parameter.
        let bad = decl(vec![TypedParam::bare("a", ty)], body);
        assert_eq!(
            m.add_item(Item::PlacedFn(Box::new(bad))),
            Err(AstError::AlreadyAttached)
        );
        assert!(m.items().is_empty());
        m.add_item(Item::PlacedFn(Box::new(decl(vec![], body))))?;
        assert!(all_attached(&m));
        Ok(())
    }

    #[test]
    fn var_and_style_items() -> R {
        let mut m = Module::new();
        let room = m.ty_of("Room", vec![], true)?;
        m.var_item(
            "current",
            VarInit::Typed {
                ty: room,
                value: None,
            },
        )?;
        let draft = m.text("")?;
        m.var_item("draft", VarInit::Inferred(draft))?;
        let body = TextRange::new(10, 40).unwrap_or_default();
        m.style_item(body)?;
        assert!(matches!(
            m.items().get(2),
            Some(Item::Style(style)) if style.body == body
        ));
        // root, type, `""`, three items
        assert_eq!(m.node_count(), 6);
        assert_eq!(m.height(), 3);
        Ok(())
    }

    /// Builds the chat of SYNTAX 5.1 (variant A) with the constructors. It
    /// uses every construct of SYNTAX 4.1 to 4.8 except `enum`, `merge`,
    /// field modes and functions, which other tests cover.
    #[test]
    fn builds_chat_variant_a() -> R {
        let mut m = Module::new();
        m.app_item("Chat", vec![AppItem::auth("paseto", Some("open"))])?;

        // data Room
        let mut room = room_members(&mut m)?;
        let members = m.name("members")?;
        let me = m.keyword(KeywordValue::Me)?;
        let only_me = m.set(vec![me])?;
        let make_rule = m.binary(BinaryOp::Eq, members, only_me)?;
        let added = m.name("added")?;
        let me = m.keyword(KeywordValue::Me)?;
        let set_me = m.set(vec![me])?;
        let join = m.binary(BinaryOp::Eq, added, set_me)?;
        let removed = m.name("removed")?;
        let me = m.keyword(KeywordValue::Me)?;
        let set_me = m.set(vec![me])?;
        let leave = m.binary(BinaryOp::Eq, removed, set_me)?;
        let edit_rule = m.binary(BinaryOp::Or, join, leave)?;
        room.push(Member::rule(RuleVerb::Make, None, make_rule));
        room.push(Member::rule(RuleVerb::Edit, Some("members"), edit_rule));
        m.data_item("Room", room)?;

        // data Message
        let room_ty = m.named_ty("Room")?;
        let text_ty = m.named_ty("Text")?;
        let text = m.name("text")?;
        let len = m.field(text, "len")?;
        let one = m.int(1)?;
        let max = m.int(2000)?;
        let bounds = m.range(one, max)?;
        let check = m.in_expr(len, bounds, false)?;
        let mut rule_conds = Vec::new();
        for _ in 0..2 {
            let me = m.keyword(KeywordValue::Me)?;
            let room = m.name("room")?;
            let members = m.field(room, "members")?;
            rule_conds.push(m.in_expr(me, members, false)?);
        }
        let [see, make] = rule_conds[..] else {
            return Err(AstError::UnknownId);
        };
        m.data_item(
            "Message",
            vec![
                Member::field(FieldDecl::new("room", room_ty)),
                Member::field(FieldDecl::new("text", text_ty)),
                Member::check(check),
                Member::rule(RuleVerb::See, None, see),
                Member::rule(RuleVerb::Make, None, make),
            ],
        )?;

        // var current: Room?
        let current_ty = m.ty_of("Room", vec![], true)?;
        m.var_item(
            "current",
            VarInit::Typed {
                ty: current_ty,
                value: None,
            },
        )?;

        // Sidebar.
        let rooms = m.query("Room", None, Some(("name", false)), None)?;
        let r = m.name("r")?;
        let label = m.field(r, "name")?;
        let r = m.name("r")?;
        let current = m.name("current")?;
        let selected = m.binary(BinaryOp::Eq, r, current)?;
        let target = m.name("current")?;
        let r = m.name("r")?;
        let button = m.element(
            "button",
            Some(label),
            vec![Attr::named("selected", selected)],
            Some(Handler::bare("click", target, Some(r))),
            vec![],
        )?;
        let room_list = m.view_for("r", rooms, vec![button])?;
        let hint = m.text("New room")?;
        let submit = m.text("Create")?;
        let target = m.name("current")?;
        let it = m.name("it")?;
        let new_room = m.make("Room", vec![("name", it)])?;
        let entry = m.element(
            "entry",
            None,
            vec![Attr::named("hint", hint), Attr::named("submit", submit)],
            None,
            vec![ElementChild::Handler(Handler::bare(
                "submit",
                target,
                Some(new_room),
            ))],
        )?;
        let sync = m.name("sync")?;
        let state = m.field(sync, "state")?;
        let muted = m.name("muted")?;
        let status = m.element(
            "text",
            Some(state),
            vec![Attr::named("look", muted)],
            None,
            vec![],
        )?;
        let sidebar = m.element(
            "col",
            None,
            vec![],
            None,
            [room_list, entry, status]
                .into_iter()
                .map(ElementChild::Node)
                .collect(),
        )?;

        // Message log with `~mine(m.author == me)`.
        let room_field = m.name("room")?;
        let r = m.name("r")?;
        let filter = m.binary(BinaryOp::Eq, room_field, r)?;
        let n = m.int(200)?;
        let log_query = m.query(
            "Message",
            Some(filter),
            Some(("made", false)),
            Some((TakeKind::Last, n)),
        )?;
        let msg = m.name("m")?;
        let author = m.field(msg, "author")?;
        let me = m.keyword(KeywordValue::Me)?;
        let mine = m.binary(BinaryOp::Eq, author, me)?;
        let msg = m.name("m")?;
        let body = m.field(msg, "text")?;
        let body = m.element("text", Some(body), vec![], None, vec![])?;
        let row = m.element(
            "row",
            None,
            vec![Attr::style("mine", Some(mine))],
            None,
            vec![ElementChild::Node(body)],
        )?;
        let log_for = m.view_for("m", log_query, vec![row])?;
        let end = m.name("end")?;
        let log = m.element(
            "col",
            None,
            vec![Attr::named("scroll", end)],
            None,
            vec![ElementChild::Node(log_for)],
        )?;
        let current = m.name("current")?;
        let placeholder = m.text("Pick or create a room")?;
        let muted = m.name("muted")?;
        let empty = m.element(
            "text",
            Some(placeholder),
            vec![Attr::named("look", muted)],
            None,
            vec![],
        )?;
        let main = m.view_if(
            Cond::Let {
                name: Ident::bare("r"),
                value: current,
            },
            vec![log],
            Some(vec![empty]),
        )?;
        let main_col = m.element("col", None, vec![], None, vec![ElementChild::Node(main)])?;
        let split = m.element(
            "split",
            None,
            vec![],
            None,
            vec![ElementChild::Node(sidebar), ElementChild::Node(main_col)],
        )?;
        m.view_item("Main", None, vec![split])?;
        m.style_item(TextRange::default())?;

        let kinds: Vec<&str> = m
            .items()
            .iter()
            .map(|item| match item {
                Item::App(_) => "app",
                Item::Data(_) => "data",
                Item::Var(_) => "var",
                Item::View(_) => "view",
                Item::Style(_) => "style",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["app", "data", "data", "var", "view", "style"]);
        assert!(all_attached(&m));
        // view > split > col > if > col > for > row > ~mine > `==` > field > name
        assert_eq!(m.height(), 12);
        Ok(())
    }

    /// A view element with many children and attributes stays linear in its
    /// size (AC-04, D74), like the wide v0.1 nodes.
    fn wide_view(wide: usize) -> Result<Module, AstError> {
        let mut m = Module::new();
        let mut children = Vec::with_capacity(wide);
        let mut attrs = Vec::with_capacity(wide);
        for _ in 0..wide {
            let node = m.element("text", None, vec![], None, vec![])?;
            children.push(ElementChild::Node(node));
            let cond = m.bool(true)?;
            attrs.push(Attr::style("s", Some(cond)));
        }
        let col = m.element("col", None, attrs, None, children)?;
        m.view_item("V", None, vec![col])?;
        Ok(m)
    }

    #[test]
    fn wide_view_counts_its_nodes() -> R {
        const WIDE: usize = 100_000;
        let m = wide_view(WIDE)?;
        // root, item, col, then per child: node, attr, condition
        assert_eq!(m.node_count(), 3 + 3 * WIDE as u32);
        Ok(())
    }

    #[test]
    fn wide_view_is_linear() {
        crate::test_support::assert_linear(4_000, |n| n, |&n| wide_view(n).ok());
    }
}
