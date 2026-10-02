//! Bytecode format and the compiler from `ostrel_ir`.
//!
//! The compiler flattens the basic blocks of every function into one linear list of
//! operations and replaces block ids by operation offsets. It also validates the IR
//! completely: every index is in range and every operand has the type its operation
//! needs. The interpreter relies on that: each register always holds a value of the
//! type declared for its local, and control never leaves the operation list.

use std::fmt;

use ostrel_ir::{
    BinOp, Block, BlockId, Const, FuncId, Function, INT_MAX, INT_MIN, InstKind, Local, Program,
    Span, Terminator, Ty, UnOp,
};

/// Arithmetic on two `Int` operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arith {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// Ordering of two `Int` operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    Lt,
    Le,
    Gt,
    Ge,
}

/// One bytecode operation. Register numbers are local indexes of the current frame;
/// jump targets are offsets into the operation list of the current function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Op {
    LoadInt {
        dst: u32,
        value: i64,
    },
    LoadBool {
        dst: u32,
        value: bool,
    },
    /// Loads entry `index` of the text constant pool.
    LoadText {
        dst: u32,
        index: u32,
    },
    Copy {
        dst: u32,
        src: u32,
    },
    Neg {
        dst: u32,
        arg: u32,
    },
    Not {
        dst: u32,
        arg: u32,
    },
    Arith {
        dst: u32,
        op: Arith,
        lhs: u32,
        rhs: u32,
    },
    Order {
        dst: u32,
        op: Order,
        lhs: u32,
        rhs: u32,
    },
    /// `dst = (lhs == rhs) != negate` on two operands of the same type.
    Equal {
        dst: u32,
        lhs: u32,
        rhs: u32,
        negate: bool,
    },
    ToText {
        dst: u32,
        src: u32,
    },
    Concat {
        dst: u32,
        parts: Box<[u32]>,
    },
    Call {
        dst: Option<u32>,
        func: u32,
        args: Box<[u32]>,
    },
    Print {
        arg: u32,
    },
    Jump {
        target: u32,
    },
    Branch {
        cond: u32,
        then: u32,
        otherwise: u32,
    },
    Return {
        src: Option<u32>,
    },
}

/// Compiled code of one function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FuncCode {
    pub(crate) name: Box<str>,
    pub(crate) params: u32,
    /// Type of every register, parameters first.
    pub(crate) locals: Box<[Ty]>,
    pub(crate) ops: Box<[Op]>,
    /// Source location of every operation, same length as `ops`.
    pub(crate) spans: Box<[Span]>,
    /// Location of the declaration.
    pub(crate) span: Span,
}

/// Contents of an [`crate::Image`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Code {
    pub(crate) funcs: Box<[FuncCode]>,
    /// Text constants, referenced by `Op::LoadText`.
    pub(crate) texts: Box<[Box<str>]>,
    /// Entry function, `None` for the empty image.
    pub(crate) main: Option<u32>,
}

/// The IR given to [`crate::compile`] is malformed. The checker and the lowering
/// never produce such IR, so this error always indicates a compiler bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    /// Name of the function the problem was found in, if any.
    pub function: Option<String>,
    /// What is wrong.
    pub message: String,
    /// Location of the offending instruction or function.
    pub span: Span,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.function {
            Some(name) => write!(f, "invalid IR in function `{name}`: {}", self.message),
            None => write!(f, "invalid IR: {}", self.message),
        }
    }
}

impl std::error::Error for CompileError {}

/// Compiles a whole program. See [`crate::compile`].
pub(crate) fn compile_program(program: &Program) -> Result<Code, CompileError> {
    let program_error = |message: String| CompileError {
        function: None,
        message,
        span: Span::default(),
    };
    if u32::try_from(program.functions.len()).is_err() {
        return Err(program_error("too many functions".to_string()));
    }
    let Some(main) = program.function(program.main) else {
        return Err(program_error(format!(
            "entry function {} does not exist",
            program.main.0
        )));
    };
    if main.params != 0 || main.result != Ty::Unit {
        return Err(CompileError {
            function: Some(main.name.clone()),
            message: "the entry function must take no parameters and return no value".to_string(),
            span: main.span,
        });
    }
    let mut texts = Vec::new();
    let mut funcs = Vec::with_capacity(program.functions.len());
    for function in &program.functions {
        let mut compiler = FnCompiler {
            program,
            function,
            texts: &mut texts,
        };
        funcs.push(compiler.compile()?);
    }
    Ok(Code {
        funcs: funcs.into_boxed_slice(),
        texts: texts.into_boxed_slice(),
        main: Some(program.main.0),
    })
}

struct FnCompiler<'a> {
    program: &'a Program,
    function: &'a Function,
    texts: &'a mut Vec<Box<str>>,
}

