//! Bodies: expressions, statements, blocks and view nodes.
//!
//! The shapes follow `ostrel_syntax::ast` with three differences: every name
//! is resolved to a [`Res`], every node carries a [`Span`] with its file, and
//! nodes that only keep the source layout (parentheses) are gone.

use ostrel_core::Span;
use ostrel_syntax::ast::{BinaryOp, TakeKind, UnaryOp};

use super::{BlockId, DefId, ExprId, FieldId, FieldRef, LocalId, StmtId, Ty, ViewNodeId};

/// What a name refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Res {
    /// A parameter, `let`, loop variable or `if let` binding.
    Local(LocalId),
    /// A top level definition: function, model, enum, `var`, view.
    Def(DefId),
    /// A field of the model in scope: a bare name in a rule, a `check`, or the
    /// `where` and `sort` of a query (SYNTAX 4.4, 4.5).
    Field(DefId, FieldRef),
    /// A variant of an enum, by position in declaration order.
    Variant(DefId, u32),
    /// A contextual name of SYNTAX 6 in the place where it has meaning.
    Context(ContextName),
    /// A name of the prelude or the standard library.
    Std(StdName),
    /// The name did not resolve; a diagnostic was reported.
    Error,
}

/// The contextual names that `resolve` binds (SYNTAX 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContextName {
    /// `new` in an `edit` rule: the proposed record.
    New,
    /// `added` in a field rule on a `Set` or `Map` field.
    Added,
    /// `removed` in a field rule on a `Set` or `Map` field.
    Removed,
    /// `it` in an `on` action.
    It,
}

/// Library names the checker knows by name. Grows by addition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StdName {
    /// `print`, script programs only.
    Print,
    /// `sync`, the sync state of the client (SYNTAX 4.7).
    Sync,
    /// `Url`, as in `Url.parse(t)` (SYNTAX 4.8); in a type position it is
    /// [`Ty::Url`].
    Url,
}

/// An expression with its source position and, after `check`, its type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expr {
    /// What kind of expression this is.
    pub kind: ExprKind,
    /// The source text of the whole expression.
    pub span: Span,
    /// `None` after `resolve`, set for every expression by `check`.
    pub ty: Option<Ty>,
}

/// The expressions of the language. Same order as `ast::ExprKind`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExprKind {
    /// An integer literal, already range checked (G9).
    Int(i64),
    /// `true` or `false`.
    Bool(bool),
    /// A string literal with interpolations.
    Str(Box<[StrPart]>),
    /// A resolved name.
    Name(Res),
    /// A bare word whose meaning follows from the expected type (D142): an
    /// element attribute value that is a bare name, or a name that only
    /// several enums declare as a variant. `resolve` reports nothing for it;
    /// `check` replaces it with [`ExprKind::Name`].
    Word {
        /// The word.
        name: Box<str>,
        /// What the name binds at its position; [`Res::Error`] if nothing
        /// or only several enums bind it.
        lexical: Res,
    },
    /// A prefix operator.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        operand: ExprId,
    },
    /// A binary operator.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// The left operand.
        lhs: ExprId,
        /// The right operand.
        rhs: ExprId,
    },
    /// A call. The callee is an expression so that the checker reports
    /// calls of non functions in one place.
    Call {
        /// The called expression.
        callee: ExprId,
        /// Arguments in source order.
        args: Box<[ExprId]>,
    },
    /// `none`.
    NoneValue,
    /// `me`.
    Me,
    /// `now`.
    Now,
    /// `signed`.
    Signed,
    /// `base.field` on a record, resolved to the field.
    Field {
        /// The record.
        base: ExprId,
        /// The model and field.
        field: (DefId, FieldRef),
    },
    /// `base.name` that is neither a declared nor an implicit model field,
    /// for example `text.len`; `check` decides what it means for the type of
    /// `base`.
    Property {
        /// The accessed value.
        base: ExprId,
        /// The property name.
        name: Box<str>,
    },
    /// `base[index]`.
    Index {
        /// The indexed value.
        base: ExprId,
        /// The key or position.
        index: ExprId,
    },
    /// `element in collection` or `element not in collection`.
    In {
        /// The searched value.
        element: ExprId,
        /// The collection or range.
        collection: ExprId,
        /// True for `not in`.
        negated: bool,
    },
    /// The inclusive range `lo..hi`.
    Range {
        /// The first value.
        lo: ExprId,
        /// The last value, included.
        hi: ExprId,
    },
    /// `value catch Kind, ...`. The kinds are the names of `api.d.ts`.
    Catch {
        /// The remote value.
        value: ExprId,
        /// The named error kinds.
        kinds: Box<[Box<str>]>,
    },
    /// `value ?? fallback`.
    Fallback {
        /// The optional or remote value.
        value: ExprId,
        /// The fallback.
        fallback: ExprId,
    },
    /// `{a, b}`.
    Set(Box<[ExprId]>),
    /// `[a, b]`.
    List(Box<[ExprId]>),
    /// `[k: v]`.
    Map(Box<[(ExprId, ExprId)]>),
    /// `make Model { f: e }`, fields resolved. Only declared fields: implicit
    /// fields are never written by `make` (SYNTAX 4.2).
    Make {
        /// The model.
        model: DefId,
        /// Field values in source order.
        fields: Box<[(FieldId, ExprId)]>,
    },
    /// A query over a model (SYNTAX 4.5). A query head that names an enum is
    /// [`ExprKind::Name`] with [`Res::Def`] instead.
    Query(Box<Query>),
}

