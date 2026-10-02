//! Lowering of a checked script program to the IR (WP T2-2, ARCHITECTURE 3.4).
//!
//! [`lower`] takes the syntax tree and the side tables of `ostrel_sema::check`
//! and builds one IR function per `fn`. Call it only when the checker reported
//! no error: the tables are complete only then. A tree or table that does not
//! fit together gives [`LowerError`] instead of a panic.
//!
//! Mapping:
//! * Function `i` of the module is [`FuncId`] `i`; the checker's locals
//!   (parameters, then every `let`) are IR locals with the same index, and
//!   temporaries follow them.
//! * Operands are evaluated left to right, each exactly once (SPEC 12.3).
//! * `and` and `or` become a branch, so the right operand runs only when the
//!   left one does not decide the result.
//! * `if` ends the current block with a branch; both arms jump to a join block,
//!   which exists only if an arm can reach it.
//! * Statements after a `return` are unreachable and not lowered.
//! * The trailing expression of a body without branches is its result
//!   (SYNTAX 7, `FnInfo::implicit_return`).

use ostrel_core::FileId;
use ostrel_sema::{Checked, FnInfo, Res};
use ostrel_syntax::ast::{self, BinaryOp, ExprId, ExprKind, Item, Module, StmtKind, StrPart};

use crate::{
    BinOp, Block, BlockId, Const, FuncId, Function, Inst, InstKind, Local, Program, Span,
    Terminator, Ty, UnOp,
};

/// Why a program could not be lowered. Both cases mean the caller ran the
/// lowering on a program the checker did not accept, or on tables of another
/// tree; the span points at the node where the mismatch was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowerError {
    /// The checker found no `fn main()`.
    NoMain,
    /// A node or table entry is missing, has an error type or lies outside
    /// the v0.1 slice.
    Unchecked(Span),
}

/// Lowers the checked script program `module` of file `file`.
pub fn lower(module: &Module, checked: &Checked, file: FileId) -> Result<Program, LowerError> {
    let main = checked.main.ok_or(LowerError::NoMain)?;
    let mut functions = Vec::with_capacity(module.items().len());
    for (index, item) in module.items().iter().enumerate() {
        #[allow(unreachable_patterns)]
        let decl = match item {
            Item::Fn(decl) => decl,
            _ => return Err(LowerError::Unchecked(Span::default())),
        };
        let span = span_of(file, decl.range);
        let info = match checked.fns.get(index) {
            Some(Some(info)) => info,
            _ => return Err(LowerError::Unchecked(span)),
        };
        functions.push(FnLowering::new(module, checked, info, file).run(decl)?);
    }
    Ok(Program {
        functions,
        main: FuncId(main.0),
    })
}

fn span_of(file: FileId, range: ast::TextRange) -> Span {
    Span {
        file: file.raw(),
        start: range.start(),
        end: range.end(),
    }
}

fn ty_of(ty: ostrel_sema::Ty, span: Span) -> Result<Ty, LowerError> {
    match ty {
        ostrel_sema::Ty::Unit => Ok(Ty::Unit),
        ostrel_sema::Ty::Int => Ok(Ty::Int),
        ostrel_sema::Ty::Text => Ok(Ty::Text),
        ostrel_sema::Ty::Bool => Ok(Ty::Bool),
        ostrel_sema::Ty::Error => Err(LowerError::Unchecked(span)),
    }
}

/// A block under construction; `term` is set exactly once.
struct Draft {
    insts: Vec<Inst>,
    term: Option<Terminator>,
}

struct FnLowering<'a> {
    module: &'a Module,
    checked: &'a Checked,
    info: &'a FnInfo,
    file: FileId,
    locals: Vec<Ty>,
    blocks: Vec<Draft>,
    /// The block that receives instructions; `None` after a `return`.
    current: Option<BlockId>,
}

impl<'a> FnLowering<'a> {
    fn new(module: &'a Module, checked: &'a Checked, info: &'a FnInfo, file: FileId) -> Self {
        FnLowering {
            module,
            checked,
            info,
            file,
            locals: Vec::new(),
            blocks: Vec::new(),
            current: None,
        }
    }