impl FnCompiler<'_> {
    fn error(&self, span: Span, message: String) -> CompileError {
        CompileError {
            function: Some(self.function.name.clone()),
            message,
            span,
        }
    }

    fn compile(&mut self) -> Result<FuncCode, CompileError> {
        let function = self.function;
        let fspan = function.span;
        if u32::try_from(function.locals.len()).is_err() {
            return Err(self.error(fspan, "too many locals".to_string()));
        }
        if function.params as usize > function.locals.len() {
            return Err(self.error(fspan, "more parameters than locals".to_string()));
        }
        if function.blocks.is_empty() {
            return Err(self.error(fspan, "function has no blocks".to_string()));
        }
        // Offset of the first operation of every block: its instructions plus one
        // terminator each.
        let mut starts = Vec::with_capacity(function.blocks.len());
        let mut total: u32 = 0;
        for block in &function.blocks {
            starts.push(total);
            let len = u32::try_from(block.insts.len())
                .ok()
                .and_then(|n| n.checked_add(1))
                .and_then(|n| total.checked_add(n));
            match len {
                Some(next) => total = next,
                None => return Err(self.error(fspan, "function is too large".to_string())),
            }
        }
        let mut ops = Vec::with_capacity(total as usize);
        let mut spans = Vec::with_capacity(total as usize);
        for block in &function.blocks {
            self.block(block, &starts, &mut ops, &mut spans)?;
        }
        Ok(FuncCode {
            name: function.name.as_str().into(),
            params: function.params,
            locals: function.locals.clone().into_boxed_slice(),
            ops: ops.into_boxed_slice(),
            spans: spans.into_boxed_slice(),
            span: fspan,
        })
    }

    fn block(
        &mut self,
        block: &Block,
        starts: &[u32],
        ops: &mut Vec<Op>,
        spans: &mut Vec<Span>,
    ) -> Result<(), CompileError> {
        for inst in &block.insts {
            ops.push(self.inst(&inst.kind, inst.span)?);
            spans.push(inst.span);
        }
        // Terminators carry no span of their own; they report the location of the
        // last instruction of their block, or of the function for an empty block.
        let span = block
            .insts
            .last()
            .map_or(self.function.span, |inst| inst.span);
        ops.push(self.terminator(&block.term, starts, span)?);
        spans.push(span);
        Ok(())
    }

    /// Checks that `local` exists, is not `Unit`, and returns its register and type.
    fn local(&self, local: Local, span: Span) -> Result<(u32, Ty), CompileError> {
        match self.function.local_ty(local) {
            Some(Ty::Unit) => Err(self.error(
                span,
                format!("local {} of type Unit used as an operand", local.0),
            )),
            Some(ty) => Ok((local.0, ty)),
            None => Err(self.error(span, format!("local {} does not exist", local.0))),
        }
    }

    /// Checks that `local` exists and has type `want`.
    fn typed(&self, local: Local, want: Ty, span: Span) -> Result<u32, CompileError> {
        let (reg, ty) = self.local(local, span)?;
        if ty == want {
            Ok(reg)
        } else {
            Err(self.error(
                span,
                format!("local {} has type {ty:?}, expected {want:?}", local.0),
            ))
        }
    }

    fn block_target(&self, id: BlockId, starts: &[u32], span: Span) -> Result<u32, CompileError> {
        starts
            .get(id.0 as usize)
            .copied()
            .ok_or_else(|| self.error(span, format!("block {} does not exist", id.0)))
    }

    fn inst(&mut self, kind: &InstKind, span: Span) -> Result<Op, CompileError> {
        Ok(match kind {
            InstKind::Const { dst, value } => match value {
                Const::Int(v) => {
                    if !(INT_MIN..=INT_MAX).contains(v) {
                        return Err(self.error(span, format!("Int constant {v} out of range")));
                    }
                    Op::LoadInt {
                        dst: self.typed(*dst, Ty::Int, span)?,
                        value: *v,
                    }
                }
                Const::Bool(v) => Op::LoadBool {
                    dst: self.typed(*dst, Ty::Bool, span)?,
                    value: *v,
                },
                Const::Text(text) => {
                    let dst = self.typed(*dst, Ty::Text, span)?;
                    let index = u32::try_from(self.texts.len())
                        .map_err(|_| self.error(span, "too many text constants".to_string()))?;
                    self.texts.push(text.as_str().into());
                    Op::LoadText { dst, index }
                }
            },
            InstKind::Copy { dst, src } => {
                let (src, ty) = self.local(*src, span)?;
                Op::Copy {
                    dst: self.typed(*dst, ty, span)?,
                    src,
                }
            }
            InstKind::Unary { dst, op, arg } => {
                let ty = match op {
                    UnOp::Neg => Ty::Int,
                    UnOp::Not => Ty::Bool,
                };
                let dst = self.typed(*dst, ty, span)?;
                let arg = self.typed(*arg, ty, span)?;
                match op {
                    UnOp::Neg => Op::Neg { dst, arg },
                    UnOp::Not => Op::Not { dst, arg },
                }
            }
            InstKind::Binary { dst, op, lhs, rhs } => {
                let dst = self.typed(*dst, op.result_ty(), span)?;
                let arith = |op| -> Result<Op, CompileError> {
                    Ok(Op::Arith {
                        dst,
                        op,
                        lhs: self.typed(*lhs, Ty::Int, span)?,
                        rhs: self.typed(*rhs, Ty::Int, span)?,
                    })
                };
                let order = |op| -> Result<Op, CompileError> {
                    Ok(Op::Order {
                        dst,
                        op,
                        lhs: self.typed(*lhs, Ty::Int, span)?,
                        rhs: self.typed(*rhs, Ty::Int, span)?,
                    })
                };
                match op {
                    BinOp::Add => arith(Arith::Add)?,
                    BinOp::Sub => arith(Arith::Sub)?,
                    BinOp::Mul => arith(Arith::Mul)?,
                    BinOp::Div => arith(Arith::Div)?,
                    BinOp::Rem => arith(Arith::Rem)?,
                    BinOp::Lt => order(Order::Lt)?,
                    BinOp::Le => order(Order::Le)?,
                    BinOp::Gt => order(Order::Gt)?,
                    BinOp::Ge => order(Order::Ge)?,
                    BinOp::Eq | BinOp::Ne => {
                        let (lhs, ty) = self.local(*lhs, span)?;
                        Op::Equal {
                            dst,
                            lhs,
                            rhs: self.typed(*rhs, ty, span)?,
                            negate: *op == BinOp::Ne,
                        }
                    }
                }
            }
            InstKind::ToText { dst, src } => Op::ToText {
                dst: self.typed(*dst, Ty::Text, span)?,
                src: self.local(*src, span)?.0,
            },
            InstKind::Concat { dst, parts } => Op::Concat {
                dst: self.typed(*dst, Ty::Text, span)?,
                parts: parts
                    .iter()
                    .map(|part| self.typed(*part, Ty::Text, span))
                    .collect::<Result<_, _>>()?,
            },
            InstKind::Call { dst, func, args } => self.call(*dst, *func, args, span)?,
            InstKind::Print { arg } => Op::Print {
                arg: self.local(*arg, span)?.0,
            },
        })
    }

    fn call(
        &self,
        dst: Option<Local>,
        func: FuncId,
        args: &[Local],
        span: Span,
    ) -> Result<Op, CompileError> {
        let Some(callee) = self.program.function(func) else {
            return Err(self.error(span, format!("function {} does not exist", func.0)));
        };
        if args.len() != callee.params as usize {
            return Err(self.error(
                span,
                format!(
                    "call of `{}` with {} arguments, expected {}",
                    callee.name,
                    args.len(),
                    callee.params
                ),
            ));
        }
        let mut regs = Vec::with_capacity(args.len());
        for (index, arg) in args.iter().enumerate() {
            // `callee.params <= callee.locals.len()` is checked when the callee itself
            // is compiled; a missing type here is reported instead of trusted.
            let Some(want) = callee.locals.get(index).copied() else {
                return Err(self.error(span, format!("callee `{}` is malformed", callee.name)));
            };
            regs.push(self.typed(*arg, want, span)?);
        }
        let dst = match dst {
            // A result may be discarded; a `Unit` result has nothing to store.
            None => None,
            Some(_) if callee.result == Ty::Unit => {
                return Err(self.error(
                    span,
                    format!("result of `{}` is Unit and cannot be stored", callee.name),
                ));
            }
            Some(local) => Some(self.typed(local, callee.result, span)?),
        };
        Ok(Op::Call {
            dst,
            func: func.0,
            args: regs.into_boxed_slice(),
        })
    }

    fn terminator(
        &self,
        term: &Terminator,
        starts: &[u32],
        span: Span,
    ) -> Result<Op, CompileError> {
        Ok(match term {
            Terminator::Jump(target) => Op::Jump {
                target: self.block_target(*target, starts, span)?,
            },
            Terminator::Branch {
                cond,
                then,
                otherwise,
            } => Op::Branch {
                cond: self.typed(*cond, Ty::Bool, span)?,
                then: self.block_target(*then, starts, span)?,
                otherwise: self.block_target(*otherwise, starts, span)?,
            },
            Terminator::Return(value) => {
                let result = self.function.result;
                match value {
                    None if result == Ty::Unit => Op::Return { src: None },
                    None => {
                        return Err(self
                            .error(span, format!("return without a value, expected {result:?}")));
                    }
                    Some(_) if result == Ty::Unit => {
                        return Err(
                            self.error(span, "return of a value from a Unit function".to_string())
                        );
                    }
                    Some(local) => Op::Return {
                        src: Some(self.typed(*local, result, span)?),
                    },
                }
            }
        })
    }
}
