//! High level intermediate representation (HIR), freeze F2 (ARCHITECTURE 3, 13).
//!
//! The HIR is the program after name resolution: one [`Program`] for all
//! source files, every top level declaration a [`Def`] with a [`DefId`], every
//! name in a body a [`Res`]. [`resolve`] builds it from syntax trees and
//! [`check`] adds types ([`Ty`], `docs/types.md`), placement ([`Placement`],
//! ARCHITECTURE 4.3) and the rule checks (ARCHITECTURE 9). Lowering reads only
//! the HIR (`ostrel_ir::lower(&hir::Program)`).
//!
//! Functions have one node (D95): `ast::Item::Fn` and `ast::Item::PlacedFn`
//! both become [`DefKind::Fn`]. Rules of models and the `call` rule of a
//! function are the same node, [`Rule`], with the operation in [`RuleOp`]
//! (D19).
//!
//! Storage: one arena per node kind in [`Program`], indexed by `u32` ids
//! (ARCHITECTURE 3.1). Ids are only meaningful for the program that made
//! them. The HIR grows by addition; enums are `non_exhaustive` where a later
//! milestone adds variants (`extern` blocks arrive with the bridge).
//!
//! Status: stub. The types are the freeze proposal; [`resolve`] and [`check`]
//! have their final shape but no implementation yet. The v0.1 checker in the
//! crate root stays the one the CLI calls until the move to the HIR.

mod body;
mod ty;

#[cfg(test)]
mod tests;

pub use body::{
    Block, ContextName, Element, ElementName, Expr, ExprKind, Handler, Query, Res, StdName, Stmt,
    StmtKind, StrPart, ViewNode, ViewNodeKind,
};
pub use ty::Ty;

use ostrel_core::{Diagnostic, FileId, Span};
use ostrel_syntax::ast;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);

        impl $name {
            /// Position in its arena.
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

id_type!(
    /// Id of a top level [`Def`] in [`Program::defs`].
    DefId
);
id_type!(
    /// Id of a [`Local`] in [`Program::locals`].
    LocalId
);
id_type!(
    /// Position of a field in [`ModelDef::fields`].
    FieldId
);
id_type!(
    /// Id of an [`Expr`] in [`Program::exprs`].
    ExprId
);
id_type!(
    /// Id of a [`Stmt`] in [`Program::stmts`].
    StmtId
);
id_type!(
    /// Id of a [`Block`] in [`Program::blocks`].
    BlockId
);
id_type!(
    /// Id of a [`ViewNode`] in [`Program::view_nodes`].
    ViewNodeId
);

/// A field of a model as a name in a body refers to it: declared in the model
/// or implicit on every row (SYNTAX 4.2, ARCHITECTURE 5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldRef {
    /// A field of [`ModelDef::fields`].
    Declared(FieldId),
    /// A field every row has without declaring it.
    Implicit(ImplicitField),
}

/// The implicit fields. Their types are fixed by the language, so they have
/// no [`FieldDef`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImplicitField {
    /// `id`, assigned by the runtime, never writable.
    Id,
    /// `made`, the server accepted stamp of the `make`.
    Made,
    /// `changed`, the server accepted stamp of the last write.
    Changed,
    /// `author`, only in an app with `auth` (G8).
    Author,
    /// `pending`, client only sync state (ARCHITECTURE 5.1).
    Pending,
    /// `rejected`, client only sync state (ARCHITECTURE 5.1).
    Rejected,
}

/// One source file as input to [`resolve`]: the tree and what the tree does
/// not know, its file and its text (positions of operators, ARCHITECTURE 3).
#[derive(Clone, Copy, Debug)]
pub struct Source<'a> {
    /// The file in the source map.
    pub file: FileId,
    /// The text the tree was parsed from.
    pub text: &'a str,
    /// The syntax tree.
    pub module: &'a ast::Module,
}

/// A whole program after name resolution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    /// Top level definitions of all files, in file order, then source order.
    pub defs: Vec<Def>,
    /// The `app` definition; `None` for a script program (G15).
    pub app: Option<DefId>,
    /// Parameters and bindings of every function, view and handler.
    pub locals: Vec<Local>,
    /// All expressions.
    pub exprs: Vec<Expr>,
    /// All statements.
    pub stmts: Vec<Stmt>,
    /// All blocks.
    pub blocks: Vec<Block>,
    /// All view nodes.
    pub view_nodes: Vec<ViewNode>,
}

