//! Intermediate representation and lowering for the Ostrel programming language.
//!
//! This is the v0.1 slice of the IR (freeze wave F1a, ARCHITECTURE 3.4 and 13):
//! functions made of basic blocks, each block a list of typed instructions and one
//! terminator. Values live in numbered, typed locals. The slice covers exactly the
//! v0.1 language subset: `Int`, `Text` and `Bool`, arithmetic and comparisons,
//! string interpolation, calls, `print`, `if`/`else` and `return`. Short circuit
//! `and`/`or` are lowered to branches, so they need no instruction of their own.
//!
//! The slice grows by addition only. Lowering from the checked program is added
//! in its own module.

// This crate processes data derived from untrusted input: no unwrap, expect,
// panic or unchecked indexing.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

/// Source location of an instruction, in byte offsets of one source file.
///
/// Placeholder with the field layout of `ostrel_core::Span` (ARCHITECTURE 3) until
/// that type exists; it is then replaced by a re-export of `ostrel_core::Span`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    /// Index of the source file in the source map.
    pub file: u32,
    /// Start byte offset, inclusive.
    pub start: u32,
    /// End byte offset, exclusive.
    pub end: u32,
}

/// Type of a local or of a function result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    /// No value: result type of functions without `-> T`.
    Unit,
    /// 64 bit signed integer; overflow is a runtime error.
    Int,
    /// Immutable UTF 8 text.
    Text,
    /// `true` or `false`.
    Bool,
}

/// Index of a function in [`Program::functions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FuncId(pub u32);

/// Index of a block in [`Function::blocks`]. Block 0 is the entry block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// Index of a local in [`Function::locals`]. Parameters are the first locals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Local(pub u32);

/// A whole program after lowering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// All functions; a [`FuncId`] indexes into this list.
    pub functions: Vec<Function>,
    /// The entry point `fn main()`; the checker guarantees it exists.
    pub main: FuncId,
}

impl Program {
    /// A function by id, or `None` if the id is out of range.
    pub fn function(&self, id: FuncId) -> Option<&Function> {
        self.functions.get(id.0 as usize)
    }
}

/// One function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// Source name, used in diagnostics and debugging output.
    pub name: String,
    /// Number of parameters; they occupy locals `0..params`.
    pub params: u32,
    /// Type of every local, parameters first.
    pub locals: Vec<Ty>,
    /// Result type.
    pub result: Ty,
    /// Basic blocks; block 0 is the entry.
    pub blocks: Vec<Block>,
    /// Location of the declaration.
    pub span: Span,
}

impl Function {
    /// Type of a local, or `None` if the index is out of range.
    pub fn local_ty(&self, local: Local) -> Option<Ty> {
        self.locals.get(local.0 as usize).copied()
    }

    /// A block by id, or `None` if the id is out of range.
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.get(id.0 as usize)
    }
}

/// A basic block: straight line instructions, then exactly one terminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Instructions in execution order.
    pub insts: Vec<Inst>,
    /// How control leaves the block.
    pub term: Terminator,
}

/// One instruction with its source location (reported by runtime errors).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inst {
    /// What the instruction does.
    pub kind: InstKind,
    /// Source location reported if the instruction raises a runtime error.
    pub span: Span,
}

/// Instruction kinds of the v0.1 slice. Operand types are fixed by the checker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstKind {
    /// `dst = value`.
    Const { dst: Local, value: Const },
    /// `dst = src`, both of the same type.
    Copy { dst: Local, src: Local },
    /// `dst = op arg`.
    Unary { dst: Local, op: UnOp, arg: Local },
    /// `dst = lhs op rhs`; `dst` has type [`BinOp::result_ty`].
    Binary {
        dst: Local,
        op: BinOp,
        lhs: Local,
        rhs: Local,
    },
    /// `dst: Text` is the text form of `src` (`Int`, `Text` or `Bool`), as `print`
    /// writes it.
    ToText { dst: Local, src: Local },
    /// `dst: Text` is the concatenation of `parts` (all `Text`), used for
    /// interpolation.
    Concat { dst: Local, parts: Vec<Local> },
    /// Call of `func` with `args`; `dst` is `None` for a `Unit` result.
    Call {
        dst: Option<Local>,
        func: FuncId,
        args: Vec<Local>,
    },
    /// Writes the text form of `arg` plus a newline through the host.
    Print { arg: Local },
}

/// Constant operand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Const {
    Int(i64),
    Text(String),
    Bool(bool),
}

impl Const {
    /// Type of the constant.
    pub fn ty(&self) -> Ty {
        match self {
            Const::Int(_) => Ty::Int,
            Const::Text(_) => Ty::Text,
            Const::Bool(_) => Ty::Bool,
        }
    }
}

/// Unary operators: `Neg` on `Int`, `Not` on `Bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnOp {
    Neg,
    Not,
}

/// Binary operators. Arithmetic and ordering take `Int`; `Eq` and `Ne` take two
/// operands of the same type. `Div` truncates toward zero, `Rem` takes the sign of
/// the dividend (ARCHITECTURE 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

impl BinOp {
    /// Result type of the operator.
    pub fn result_ty(self) -> Ty {
        match self {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => Ty::Int,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Ty::Bool,
        }
    }
}

/// How control leaves a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    /// Continue with another block.
    Jump(BlockId),
    /// Continue with `then` if `cond` (a `Bool`) is true, else with `otherwise`.
    Branch {
        cond: Local,
        then: BlockId,
        otherwise: BlockId,
    },
    /// Return from the function; `None` for a `Unit` result.
    Return(Option<Local>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(kind: InstKind) -> Inst {
        Inst {
            kind,
            span: Span::default(),
        }
    }

    /// `fn main() { print(7 / 2) }` written by hand.
    fn sample() -> Program {
        let main = Function {
            name: "main".to_string(),
            params: 0,
            locals: vec![Ty::Int, Ty::Int, Ty::Int],
            result: Ty::Unit,
            blocks: vec![Block {
                insts: vec![
                    inst(InstKind::Const {
                        dst: Local(0),
                        value: Const::Int(7),
                    }),
                    inst(InstKind::Const {
                        dst: Local(1),
                        value: Const::Int(2),
                    }),
                    inst(InstKind::Binary {
                        dst: Local(2),
                        op: BinOp::Div,
                        lhs: Local(0),
                        rhs: Local(1),
                    }),
                    inst(InstKind::Print { arg: Local(2) }),
                ],
                term: Terminator::Return(None),
            }],
            span: Span::default(),
        };
        Program {
            functions: vec![main],
            main: FuncId(0),
        }
    }

    #[test]
    fn lookups_are_checked() {
        let program = sample();
        assert!(program.function(FuncId(1)).is_none());
        let main = program.function(program.main);
        assert_eq!(main.map(|f| f.name.as_str()), Some("main"));
        let Some(main) = main else { return };
        assert_eq!(main.local_ty(Local(2)), Some(Ty::Int));
        assert_eq!(main.local_ty(Local(3)), None);
        assert!(main.block(BlockId(0)).is_some());
        assert!(main.block(BlockId(1)).is_none());
    }

    #[test]
    fn operator_and_constant_types() {
        assert_eq!(BinOp::Rem.result_ty(), Ty::Int);
        assert_eq!(BinOp::Le.result_ty(), Ty::Bool);
        assert_eq!(Const::Int(-1).ty(), Ty::Int);
        assert_eq!(Const::Text(String::new()).ty(), Ty::Text);
        assert_eq!(Const::Bool(true).ty(), Ty::Bool);
    }
}
