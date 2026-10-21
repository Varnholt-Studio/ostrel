//! The full IR, freeze F2 (ARCHITECTURE 3, 4.3, 5.7, 9, 13).
//!
//! One [`Program`] holds both targets. Every function has a [`Placement`]:
//! the server backend (`ostrel_vm`) compiles the [`Placement::Server`] and
//! [`Placement::Shared`] functions, the browser backend (`ostrel_codegen_js`)
//! the [`Placement::Client`] and shared ones ([`Program::target`]). Across the
//! split there is exactly one way: a client function calls a server function
//! with [`InstKind::RemoteCall`] (`Call` and `Return` over the WebSocket, G11)
//! and waits for it with [`InstKind::Settle`]. Server code never calls client
//! code (ARCHITECTURE 4.3).
//!
//! Rules are part of the IR, not of a backend:
//! * `see` is a [`query::Filter`] on the model and is written into every
//!   server query as [`query::See::Rule`], so unreadable rows never leave the
//!   database (ARCHITECTURE 9).
//! * `make`, `edit`, `drop`, `call` and `check` are predicate functions
//!   ([`FnKind::Predicate`]); every write instruction names the [`Auth`] it
//!   runs under, and under [`Auth::Caller`] the runtime evaluates the rules of
//!   the op exactly as for a sync op.
//! * A server function has a [`Prologue`] that the VM runs on every entry, from
//!   a client and from server code alike, in the fixed order of D99 and D117:
//!   resolve model arguments through `see`, evaluate the `call` rule, write
//!   the audit entry of elevated code, then run the body (D98).
//!
//! Status: stub. The types are the freeze proposal and [`Program::validate`]
//! checks the placement and authority contract; [`lower()`] lowers the schema
//! (enums and models) and no function bodies yet. The v0.1 slice in the crate
//! root stays what the CLI and the VM use until the move to the HIR; it is not
//! changed by this module.

mod inst;
mod lower;
pub mod query;
mod ty;
mod validate;
pub mod view;

#[cfg(test)]
mod tests;

pub use inst::{
    Auth, BinOp, CallErrorKind, Caught, Const, Cost, Delta, Inst, InstKind, RowState, StdFn,
    Terminator, UnOp, Work,
};
pub use lower::{LowerError, lower};
pub use ty::Ty;
pub use validate::{Problem, Violation};

use ostrel_core::Span;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);

        impl $name {
            /// Position in its list.
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

id_type!(
    /// Index of a function in [`Program::functions`].
    FuncId
);
id_type!(
    /// Index of a block in [`Function::blocks`]. Block 0 is the entry block.
    BlockId
);
id_type!(
    /// Index of a local in [`Function::locals`]. Parameters are the first
    /// locals.
    Local
);
id_type!(
    /// Index of a model in [`Program::models`]. The same position is the
    /// `ModelId` of the compiled schema (ARCHITECTURE 5.4, D135).
    ModelId
);
id_type!(
    /// Index of a declared field in [`Model::fields`], the `FieldId` of the
    /// compiled schema.
    FieldId
);
id_type!(
    /// Index of an enum in [`Program::enums`].
    EnumId
);
id_type!(
    /// Index of a rule in [`Program::rules`].
    RuleId
);
id_type!(
    /// Index of a top level `var` in [`Program::vars`].
    VarId
);
id_type!(
    /// Index of a view in [`Program::views`].
    ViewId
);

/// A field of a model: declared or implicit on every row (SYNTAX 4.2,
/// ARCHITECTURE 5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldRef {
    /// A field of [`Model::fields`].
    Declared(FieldId),
    /// A field every row has without declaring it.
    Implicit(ImplicitField),
}

/// The implicit fields; their types are fixed by the language.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImplicitField {
    /// `id`, assigned by the runtime, never writable.
    Id,
    /// `made`, the accepted stamp of the `make`.
    Made,
    /// `changed`, the accepted stamp of the last write.
    Changed,
    /// `author`, only in an app with `auth` (G8).
    Author,
    /// `pending`, client only.
    Pending,
    /// `rejected`, client only.
    Rejected,
}

impl ImplicitField {
    /// True for the fields that exist only on the client (ARCHITECTURE 5.1).
    pub fn is_client_only(self) -> bool {
        matches!(self, ImplicitField::Pending | ImplicitField::Rejected)
    }
}

/// Where code runs (ARCHITECTURE 4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Placement {
    /// Pure; compiled to both targets.
    Shared,
    /// API server only.
    Server,
    /// Browser only.
    Client,
}

/// One of the two outputs of the compiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// The server process with the bytecode VM.
    Server,
    /// The browser bundle.
    Client,
}

impl Placement {
    /// True when code with this placement is compiled into `target`.
    pub fn runs_on(self, target: Target) -> bool {
        matches!(
            (self, target),
            (Placement::Shared, _)
                | (Placement::Server, Target::Server)
                | (Placement::Client, Target::Client)
        )
    }
}