    fn run(mut self, decl: &ast::FnDecl) -> Result<Function, LowerError> {
        let span = span_of(self.file, decl.range);
        for &ty in &self.info.locals {
            let ty = ty_of(ty, span)?;
            self.locals.push(ty);
        }
        let result = ty_of(self.info.result, span)?;
        let params =
            u32::try_from(self.info.params.len()).map_err(|_| LowerError::Unchecked(span))?;
        let entry = self.new_block();
        self.current = Some(entry);
        self.lower_block(decl.body)?;
        if self.current.is_some() {
            // Only a function without result may end without `return`.
            if result != Ty::Unit {
                return Err(LowerError::Unchecked(span));
            }
            self.terminate(Terminator::Return(None));
        }
        let mut blocks = Vec::with_capacity(self.blocks.len());
        for draft in self.blocks {
            let term = draft.term.ok_or(LowerError::Unchecked(span))?;
            blocks.push(Block {
                insts: draft.insts,
                term,
            });
        }
        Ok(Function {
            name: decl.name.text.to_string(),
            params,
            locals: self.locals,
            result,
            blocks,
            span,
        })
    }

    fn new_block(&mut self) -> BlockId {
        let id = BlockId(u32::try_from(self.blocks.len()).unwrap_or(u32::MAX));
        self.blocks.push(Draft {
            insts: Vec::new(),
            term: None,
        });
        id
    }

    fn new_local(&mut self, ty: Ty) -> Local {
        let id = Local(u32::try_from(self.locals.len()).unwrap_or(u32::MAX));
        self.locals.push(ty);
        id
    }

    fn draft(&mut self) -> Option<&mut Draft> {
        let id = self.current?;
        self.blocks.get_mut(id.0 as usize)
    }

    fn emit(&mut self, kind: InstKind, span: Span) {
        if let Some(draft) = self.draft() {
            draft.insts.push(Inst { kind, span });
        }
    }

    /// Ends the current block; code after it is unreachable until a new
    /// block is selected.
    fn terminate(&mut self, term: Terminator) {
        if let Some(draft) = self.draft() {
            draft.term = Some(term);
        }
        self.current = None;
    }

    fn span(&self, range: ast::TextRange) -> Span {
        span_of(self.file, range)
    }

    fn lower_block(&mut self, block: ast::BlockId) -> Result<(), LowerError> {
        let Some(block) = self.module.block(block) else {
            return Err(LowerError::Unchecked(Span::default()));
        };
        for &stmt in &block.stmts {
            if self.current.is_none() {
                break;
            }
            let Some(stmt) = self.module.stmt(stmt).map(|s| (stmt, s)) else {
                return Err(LowerError::Unchecked(Span::default()));
            };
            self.lower_stmt(stmt.0, stmt.1)?;
        }
        Ok(())
    }

    fn lower_stmt(&mut self, id: ast::StmtId, stmt: &ast::Stmt) -> Result<(), LowerError> {
        let span = self.span(stmt.range);
        #[allow(unreachable_patterns)]
        match stmt.kind {
            StmtKind::Let { value, .. } => {
                let dst = self
                    .checked
                    .lets
                    .get(&id)
                    .ok_or(LowerError::Unchecked(span))?;
                let dst = Local(dst.0);
                let src = self.value(value)?;
                self.emit(InstKind::Copy { dst, src }, span);
            }
            StmtKind::If {
                cond,
                then_block,
                else_block,
            } => self.lower_if(cond, then_block, else_block)?,
            StmtKind::Return { value } => {
                let value = self.value(value)?;
                self.terminate(Terminator::Return(Some(value)));
            }
            StmtKind::Expr(expr) if self.info.implicit_return == Some(expr) => {
                let value = self.value(expr)?;
                self.terminate(Terminator::Return(Some(value)));
            }
            StmtKind::Expr(expr) => {
                self.expr(expr)?;
            }
            _ => return Err(LowerError::Unchecked(span)),
        }
        Ok(())
    }