impl Program {
    /// The definition with this id.
    pub fn def(&self, id: DefId) -> Option<&Def> {
        self.defs.get(id.index())
    }

    /// The local with this id.
    pub fn local(&self, id: LocalId) -> Option<&Local> {
        self.locals.get(id.index())
    }

    /// The expression with this id.
    pub fn expr(&self, id: ExprId) -> Option<&Expr> {
        self.exprs.get(id.index())
    }

    /// The statement with this id.
    pub fn stmt(&self, id: StmtId) -> Option<&Stmt> {
        self.stmts.get(id.index())
    }

    /// The block with this id.
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.get(id.index())
    }

    /// The view node with this id.
    pub fn view_node(&self, id: ViewNodeId) -> Option<&ViewNode> {
        self.view_nodes.get(id.index())
    }

    /// A declared field of a model; implicit fields have no [`FieldDef`].
    pub fn field(&self, model: DefId, field: FieldId) -> Option<&FieldDef> {
        match &self.def(model)?.kind {
            DefKind::Model(m) => m.fields.get(field.index()),
            _ => None,
        }
    }

    /// True if `serial per key` on a field of `model` numbers rows in groups
    /// that a row never leaves (D136, SYNTAX 4.2): a `fixed` field of type
    /// `Bool`, `Int`, `Text` without `merge text`, `Time`, `Bytes`, an enum
    /// or a reference, or the implicit `author`. Every other key, and a key
    /// that is not a field of `model`, is a compile error at the key.
    pub fn is_serial_key(&self, model: DefId, key: FieldRef) -> bool {
        match key {
            FieldRef::Implicit(field) => field == ImplicitField::Author,
            FieldRef::Declared(field) => self.field(model, field).is_some_and(|f| {
                f.mode == FieldMode::Fixed
                    && f.merge == Merge::Default
                    && matches!(
                        f.ty,
                        Ty::Bool
                            | Ty::Int
                            | Ty::Text
                            | Ty::Time
                            | Ty::Bytes
                            | Ty::Enum(_)
                            | Ty::Ref(_)
                    )
            }),
        }
    }
}

/// A top level declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Def {
    /// The declared name.
    pub name: Box<str>,
    /// The whole declaration.
    pub span: Span,
    /// What is declared.
    pub kind: DefKind,
}

/// The kinds of top level declarations.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DefKind {
    /// `app Name` (SYNTAX 4.1).
    App(AppDef),
    /// `enum Name = a | b`; variants in declaration order.
    Enum(Box<[Box<str>]>),
    /// `data Name`, a model.
    Model(ModelDef),
    /// `var name`, client state (SYNTAX 4.6).
    Var(VarDef),
    /// Every function, placed or not (D95).
    Fn(FnDef),
    /// `view Name(params)`.
    View(ViewDef),
    /// `style`; the CSS stays source text, owned by the formatter and codegen.
    Style,
}

/// `app` lines after validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDef {
    /// `auth paseto open|invite`; `None` without auth (AC-46).
    pub auth: Option<Registration>,
    /// The `home` view.
    pub home: Option<DefId>,
}

/// Registration mode of `auth paseto` (D33).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Registration {
    /// `auth paseto open`.
    Open,
    /// `auth paseto invite`.
    Invite,
}

/// A model with its fields, rules and checks. The implicit fields are not
/// listed; names in bodies refer to them by [`FieldRef::Implicit`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelDef {
    /// Declared fields in source order, indexed by [`FieldId`].
    pub fields: Vec<FieldDef>,
    /// `see`, `make`, `edit` and `drop` rules in source order.
    pub rules: Vec<Rule>,
    /// `check` invariants in source order.
    pub checks: Vec<ExprId>,
}

/// One declared field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDef {
    /// The field name.
    pub name: Box<str>,
    /// The declaration line.
    pub span: Span,
    /// The field type; [`Ty::is_field_type`] holds after `check`.
    pub ty: Ty,
    /// The default after `=`.
    pub default: Option<ExprId>,
    /// The write mode.
    pub mode: FieldMode,
    /// The merge strategy.
    pub merge: Merge,
}