/// A whole program after lowering, both targets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    /// The `app` declaration; `None` for a script program (G15).
    pub app: Option<App>,
    /// Enums in declaration order.
    pub enums: Vec<Enum>,
    /// Models in declaration order (std models such as `User` included); the
    /// schema the server, the database and the wire codec work with.
    pub models: Vec<Model>,
    /// Every rule of every model and function.
    pub rules: Vec<Rule>,
    /// Top level `var`s, client state.
    pub vars: Vec<Var>,
    /// All functions of both targets.
    pub functions: Vec<Function>,
    /// All views.
    pub views: Vec<view::View>,
    /// The `style` blocks; the CSS stays source text, the browser backend
    /// reads it at these positions.
    pub styles: Vec<Span>,
    /// `fn main()` of a script program: a source function without
    /// parameters; always `None` with an `app` (G15).
    pub main: Option<FuncId>,
}

impl Program {
    /// A function by id.
    pub fn function(&self, id: FuncId) -> Option<&Function> {
        self.functions.get(id.index())
    }

    /// A model by id.
    pub fn model(&self, id: ModelId) -> Option<&Model> {
        self.models.get(id.index())
    }

    /// A rule by id.
    pub fn rule(&self, id: RuleId) -> Option<&Rule> {
        self.rules.get(id.index())
    }

    /// The std `User` model, the type of `me` and of the implicit `author`:
    /// the model named `User` in a program whose `app` has `auth`, `None`
    /// otherwise (G8, D142). With `auth` the name is taken by the std model, a
    /// declared `User` beside it is a compile error (E0201), so the name
    /// identifies it.
    pub fn user_model(&self) -> Option<ModelId> {
        self.app.as_ref()?.auth?;
        self.models
            .iter()
            .position(|m| m.name == "User")
            .map(|i| ModelId(i as u32))
    }

    /// A declared field of a model.
    pub fn field(&self, model: ModelId, field: FieldId) -> Option<&Field> {
        self.model(model)?.fields.get(field.index())
    }

    /// The functions compiled into `target`, in id order.
    pub fn target(&self, target: Target) -> impl Iterator<Item = (FuncId, &Function)> + '_ {
        self.functions
            .iter()
            .enumerate()
            .filter(move |(_, f)| f.placement.runs_on(target))
            .map(|(i, f)| (FuncId(i as u32), f))
    }

    /// Checks the placement and authority contract of the IR (see
    /// [`Problem`]) and returns every violation, in function and block order.
    /// A program built by [`lower()`] from a checked HIR has none; a violation
    /// is a defect of the lowering.
    pub fn validate(&self) -> Vec<Violation> {
        validate::validate(self)
    }
}

/// `app` after lowering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// The app name.
    pub name: String,
    /// `auth paseto open|invite`; `None` without auth (AC-46).
    pub auth: Option<Registration>,
    /// The home view.
    pub home: Option<ViewId>,
}

/// Registration mode of `auth paseto` (D33).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Registration {
    /// `auth paseto open`.
    Open,
    /// `auth paseto invite`.
    Invite,
}

/// An enum; variants in declaration order, which defines `<`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Enum {
    /// The enum name.
    pub name: String,
    /// The variant names.
    pub variants: Vec<String>,
}

/// A model: fields, rules and invariants. Names travel on the wire, ids stay
/// in memory (D132, D135).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    /// The model name.
    pub name: String,
    /// Declared fields in source order, indexed by [`FieldId`].
    pub fields: Vec<Field>,
    /// The `see` rule as a filter of the pushdown subset; `None` means no row
    /// is readable (default deny, D8).
    pub see: Option<query::Filter>,
    /// The `make`, `edit` and `drop` rules of the model, record and field
    /// rules, in source order. An op with no rule for its verb is denied (D8).
    pub rules: Vec<RuleId>,
    /// The `check` invariants: predicate functions over the proposed row.
    pub checks: Vec<FuncId>,
    /// The declaration.
    pub span: Span,
}

/// One declared field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// The field name.
    pub name: String,
    /// The field type; [`Ty::is_field_type`] holds.
    pub ty: Ty,
    /// The write mode.
    pub mode: FieldMode,
    /// The merge strategy.
    pub merge: Merge,
    /// The default: a client function without parameters that the generated
    /// client calls for a `make` without this field (D116).
    pub default: Option<FuncId>,
    /// The declaration line.
    pub span: Span,
}

/// Who may write a field (SYNTAX 4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldMode {
    /// Written under the `edit` rules.
    Plain,
    /// Set by `make` only.
    Fixed,
    /// Written by server code only.
    Server,
    /// `serial [per f]`, assigned by the server; `f` is a `fixed` field or
    /// the implicit `author` (D136).
    Serial(Option<FieldRef>),
}

/// Merge strategy of a field (ARCHITECTURE 5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Merge {
    /// The default merge of the field type.
    Default,
    /// `merge text`, the sequence CRDT.
    Text,
}

