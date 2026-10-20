//! Instructions, operators, terminators and the step cost of every
//! instruction (ARCHITECTURE 3.4, D94, D112, D114).

use super::query::QueryPlan;
use super::{BlockId, FieldId, FieldRef, FuncId, Local, ModelId};

/// One instruction with its source location (reported by runtime errors).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inst {
    /// What the instruction does.
    pub kind: InstKind,
    /// The position a runtime error of this instruction reports.
    pub span: ostrel_core::Span,
}

/// Who a database read or write runs as (ARCHITECTURE 5.7, 9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Auth {
    /// The rules of the calling principal: a write passes the same rule checks
    /// as a sync op (ARCHITECTURE 5.5 step 8, 9), on the client as the
    /// optimistic hint, on the server as the authority. Any evaluation error
    /// denies.
    Caller,
    /// `as system`: no rule is evaluated (G13, D34). Only in an elevated
    /// server function.
    System,
    /// Inside a rule or `check` predicate: reads the stored state as the
    /// validator sees it, without `see`; a hop to a row that does not exist
    /// is an evaluation error and denies (ARCHITECTURE 5.5 step 8, 9). Never
    /// on a write: predicates are pure.
    Rule,
}

/// The error kinds of a client call to a server function (G11, D81, D134),
/// the `CallErrorKind` names of `runtime/js/api.d.ts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CallErrorKind {
    /// A rule denied the call or a write inside it.
    Denied,
    /// A model argument does not exist or is not readable (D99).
    NotFound,
    /// A limit of ARCHITECTURE 5.9 was reached.
    Limit,
    /// An argument does not fit its type.
    Invalid,
    /// The `Call` frame never reached the WebSocket (SPEC 12.7).
    Offline,
    /// The frame was sent and no `Return` arrived (D81).
    OutcomeUnknown,
    /// The server could not start the body; a retry is safe (D134).
    Unavailable,
}

impl CallErrorKind {
    /// Every kind, in declaration order.
    pub const ALL: [CallErrorKind; 7] = [
        CallErrorKind::Denied,
        CallErrorKind::NotFound,
        CallErrorKind::Limit,
        CallErrorKind::Invalid,
        CallErrorKind::Offline,
        CallErrorKind::OutcomeUnknown,
        CallErrorKind::Unavailable,
    ];

    /// The name a program writes after `catch`.
    pub fn name(self) -> &'static str {
        match self {
            CallErrorKind::Denied => "Denied",
            CallErrorKind::NotFound => "NotFound",
            CallErrorKind::Limit => "Limit",
            CallErrorKind::Invalid => "Invalid",
            CallErrorKind::Offline => "Offline",
            CallErrorKind::OutcomeUnknown => "OutcomeUnknown",
            CallErrorKind::Unavailable => "Unavailable",
        }
    }

    /// The kind with this name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// The kinds a [`InstKind::Settle`] turns into its fallback. `??` alone has
/// exactly `Offline`; `catch K, ... ??` adds the named kinds (G11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caught(Vec<CallErrorKind>);

impl Caught {
    /// The kinds of `e catch kinds ?? f`: `Offline` plus `kinds`, sorted,
    /// without repeats.
    pub fn new(kinds: impl IntoIterator<Item = CallErrorKind>) -> Self {
        let mut all: Vec<CallErrorKind> = kinds.into_iter().collect();
        all.push(CallErrorKind::Offline);
        all.sort_unstable();
        all.dedup();
        Caught(all)
    }

    /// True when a failure of this kind gives the fallback.
    pub fn contains(&self, kind: CallErrorKind) -> bool {
        self.0.contains(&kind)
    }

    /// The kinds in declaration order.
    pub fn kinds(&self) -> &[CallErrorKind] {
        &self.0
    }
}