/// One piece of a string literal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StrPart {
    /// Literal text.
    Text(Box<str>),
    /// An interpolation.
    Interp(ExprId),
}

/// `Model [where e] [sort f [desc]] [limit n | last n]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    /// The queried model.
    pub model: DefId,
    /// The `where` condition; its bare names are [`Res::Field`].
    pub filter: Option<ExprId>,
    /// The sort field and `desc`; the tie break by `id` is implied.
    pub sort: Option<(FieldRef, bool)>,
    /// `limit n` or `last n`.
    pub take: Option<(TakeKind, ExprId)>,
}

/// An indented sequence of statements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Statements in source order.
    pub stmts: Box<[StmtId]>,
    /// The source of the block.
    pub span: Span,
}

/// A statement with its source position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stmt {
    /// What kind of statement this is.
    pub kind: StmtKind,
    /// The source of the statement.
    pub span: Span,
}

/// The statements of a body. Same order as `ast::StmtKind`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StmtKind {
    /// `let name = value`.
    Let {
        /// The bound local.
        local: LocalId,
        /// The value.
        value: ExprId,
    },
    /// `if cond` with optional `else`.
    If {
        /// The condition.
        cond: ExprId,
        /// The block run when the condition holds.
        then_block: BlockId,
        /// The `else` block.
        else_block: Option<BlockId>,
    },
    /// `return value`.
    Return(ExprId),
    /// An expression statement.
    Expr(ExprId),
    /// `if let name = value` with optional `else`.
    IfLet {
        /// The local bound in `then_block`.
        local: LocalId,
        /// The unwrapped optional.
        value: ExprId,
        /// The block run when the value is present.
        then_block: BlockId,
        /// The `else` block.
        else_block: Option<BlockId>,
    },
    /// `for name in source`.
    For {
        /// The loop variable, bound only in `body` (SPEC 12.10).
        local: LocalId,
        /// What is iterated.
        source: ExprId,
        /// The loop body.
        body: BlockId,
    },
    /// `drop source`.
    Drop(ExprId),
    /// `target = value`.
    Assign {
        /// The assigned place.
        target: ExprId,
        /// The new value.
        value: ExprId,
    },
}

/// A node of a view tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewNode {
    /// What kind of node this is.
    pub kind: ViewNodeKind,
    /// The source of the node.
    pub span: Span,
}

/// The view nodes. Same order as `ast::ViewNodeKind`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ViewNodeKind {
    /// An element or the use of another view.
    Element(Box<Element>),
    /// `if cond` or `if let`.
    If {
        /// The condition, a `Bool` expression, or the optional for `if let`.
        cond: ExprId,
        /// The local of `if let`, bound in `then_nodes`.
        bind: Option<LocalId>,
        /// Nodes shown when the condition holds.
        then_nodes: Box<[ViewNodeId]>,
        /// Nodes after `else`.
        else_nodes: Box<[ViewNodeId]>,
    },
    /// `for name in source`; a query source is live.
    For {
        /// The loop variable.
        local: LocalId,
        /// What is iterated.
        source: ExprId,
        /// Nodes shown per item.
        nodes: Box<[ViewNodeId]>,
    },
}

/// What an element line names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ElementName {
    /// A std element such as `row` or `text`; validated by `check`.
    Std(Box<str>),
    /// A view of the program.
    View(DefId),
}

/// An element line with attributes, handlers and children.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    /// The element or view.
    pub name: ElementName,
    /// The value after the name.
    pub value: Option<ExprId>,
    /// `name: value` attributes in source order.
    pub attrs: Box<[(Box<str>, ExprId)]>,
    /// `~name` and `~name(cond)` style tags in source order.
    pub styles: Box<[(Box<str>, Option<ExprId>)]>,
    /// `on` handlers, from the element line and indented lines, in source
    /// order.
    pub handlers: Box<[Handler]>,
    /// Child nodes in source order.
    pub children: Box<[ViewNodeId]>,
}

/// `on event action` or `on event target = value`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handler {
    /// The event name.
    pub event: Box<str>,
    /// The action, or the assigned place when `assign` is set.
    pub action: ExprId,
    /// The assigned value.
    pub assign: Option<ExprId>,
    /// The source of the handler.
    pub span: Span,
}