/// The operations a predicate rule guards. `see` is not here: it is a filter
/// ([`Model::see`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuleOp {
    /// `make` on a model.
    Make,
    /// `edit` on a model or a field.
    Edit,
    /// `drop` on a model.
    Drop,
    /// `call` on a server function.
    Call,
}

/// A predicate rule (ARCHITECTURE 9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The guarded operation.
    pub op: RuleOp,
    /// The field of a field rule; `None` for a record rule and for `call`.
    pub field: Option<FieldId>,
    /// The predicate, a [`FnKind::Predicate`] function with a `Bool` result.
    /// For `call` its parameters are those of the guarded function, after
    /// the model arguments were resolved; other rules have none and read
    /// their row with [`InstKind::RuleField`] and [`InstKind::RuleDelta`].
    pub pred: FuncId,
    /// The directions a field rule on a `Set` or `Map` field admits; `Some`
    /// exactly for such a rule (D187). Every rule evaluator checks it before
    /// the predicate: an op of a direction it does not admit is `Denied`
    /// without running [`Rule::pred`].
    pub deltas: Option<Deltas>,
    /// The rule line.
    pub span: Span,
}

/// The directions of a field rule on a `Set` or `Map` field: those its
/// condition mentions by `added` or `removed` (D187, G12). A direction the
/// condition does not mention is denied before the predicate runs, so no
/// other atom (`signed`, `me`, a constant) can admit it. A `Map` value change
/// is a removal and then an addition and needs both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Deltas {
    /// The condition mentions `added`.
    pub added: bool,
    /// The condition mentions `removed`.
    pub removed: bool,
}

impl Deltas {
    /// True if an op of direction `dir` may reach the predicate; false means
    /// `Denied` without evaluating it.
    pub fn admits(self, dir: Delta) -> bool {
        match dir {
            Delta::Added => self.added,
            Delta::Removed => self.removed,
        }
    }
}

/// A top level `var`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Var {
    /// The name.
    pub name: String,
    /// Its type.
    pub ty: Ty,
    /// The initial value: a client or shared function without parameters.
    pub init: Option<FuncId>,
    /// The declaration.
    pub span: Span,
}

/// Where a function comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FnKind {
    /// A `fn` of the source (D95: one node for every function head).
    Source,
    /// A rule or `check` predicate. Pure, [`Placement::Shared`]: the server
    /// evaluates it as the authority, the client as a hint (ARCHITECTURE 9).
    /// Any runtime error in it denies.
    Predicate,
    /// Made by the lowering: a view thunk, a handler, a field default, a
    /// `var` initialiser, the row function of a bulk `edit`.
    Derived,
}

/// One function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    /// The source name, or a name derived from it.
    pub name: String,
    /// Where the function runs.
    pub placement: Placement,
    /// Where the function comes from.
    pub kind: FnKind,
    /// True inside `as system` code: an elevated source function and every
    /// function the lowering derives from its body. Only these may use
    /// [`Auth::System`] and [`query::See::System`].
    pub elevated: bool,
    /// Number of parameters; they occupy locals `0..params`.
    pub params: u32,
    /// Type of every local, parameters first.
    pub locals: Vec<Ty>,
    /// The result type.
    pub result: Ty,
    /// What the VM runs before block 0 on every entry; present exactly for
    /// the server source functions.
    pub prologue: Option<Prologue>,
    /// Basic blocks; block 0 is the entry.
    pub blocks: Vec<Block>,
    /// The declaration.
    pub span: Span,
}

impl Function {
    /// Type of a local.
    pub fn local_ty(&self, local: Local) -> Option<&Ty> {
        self.locals.get(local.index())
    }

    /// A block by id.
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.get(id.index())
    }
}

/// The entry of a server function (ARCHITECTURE 5.7, D98, D99, D117). The VM
/// runs it on every entry, from a client `Call` and from a server call alike,
/// always in this order:
/// 1. every parameter in `resolve` (a model reference) is read through the
///    caller's `see`; an unreadable row fails the call with `NotFound`, the
///    same kind and text as a missing row;
/// 2. on an entry from a client, and on every entry of an elevated function
///    (D98), `call_rule` is evaluated on the resolved arguments with the `me`
///    of the original request; a missing rule, false or any evaluation error
///    fails with `Denied`;
/// 3. for an elevated function, the audit entry is written; a failure fails
///    with `Unavailable` and the body does not run (D134);
/// 4. block 0 runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prologue {
    /// The model reference parameters, in parameter order.
    pub resolve: Vec<Local>,
    /// The `call` rule; `None` denies every call from a client (default
    /// deny, AC-32). An elevated function always has one (D34).
    pub call_rule: Option<RuleId>,
}

/// A basic block: straight line instructions, then exactly one terminator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Instructions in execution order.
    pub insts: Vec<Inst>,
    /// How control leaves the block.
    pub term: Terminator,
    /// The position a step of the terminator reports.
    pub term_span: Span,
}