    fn lower_if(
        &mut self,
        cond: ExprId,
        then_block: ast::BlockId,
        else_block: Option<ast::BlockId>,
    ) -> Result<(), LowerError> {
        let cond = self.value(cond)?;
        let then = self.new_block();
        let otherwise = self.new_block();
        self.terminate(Terminator::Branch {
            cond,
            then,
            otherwise,
        });
        self.current = Some(then);
        self.lower_block(then_block)?;
        let then_end = self.current;
        self.current = Some(otherwise);
        if let Some(else_block) = else_block {
            self.lower_block(else_block)?;
        }
        let else_end = self.current;
        if then_end.is_none() && else_end.is_none() {
            return Ok(());
        }
        let join = self.new_block();
        for end in [then_end, else_end].into_iter().flatten() {
            self.current = Some(end);
            self.terminate(Terminator::Jump(join));
        }
        self.current = Some(join);
        Ok(())
    }

    /// Lowers an expression that must produce a value.
    fn value(&mut self, id: ExprId) -> Result<Local, LowerError> {
        let span = self.expr_span(id);
        self.expr(id)?.ok_or(LowerError::Unchecked(span))
    }

    fn expr_span(&self, id: ExprId) -> Span {
        self.module
            .expr(id)
            .map_or(Span::default(), |e| self.span(e.range))
    }

    /// Lowers an expression; `None` for a call without result.
    fn expr(&mut self, id: ExprId) -> Result<Option<Local>, LowerError> {
        let Some(expr) = self.module.expr(id) else {
            return Err(LowerError::Unchecked(Span::default()));
        };
        let span = self.span(expr.range);
        let unchecked = LowerError::Unchecked(span);
        #[allow(unreachable_patterns)]
        let local = match &expr.kind {
            &ExprKind::Int(value) => self.constant(Const::Int(value), span),
            &ExprKind::Bool(value) => self.constant(Const::Bool(value), span),
            ExprKind::Str(parts) => self.string(parts, span)?,
            ExprKind::Name(_) => match self.checked.names.get(&id) {
                Some(&Res::Local(local)) => Local(local.0),
                _ => return Err(unchecked),
            },
            &ExprKind::Unary { op, operand } => {
                let arg = self.value(operand)?;
                let op = match op {
                    ast::UnaryOp::Neg => UnOp::Neg,
                    ast::UnaryOp::Not => UnOp::Not,
                    #[allow(unreachable_patterns)]
                    _ => return Err(unchecked),
                };
                let dst = self.new_local(if op == UnOp::Neg { Ty::Int } else { Ty::Bool });
                self.emit(InstKind::Unary { dst, op, arg }, span);
                dst
            }
            &ExprKind::Binary { op, lhs, rhs } => self.binary(op, lhs, rhs, span)?,
            ExprKind::Call { callee, args } => return self.call(*callee, args, span),
            &ExprKind::Paren(inner) => return self.expr(inner),
            _ => return Err(unchecked),
        };
        Ok(Some(local))
    }

    fn constant(&mut self, value: Const, span: Span) -> Local {
        let dst = self.new_local(value.ty());
        self.emit(InstKind::Const { dst, value }, span);
        dst
    }

    fn string(&mut self, parts: &[StrPart], span: Span) -> Result<Local, LowerError> {
        if parts.iter().all(|p| matches!(p, StrPart::Text(_))) {
            let text: String = parts
                .iter()
                .filter_map(|p| match p {
                    StrPart::Text(t) => Some(&**t),
                    _ => None,
                })
                .collect();
            return Ok(self.constant(Const::Text(text), span));
        }
        let mut locals = Vec::with_capacity(parts.len());
        for part in parts {
            #[allow(unreachable_patterns)]
            let local = match part {
                StrPart::Text(text) => self.constant(Const::Text(text.to_string()), span),
                &StrPart::Interp(expr) => {
                    let src = self.value(expr)?;
                    if self.locals.get(src.0 as usize) == Some(&Ty::Text) {
                        src
                    } else {
                        let dst = self.new_local(Ty::Text);
                        let at = self.expr_span(expr);
                        self.emit(InstKind::ToText { dst, src }, at);
                        dst
                    }
                }
                _ => return Err(LowerError::Unchecked(span)),
            };
            locals.push(local);
        }
        let dst = self.new_local(Ty::Text);
        self.emit(InstKind::Concat { dst, parts: locals }, span);
        Ok(dst)
    }