/// The instructions. Operand types are fixed by the checker; [`super::Program::validate`]
/// checks the placement rules of every kind.
///
/// Cost: every kind has a step cost of `1 + ceil(B / 64) + 2 * A`
/// (ARCHITECTURE 3.4), given by [`InstKind::cost`]. A kind whose work grows
/// with the size of an operand names its B there; a new kind defines its cost
/// in the same commit (D94).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InstKind {
    // Values.
    /// `dst = value`.
    Const { dst: Local, value: Const },
    /// `dst = src`, both of the same type.
    Copy { dst: Local, src: Local },
    /// `dst = op arg`.
    Unary { dst: Local, op: UnOp, arg: Local },
    /// `dst = lhs op rhs`. For `Eq` and `Ne` both operands have the same type;
    /// on references they compare ids, on values they compare structurally.
    Binary {
        dst: Local,
        op: BinOp,
        lhs: Local,
        rhs: Local,
    },
    /// `dst: Text` is the text form of `src`.
    ToText { dst: Local, src: Local },
    /// `dst: Text` is the concatenation of `parts`, all `Text`.
    Concat { dst: Local, parts: Vec<Local> },
    /// `dst: T?` holds the value of `src: T`.
    Some { dst: Local, src: Local },
    /// `dst: Bool` is true when `src: T?` holds a value.
    IsSome { dst: Local, src: Local },
    /// `dst: T` is the value of `src: T?`. Only where [`InstKind::IsSome`]
    /// was true on every path; the checker guarantees it (`if let`, `??`).
    Unwrap { dst: Local, src: Local },
    /// `dst: Set[T]`, `List[T]` with the elements in order. Repeated set
    /// elements count once.
    Collect { dst: Local, elems: Vec<Local> },
    /// `dst: Map[K, V]` with the entries in order; a later entry wins.
    CollectMap {
        dst: Local,
        entries: Vec<(Local, Local)>,
    },
    /// `dst: Range[T]` is `lo..hi`, inclusive; `lo > hi` is the empty range
    /// (SPEC 12.11).
    Range { dst: Local, lo: Local, hi: Local },
    /// `dst: Bool` is `elem in coll`: element of a `Set`, `List` or `Range`,
    /// key of a `Map`. A `none` element is never contained.
    Contains {
        dst: Local,
        elem: Local,
        coll: Local,
    },
    /// `dst: V?` is `map[key]`; `dst: T?` is `list[index]`, `none` outside
    /// the list.
    Index {
        dst: Local,
        base: Local,
        index: Local,
    },
    /// A library function of `std/core` (`text.len`, `draft.trim`, ...).
    Std {
        dst: Option<Local>,
        func: StdFn,
        args: Vec<Local>,
    },
    /// `print(arg)`: writes the text form of `arg` plus a newline through the
    /// host. Script programs only.
    Print { arg: Local },

    // Context.
    /// `dst: User?` is `me`, the user row of the principal (D98: the `me` of
    /// the original request also inside nested elevated calls).
    Me { dst: Local },
    /// `dst: Bool` is `signed`.
    Signed { dst: Local },
    /// `dst: Time` is `now`, from the host clock.
    Now { dst: Local },
    /// Inside a rule or `check` predicate: `dst` is a field of the row the
    /// rule is evaluated on, as stored or as proposed (SYNTAX 4.4). The
    /// validator supplies the row; the predicate never reads it from the
    /// database itself.
    RuleField {
        dst: Local,
        state: RowState,
        field: FieldRef,
    },
    /// Inside a field rule on a `Set` or `Map` field: `dst` is `added` or
    /// `removed`, one element (or one entry) and the other direction empty
    /// (G12, D97).
    RuleDelta { dst: Local, dir: Delta },

    // Calls.
    /// Call of a function of the same side, or of a shared function. A call
    /// of a server function from server code runs the callee's
    /// [`super::Prologue`] first (D98).
    Call {
        dst: Option<Local>,
        func: FuncId,
        args: Vec<Local>,
    },
    /// Client only: sends `Call` for the server function `func` and returns
    /// at once with `dst: Remote T` (G11, ARCHITECTURE 4.3). Every argument
    /// has a wire type. Without `dst` (a call in a handler), a failure goes to
    /// the std error notice.
    RemoteCall {
        dst: Option<Local>,
        func: FuncId,
        args: Vec<Local>,
    },
    /// Client only: waits for `remote: Remote T`. `dst: T` is the result, or
    /// the value of `fallback` when the call failed with a kind in `caught`.
    /// Any other failure fails the running handler with that kind (G11).
    Settle {
        dst: Local,
        remote: Local,
        caught: Caught,
        fallback: Local,
    },

    // Rows.
    /// `dst` is the field of the row `row: Model`, read as `auth`: on the
    /// server a row that does not exist or that `see` hides fails the running
    /// call with `NotFound` (D99), on the client a row missing from the
    /// replica gives `none` for an optional field and the same failure
    /// otherwise.
    GetField {
        dst: Local,
        row: Local,
        model: ModelId,
        field: FieldRef,
        auth: Auth,
    },
    /// `make Model { ... }`: `dst: Model` is the new row. `fields` names
    /// every declared field except `serial` fields exactly once, in
    /// [`FieldId`] order; defaults are already evaluated into locals, the
    /// server never evaluates one (D116).
    Make {
        dst: Option<Local>,
        model: ModelId,
        fields: Vec<(FieldId, Local)>,
        auth: Auth,
    },
    /// `row.field = value` on a field without a collection type (also `Rank`).
    SetField {
        row: Local,
        model: ModelId,
        field: FieldId,
        value: Local,
        auth: Auth,
    },
    /// Adds one element to a `Set` field (G12).
    SetAdd {
        row: Local,
        model: ModelId,
        field: FieldId,
        elem: Local,
        auth: Auth,
    },
    /// Removes one element of a `Set` field, naming every tag of it the
    /// writer observed (G12, D96).
    SetRemove {
        row: Local,
        model: ModelId,
        field: FieldId,
        elem: Local,
        auth: Auth,
    },
    /// Sets one key of a `Map` field. Under [`Auth::Caller`], a key that is
    /// present in the server state is checked twice, first as the removal of
    /// the old entry and then as the addition of the new one; both must admit
    /// it (D97).
    MapPut {
        row: Local,
        model: ModelId,
        field: FieldId,
        key: Local,
        value: Local,
        auth: Auth,
    },
    /// Removes one key of a `Map` field.
    MapRemove {
        row: Local,
        model: ModelId,
        field: FieldId,
        key: Local,
        auth: Auth,
    },
    /// `drop row`.
    Drop {
        row: Local,
        model: ModelId,
        auth: Auth,
    },
    /// `dst: Rows(model)` is a snapshot of the query, `see` included
    /// (ARCHITECTURE 9). Rows enter the VM one at a time through
    /// [`Terminator::Next`].
    Query { dst: Local, plan: QueryPlan },
    /// Server only: `drop Query` as one bulk operation (D100): one
    /// transaction; more than 1 000 matched rows fail with `Limit` before any
    /// write; the `drop` rule per row under `auth`; any denied row rolls back
    /// and fails with `Denied`; a version conflict rolls back and restarts the
    /// whole operation, at most 3 attempts, then `Limit`. Nothing changes on
    /// any failure.
    BulkDrop { plan: QueryPlan, auth: Auth },
    /// Server only: a bulk `edit` over a query (D100), with the transaction,
    /// limits and outcomes of [`InstKind::BulkDrop`]. For each matched row,
    /// in query order, the server function `row_fn` is called with the row
    /// followed by `args`; its writes to that row run inside the bulk
    /// transaction with `expect_version`. A restart runs the query and every
    /// `row_fn` call again.
    BulkEdit {
        plan: QueryPlan,
        row_fn: FuncId,
        args: Vec<Local>,
        auth: Auth,
    },

    // Loops.
    /// `dst: Iter(T)` starts a `for` loop over `src`: the rows of a `Rows`
    /// snapshot in query order, the elements of a `Set` or `List`, the values
    /// of an `Int` range. [`Terminator::Next`] advances it.
    IterStart { dst: Local, src: Local },

    // Client state.
    /// Client only: `dst` is the value of the top level `var`.
    LoadVar { dst: Local, var: super::VarId },
    /// Client only: assigns the top level `var`; views that read it update.
    StoreVar { var: super::VarId, src: Local },
}