/// Who may write a field (SYNTAX 4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldMode {
    /// No mode: written under the `edit` rules.
    Plain,
    /// `fixed`: set by `make` only.
    Fixed,
    /// `server`: written by server code only.
    Server,
    /// `serial [per f]`: assigned by the server (ARCHITECTURE 5.1). The
    /// field after `per` may be any field name; [`Program::is_serial_key`]
    /// says whether `check` accepts it (D136).
    Serial(Option<FieldRef>),
}

/// Merge strategy of a field (SYNTAX 4.3, ARCHITECTURE 5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Merge {
    /// The default merge of the field type.
    Default,
    /// `merge text`: the sequence CRDT, `Text` fields only.
    Text,
}

/// A permission rule (ARCHITECTURE 9). Default deny: an operation without a
/// rule is not allowed (D8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The guarded operation.
    pub op: RuleOp,
    /// The field of a field rule; `None` for a record rule and for `call`.
    pub field: Option<FieldId>,
    /// The condition; a pure `Bool` expression after `check`.
    pub cond: ExprId,
    /// The rule line.
    pub span: Span,
}

/// The operations a rule guards (D19).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuleOp {
    /// `see` on a model.
    See,
    /// `make` on a model.
    Make,
    /// `edit` on a model or field.
    Edit,
    /// `drop` on a model.
    Drop,
    /// `call` on a server function.
    Call,
}

/// A top level `var`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VarDef {
    /// The declared type, or the inferred one after `check`.
    pub ty: Option<Ty>,
    /// The initial value.
    pub value: Option<ExprId>,
}

/// Where code runs (ARCHITECTURE 4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Placement {
    /// Pure, compiled to both targets.
    Shared,
    /// API server only.
    Server,
    /// Browser only.
    Client,
}

/// A function: `fn`, `server fn`, `client fn`, with or without `as system`
/// and `call` rule (D95).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnDef {
    /// `server` or `client` as written; wins over inference. Never
    /// [`Placement::Shared`], which is only inferred.
    pub declared: Option<Placement>,
    /// The placement: `declared` after `resolve`, the inferred one after
    /// `check` when nothing was declared.
    pub placement: Option<Placement>,
    /// Parameters in order, each a local with its declared type.
    pub params: Box<[LocalId]>,
    /// The result type; [`Ty::Unit`] without `-> T`.
    pub result: Ty,
    /// The span of `as system` (G13, D34).
    pub elevated: Option<Span>,
    /// The `call` rule, with [`RuleOp::Call`].
    pub call_rule: Option<Rule>,
    /// The body after the `call` rule.
    pub body: BlockId,
}

/// A view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewDef {
    /// Parameters in order.
    pub params: Box<[LocalId]>,
    /// Top level nodes in source order.
    pub nodes: Box<[ViewNodeId]>,
}

/// A parameter or local binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Local {
    /// The name.
    pub name: Box<str>,
    /// Where it is bound.
    pub span: Span,
    /// The declared type, or the inferred one after `check`.
    pub ty: Option<Ty>,
}

/// Resolves the names of all files of one program and builds its HIR.
///
/// Never panics on any tree. Reports unknown and duplicate names; the result
/// is complete when no diagnostic is an error.
///
/// Stub: reports every non empty input as not yet supported (`E0100`) and
/// returns an empty program.
pub fn resolve(sources: &[Source<'_>]) -> (Program, Vec<Diagnostic>) {
    let diagnostics = sources
        .iter()
        .find(|s| !s.module.items().is_empty())
        .map(|s| {
            Diagnostic::error(
                crate::codes::NOT_IN_V0_1,
                Span::point(s.file, 0),
                "this program needs the v0.2 checker, which is not available yet",
            )
        })
        .into_iter()
        .collect();
    (Program::default(), diagnostics)
}

/// Checks types, placement and rules, and fills [`Expr::ty`],
/// [`FnDef::placement`], [`Local::ty`] and [`VarDef::ty`].
///
/// Runs only on a program whose [`resolve`] reported no error.
///
/// Stub: checks nothing and reports nothing.
pub fn check(program: &mut Program) -> Vec<Diagnostic> {
    let _ = program;
    Vec::new()
}