    fn binary(
        &mut self,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
        span: Span,
    ) -> Result<Local, LowerError> {
        let op = match op {
            BinaryOp::And => return self.short_circuit(true, lhs, rhs, span),
            BinaryOp::Or => return self.short_circuit(false, lhs, rhs, span),
            BinaryOp::Add => BinOp::Add,
            BinaryOp::Sub => BinOp::Sub,
            BinaryOp::Mul => BinOp::Mul,
            BinaryOp::Div => BinOp::Div,
            BinaryOp::Rem => BinOp::Rem,
            BinaryOp::Eq => BinOp::Eq,
            BinaryOp::Ne => BinOp::Ne,
            BinaryOp::Lt => BinOp::Lt,
            BinaryOp::Le => BinOp::Le,
            BinaryOp::Gt => BinOp::Gt,
            BinaryOp::Ge => BinOp::Ge,
            #[allow(unreachable_patterns)]
            _ => return Err(LowerError::Unchecked(span)),
        };
        let lhs = self.value(lhs)?;
        let rhs = self.value(rhs)?;
        let dst = self.new_local(op.result_ty());
        self.emit(InstKind::Binary { dst, op, lhs, rhs }, span);
        Ok(dst)
    }

    /// `lhs and rhs` (`is_and`) or `lhs or rhs`: the result is the left value
    /// unless it does not decide, then the right one.
    fn short_circuit(
        &mut self,
        is_and: bool,
        lhs: ExprId,
        rhs: ExprId,
        span: Span,
    ) -> Result<Local, LowerError> {
        let left = self.value(lhs)?;
        let dst = self.new_local(Ty::Bool);
        self.emit(InstKind::Copy { dst, src: left }, span);
        let right_block = self.new_block();
        let join = self.new_block();
        let (then, otherwise) = if is_and {
            (right_block, join)
        } else {
            (join, right_block)
        };
        self.terminate(Terminator::Branch {
            cond: left,
            then,
            otherwise,
        });
        self.current = Some(right_block);
        let right = self.value(rhs)?;
        self.emit(InstKind::Copy { dst, src: right }, span);
        self.terminate(Terminator::Jump(join));
        self.current = Some(join);
        Ok(dst)
    }

    fn call(
        &mut self,
        callee: ExprId,
        args: &[ExprId],
        span: Span,
    ) -> Result<Option<Local>, LowerError> {
        let unchecked = LowerError::Unchecked(span);
        let target = *self.checked.names.get(&callee).ok_or(unchecked)?;
        let mut values = Vec::with_capacity(args.len());
        for &arg in args {
            values.push(self.value(arg)?);
        }
        match target {
            Res::Print => {
                let [arg] = *values.as_slice() else {
                    return Err(unchecked);
                };
                self.emit(InstKind::Print { arg }, span);
                Ok(None)
            }
            Res::Fn(func) => {
                let info = match self.checked.fns.get(func.0 as usize) {
                    Some(Some(info)) => info,
                    _ => return Err(unchecked),
                };
                let dst = match ty_of(info.result, span)? {
                    Ty::Unit => None,
                    ty => Some(self.new_local(ty)),
                };
                let func = FuncId(func.0);
                self.emit(
                    InstKind::Call {
                        dst,
                        func,
                        args: values,
                    },
                    span,
                );
                Ok(dst)
            }
            Res::Local(_) => Err(unchecked),
        }
    }
}

#[cfg(test)]
mod tests;