/// The state of the row a rule reads (SYNTAX 4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowState {
    /// The stored state: bare field names in `see`, `edit`, `drop`.
    Stored,
    /// The proposed state: `new.f` in `edit`, bare names in `make` and in
    /// `check`.
    Proposed,
}

/// The two directions of a collection field rule (G12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Delta {
    /// `added`.
    Added,
    /// `removed`.
    Removed,
}

/// Library functions with a fixed meaning. Grows by addition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StdFn {
    /// `Text.len`: Unicode scalar values; `Set`, `List`, `Map`: elements.
    Len,
    /// `Text.trim`: without leading and trailing white space.
    Trim,
    /// `Time.clock`: local `HH:MM`.
    Clock,
    /// `Time.date`: local date.
    Date,
}

/// Constant operand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Const {
    /// Within [`crate::INT_MIN`], [`crate::INT_MAX`] (G9).
    Int(i64),
    /// Finite, stored as its IEEE 754 bits so that the IR stays `Eq`.
    Float(u64),
    Text(String),
    Bool(bool),
    /// A variant of an enum, by position in declaration order.
    Variant(super::EnumId, u32),
    /// `none` of the destination's optional type.
    None,
}

/// Unary operators: `Neg` on `Int` and `Float`, `Not` on `Bool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnOp {
    Neg,
    Not,
}

/// Binary operators. Arithmetic takes two `Int` or two `Float`, and `Time`
/// plus or minus `Int`; ordering takes two operands of one ordered type; `Eq`
/// and `Ne` take two operands of the same type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// How control leaves a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminator {
    /// Continue with another block.
    Jump(BlockId),
    /// Continue with `then` if `cond` (a `Bool`) is true, else with `otherwise`.
    Branch {
        cond: Local,
        then: BlockId,
        otherwise: BlockId,
    },
    /// Advances `iter` (see [`InstKind::IterStart`]): with a next item, binds
    /// it to `bind` and continues with `body`, otherwise with `done`.
    Next {
        iter: Local,
        bind: Local,
        body: BlockId,
        done: BlockId,
    },
    /// Return from the function; `None` for a `Unit` result.
    Return(Option<Local>),
}

impl InstKind {
    /// The step cost of the instruction (ARCHITECTURE 3.4): `1 + ceil(B / 64) + 2 * A`,
    /// with B and A as returned here. The VM computes B from the operands
    /// before the instruction runs.
    pub fn cost(&self) -> Cost {
        use InstKind as K;
        use Work as W;
        let (work, allocs) = match self {
            K::Const { .. } | K::Copy { .. } | K::Unary { .. } => (W::None, 0),
            K::Binary { op, .. } => match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => (W::None, 0),
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    (W::Compare, 0)
                }
            },
            K::ToText { .. } => (W::None, 3),
            K::Concat { .. } => (W::ConcatParts, 3),
            K::Some { .. } | K::IsSome { .. } | K::Unwrap { .. } => (W::None, 0),
            K::Collect { .. } | K::CollectMap { .. } => (W::OperandBytes, 1),
            K::Range { .. } => (W::None, 1),
            K::Contains { .. } | K::Index { .. } => (W::Compare, 0),
            K::Std { .. } => (W::OperandBytes, 1),
            K::Print { .. } => (W::Printed, 0),
            K::Me { .. } | K::Signed { .. } | K::Now { .. } => (W::None, 0),
            K::RuleField { .. } | K::RuleDelta { .. } => (W::Result, 0),
            K::Call { .. } => (W::CalleeRegisters, 0),
            K::RemoteCall { .. } => (W::OperandBytes, 1),
            K::Settle { .. } => (W::Result, 0),
            K::GetField { .. } => (W::Result, 0),
            K::Make { .. }
            | K::SetField { .. }
            | K::SetAdd { .. }
            | K::SetRemove { .. }
            | K::MapPut { .. }
            | K::MapRemove { .. }
            | K::Drop { .. } => (W::OperandBytes, 1),
            K::Query { .. } => (W::None, 1),
            K::BulkDrop { .. } | K::BulkEdit { .. } => (W::PerRow, 1),
            K::LoadVar { .. } | K::StoreVar { .. } => (W::None, 0),
            K::IterStart { .. } => (W::None, 1),
        };
        Cost { work, allocs }
    }
}

impl Terminator {
    /// The step cost of the terminator, in the form of [`InstKind::cost`].
    /// [`Terminator::Next`] charges the bytes of the item that enters the VM,
    /// so a query costs in proportion to the rows it delivers.
    pub fn cost(&self) -> Cost {
        let work = match self {
            Terminator::Jump(_) | Terminator::Branch { .. } | Terminator::Return(_) => Work::None,
            Terminator::Next { .. } => Work::Result,
        };
        Cost { work, allocs: 0 }
    }
}

/// The step cost of one instruction kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cost {
    /// What B measures.
    pub work: Work,
    /// A, the heap allocations of the instruction (D112, D114).
    pub allocs: u32,
}

/// What B, the work beyond a plain instruction, measures for one kind
/// (ARCHITECTURE 3.4). "Bytes of a value" are the bytes the VM heap accounts
/// for it (the measure of `HeapLimit`), so a value that cannot be allocated
/// cannot be charged either.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Work {
    /// B = 0.
    None,
    /// Comparison: B = the bytes of the operand with fewer bytes; for `Text`
    /// equality of two texts of different length B = 0 (D115); for
    /// `Contains` and `Index` the bytes of the searched collection.
    Compare,
    /// `Concat`: the sum of the part lengths plus the register references;
    /// above `TextLimit` or the heap left only the references.
    ConcatParts,
    /// `print`: the length of the printed text.
    Printed,
    /// A call: the registers initialised for the callee; its A is 0, the
    /// calibration instruction (D114).
    CalleeRegisters,
    /// The sum of the bytes of all operand values (the values copied into a
    /// new collection, sent as call arguments, or written as an op).
    OperandBytes,
    /// The bytes of the value that enters the VM: a field read from a row, a
    /// rule input, a call result. Known before the value is converted into a
    /// VM value, so it is charged before.
    Result,
    /// A bulk operation: before each matched row, the bytes of that row, as
    /// for [`Work::Result`]; a restart charges again.
    PerRow,
}
