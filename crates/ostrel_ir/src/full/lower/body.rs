//! Function bodies, predicates and thunks: statements and expressions of the
//! HIR as basic blocks of the full IR.
//!
//! Every value lives in a local of its own; `let` copies, so a later
//! assignment of the source never changes it. `and`, `or`, `??` and `if let`
//! branch, so their right side runs only when its value is needed. Code
//! after a `return` is not reachable and not lowered.

use std::collections::HashMap;

use ostrel_core::Span;
use ostrel_sema::hir;
use ostrel_syntax::ast::{BinaryOp, TakeKind, UnaryOp};

use super::super::query::{Operand, QueryPlan, See, Take};
use super::super::{
    Auth, BinOp, Block, BlockId, CallErrorKind, Caught, Const, Delta, FieldId, FnKind, Function,
    Inst, InstKind, Local, ModelId, Placement, RowState, StdFn, Terminator, Ty, UnOp,
};
use super::filter::{self, Operands};
use super::{Cx, LowerError, Lowered, field_ref};

/// What a predicate reads besides its parameters.
#[derive(Clone, Debug)]
pub(in crate::full) struct RuleCx {
    /// The model whose row the rule reads; `None` for a `call` rule.
    model: Option<hir::DefId>,
    /// The state bare field names read.
    state: RowState,
    /// The type of `added` and `removed` in a field rule on a `Set` or `Map`.
    delta: Option<Ty>,
}

impl RuleCx {
    /// A `call` rule: it reads only the parameters of its function.
    pub(in crate::full) fn call() -> Self {
        RuleCx {
            model: None,
            state: RowState::Stored,
            delta: None,
        }
    }

    /// A rule or `check` of `model`.
    pub(in crate::full) fn row(model: hir::DefId, state: RowState, delta: Option<Ty>) -> Self {
        RuleCx {
            model: Some(model),
            state,
            delta,
        }
    }
}

/// A block under construction.
#[derive(Default)]
struct Draft {
    insts: Vec<Inst>,
    term: Option<(Terminator, Span)>,
}

/// Builds one function.
pub(in crate::full) struct Fb<'c, 'h> {
    cx: &'c Cx<'h>,
    name: String,
    placement: Placement,
    kind: FnKind,
    elevated: bool,
    span: Span,
    params: u32,
    locals: Vec<Ty>,
    map: HashMap<hir::LocalId, Local>,
    blocks: Vec<Draft>,
    /// The block being filled; `None` after a terminator, where code is not
    /// reachable.
    cur: Option<usize>,
    rule: Option<RuleCx>,
    /// A predicate that mentions `me` returns `false` at its entry without
    /// a principal; after that guard `me` is present.
    me_guarded: bool,
    result: Ty,
}

impl<'c, 'h> Fb<'c, 'h> {
    pub(in crate::full) fn new(
        cx: &'c Cx<'h>,
        name: String,
        placement: Placement,
        kind: FnKind,
        elevated: bool,
        span: Span,
    ) -> Self {
        Fb {
            cx,
            name,
            placement,
            kind,
            elevated,
            span,
            params: 0,
            locals: Vec::new(),
            map: HashMap::new(),
            blocks: vec![Draft::default()],
            cur: Some(0),
            rule: None,
            me_guarded: false,
            result: Ty::Unit,
        }
    }

    /// Binds the parameters, in order, to the first locals.
    pub(in crate::full) fn params(&mut self, params: &[hir::LocalId]) -> Lowered<()> {
        for &param in params {
            let ty = self.cx.local_ty(param, self.span)?;
            let local = self.local(ty);
            self.map.insert(param, local);
        }
        self.params = params.len() as u32;
        Ok(())
    }

    /// A function body (SYNTAX 4.10): without a branch directly in the body
    /// (D172), its last expression is its result.
    pub(in crate::full) fn body(mut self, body: hir::BlockId, result: &Ty) -> Lowered<Function> {
        self.result = result.clone();
        let stmts = self
            .cx
            .hir
            .block(body)
            .ok_or(LowerError::Unchecked(self.span))?
            .stmts
            .clone();
        let branches = stmts.iter().any(|&s| {
            matches!(
                self.cx.hir.stmt(s).map(|st| &st.kind),
                Some(hir::StmtKind::If { .. } | hir::StmtKind::IfLet { .. })
            )
        });
        let last_value = match stmts.last().and_then(|&s| self.cx.hir.stmt(s)) {
            Some(hir::Stmt {
                kind: hir::StmtKind::Expr(e),
                span,
            }) if !branches && *result != Ty::Unit => Some((*e, *span)),
            _ => None,
        };
        let count = stmts.len() - usize::from(last_value.is_some());
        for &s in stmts.iter().take(count) {
            if self.cur.is_none() {
                break;
            }
            self.stmt(s)?;
        }
        if self.cur.is_some() {
            match last_value {
                Some((e, at)) => {
                    let value = self.value(e, result)?;
                    self.terminate(Terminator::Return(Some(value)), at);
                }
                None if *result == Ty::Unit => {
                    self.terminate(Terminator::Return(None), self.span);
                }
                None => return Err(LowerError::Unchecked(self.span)),
            }
        }
        self.finish()
    }

    /// A predicate: a rule or `check` condition, or a `call` rule.
    pub(in crate::full) fn predicate(
        mut self,
        cond: hir::ExprId,
        rule: RuleCx,
    ) -> Lowered<Function> {
        self.rule = Some(rule);
        self.result = Ty::Bool;
        // A rule that mentions `me` is false as a whole without a principal
        // (SYNTAX 4.4): `me` is never a value there, not even `none` under
        // `!=`, `not` or `not in`.
        if filter::mentions_me(self.cx.hir, cond) {
            let at = filter::span(self.cx, cond);
            let me = self.me(at)?;
            self.deny_unless_some(me, at);
            self.me_guarded = true;
        }
        let value = self.value(cond, &Ty::Bool)?;
        self.terminate(Terminator::Return(Some(value)), filter::span(self.cx, cond));
        self.finish()
    }

    /// A function without parameters that gives the value of `e`: a field
    /// default or a `var` initialiser.
    pub(in crate::full) fn thunk(mut self, e: hir::ExprId, ty: &Ty) -> Lowered<Function> {
        self.result = ty.clone();
        let value = self.value(e, ty)?;
        self.terminate(Terminator::Return(Some(value)), filter::span(self.cx, e));
        self.finish()
    }

    fn finish(self) -> Lowered<Function> {
        let span = self.span;
        let blocks = self
            .blocks
            .into_iter()
            .map(|draft| {
                let (term, term_span) = draft.term.ok_or(LowerError::Unchecked(span))?;
                Ok(Block {
                    insts: draft.insts,
                    term,
                    term_span,
                })
            })
            .collect::<Lowered<Vec<_>>>()?;
        Ok(Function {
            name: self.name,
            placement: self.placement,
            kind: self.kind,
            elevated: self.elevated,
            params: self.params,
            locals: self.locals,
            result: self.result,
            prologue: None,
            blocks,
            span,
        })
    }

    // Building blocks.

    fn local(&mut self, ty: Ty) -> Local {
        self.locals.push(ty);
        Local(self.locals.len() as u32 - 1)
    }

    fn ty(&self, local: Local) -> Ty {
        self.locals.get(local.index()).cloned().unwrap_or(Ty::Unit)
    }

    fn emit(&mut self, kind: InstKind, span: Span) {
        if let Some(draft) = self.cur.and_then(|b| self.blocks.get_mut(b)) {
            draft.insts.push(Inst { kind, span });
        }
    }

    fn new_block(&mut self) -> usize {
        self.blocks.push(Draft::default());
        self.blocks.len() - 1
    }

    fn set_term(&mut self, block: usize, term: Terminator, span: Span) {
        if let Some(draft) = self.blocks.get_mut(block) {
            draft.term = Some((term, span));
        }
    }

    fn terminate(&mut self, term: Terminator, span: Span) {
        if let Some(b) = self.cur.take() {
            self.set_term(b, term, span);
        }
    }

    /// Ends the current block without a terminator yet; the caller jumps it
    /// to a join block, if there is one.
    fn leave(&mut self) -> Option<usize> {
        self.cur.take()
    }

    /// Continues in a new block that every block of `open` jumps to; with no
    /// open block the code after is not reachable.
    fn join(&mut self, open: Vec<usize>, span: Span) {
        if open.is_empty() {
            self.cur = None;
            return;
        }
        let join = self.new_block();
        for b in open {
            self.set_term(b, Terminator::Jump(BlockId(join as u32)), span);
        }
        self.cur = Some(join);
    }

    fn constant(&mut self, value: Const, ty: Ty, span: Span) -> Local {
        let dst = self.local(ty);
        self.emit(InstKind::Const { dst, value }, span);
        dst
    }

    fn auth(&self) -> Auth {
        if self.kind == FnKind::Predicate {
            Auth::Rule
        } else if self.elevated {
            Auth::System
        } else {
            Auth::Caller
        }
    }

    fn expr_ty(&self, e: hir::ExprId) -> Lowered<Ty> {
        self.cx
            .hir
            .expr(e)
            .and_then(|x| x.ty.as_ref())
            .and_then(|t| self.cx.ty(t))
            .ok_or(LowerError::Unchecked(filter::span(self.cx, e)))
    }

    fn kind(&self, e: hir::ExprId) -> Option<&'h hir::ExprKind> {
        self.cx.hir.expr(e).map(|x| &x.kind)
    }

    fn model(&self, def: hir::DefId, at: Span) -> Lowered<ModelId> {
        self.cx
            .models
            .get(&def)
            .copied()
            .ok_or(LowerError::Unchecked(at))
    }

    /// A value of type `want` from a value of type `have`: the same local,
    /// or the value wrapped as an optional.
    fn coerce(&mut self, local: Local, want: &Ty, span: Span) -> Lowered<Local> {
        let have = self.ty(local);
        if have == *want {
            return Ok(local);
        }
        if *want == Ty::Optional(Box::new(have)) {
            let dst = self.local(want.clone());
            self.emit(InstKind::Some { dst, src: local }, span);
            return Ok(dst);
        }
        Err(LowerError::Unchecked(span))
    }

    // Statements.

    fn block(&mut self, block: hir::BlockId) -> Lowered<()> {
        let stmts = self
            .cx
            .hir
            .block(block)
            .ok_or(LowerError::Unchecked(self.span))?
            .stmts
            .clone();
        for &s in stmts.iter() {
            if self.cur.is_none() {
                break;
            }
            self.stmt(s)?;
        }
        Ok(())
    }

    fn stmt(&mut self, s: hir::StmtId) -> Lowered<()> {
        let stmt = self
            .cx
            .hir
            .stmt(s)
            .ok_or(LowerError::Unchecked(self.span))?;
        let at = stmt.span;
        match &stmt.kind {
            hir::StmtKind::Let { local, value } => {
                let ty = self.cx.local_ty(*local, at)?;
                let src = self.value(*value, &ty)?;
                let dst = self.local(ty);
                self.emit(InstKind::Copy { dst, src }, at);
                self.map.insert(*local, dst);
            }
            hir::StmtKind::If {
                cond,
                then_block,
                else_block,
            } => {
                let cond = self.value(*cond, &Ty::Bool)?;
                let then_b = self.new_block();
                let else_b = self.new_block();
                self.terminate(
                    Terminator::Branch {
                        cond,
                        then: BlockId(then_b as u32),
                        otherwise: BlockId(else_b as u32),
                    },
                    at,
                );
                let mut open = Vec::new();
                self.cur = Some(then_b);
                self.block(*then_block)?;
                open.extend(self.leave());
                self.cur = Some(else_b);
                if let Some(b) = else_block {
                    self.block(*b)?;
                }
                open.extend(self.leave());
                self.join(open, at);
            }
            hir::StmtKind::IfLet {
                local,
                value,
                then_block,
                else_block,
            } => {
                let value = self.expr(*value)?;
                let Ty::Optional(inner) = self.ty(value) else {
                    return Err(LowerError::Unchecked(at));
                };
                let is = self.local(Ty::Bool);
                self.emit(
                    InstKind::IsSome {
                        dst: is,
                        src: value,
                    },
                    at,
                );
                let then_b = self.new_block();
                let else_b = self.new_block();
                self.terminate(
                    Terminator::Branch {
                        cond: is,
                        then: BlockId(then_b as u32),
                        otherwise: BlockId(else_b as u32),
                    },
                    at,
                );
                let mut open = Vec::new();
                self.cur = Some(then_b);
                let bound = self.local(*inner);
                self.emit(
                    InstKind::Unwrap {
                        dst: bound,
                        src: value,
                    },
                    at,
                );
                self.map.insert(*local, bound);
                self.block(*then_block)?;
                open.extend(self.leave());
                self.cur = Some(else_b);
                if let Some(b) = else_block {
                    self.block(*b)?;
                }
                open.extend(self.leave());
                self.join(open, at);
            }
            hir::StmtKind::For {
                local,
                source,
                body,
            } => self.for_loop(*local, *source, *body, at)?,
            hir::StmtKind::Return(e) => {
                if self.result == Ty::Unit {
                    self.effect(*e)?;
                    self.terminate(Terminator::Return(None), at);
                } else {
                    let want = self.result.clone();
                    let value = self.value(*e, &want)?;
                    self.terminate(Terminator::Return(Some(value)), at);
                }
            }
            hir::StmtKind::Expr(e) => self.effect(*e)?,
            hir::StmtKind::Drop(e) => match self.kind(*e) {
                Some(hir::ExprKind::Query(q)) => {
                    let plan = self.plan(q, at)?;
                    let auth = self.auth();
                    self.emit(InstKind::BulkDrop { plan, auth }, at);
                }
                _ => {
                    let row = self.expr(*e)?;
                    let Ty::Ref(model) = self.ty(row) else {
                        return Err(LowerError::Unchecked(at));
                    };
                    let auth = self.auth();
                    self.emit(InstKind::Drop { row, model, auth }, at);
                }
            },
            hir::StmtKind::Assign { target, value } => self.assign(*target, *value, at)?,
            _ => return Err(LowerError::Unsupported(at)),
        }
        Ok(())
    }

    fn for_loop(
        &mut self,
        local: hir::LocalId,
        source: hir::ExprId,
        body: hir::BlockId,
        at: Span,
    ) -> Lowered<()> {
        let src = match self.kind(source) {
            // `for s in Status`: the variants in declaration order.
            Some(hir::ExprKind::Name(hir::Res::Def(def)))
                if matches!(
                    self.cx.hir.def(*def).map(|d| &d.kind),
                    Some(hir::DefKind::Enum(_))
                ) =>
            {
                let enum_id = *self.cx.enums.get(def).ok_or(LowerError::Unchecked(at))?;
                let count = match self.cx.hir.def(*def).map(|d| &d.kind) {
                    Some(hir::DefKind::Enum(variants)) => variants.len() as u32,
                    _ => 0,
                };
                let elems = (0..count)
                    .map(|i| self.constant(Const::Variant(enum_id, i), Ty::Enum(enum_id), at))
                    .collect();
                let dst = self.local(Ty::List(Box::new(Ty::Enum(enum_id))));
                self.emit(InstKind::Collect { dst, elems }, at);
                dst
            }
            _ => self.expr(source)?,
        };
        let src_ty = self.ty(src);
        if !matches!(
            src_ty,
            Ty::Rows(_) | Ty::Set(_) | Ty::List(_) | Ty::Range(_)
        ) {
            return Err(LowerError::Unchecked(at));
        }
        let iter = self.local(Ty::Iter(Box::new(src_ty)));
        self.emit(InstKind::IterStart { dst: iter, src }, at);
        let head = self.new_block();
        self.terminate(Terminator::Jump(BlockId(head as u32)), at);
        self.cur = Some(head);
        let item = self.cx.local_ty(local, at)?;
        let bind = self.local(item);
        self.map.insert(local, bind);
        let body_b = self.new_block();
        let done = self.new_block();
        self.terminate(
            Terminator::Next {
                iter,
                bind,
                body: BlockId(body_b as u32),
                done: BlockId(done as u32),
            },
            at,
        );
        self.cur = Some(body_b);
        self.block(body)?;
        self.terminate(Terminator::Jump(BlockId(head as u32)), at);
        self.cur = Some(done);
        Ok(())
    }

    fn assign(&mut self, target: hir::ExprId, value: hir::ExprId, at: Span) -> Lowered<()> {
        let unchecked = LowerError::Unchecked(at);
        match self.kind(target).ok_or(unchecked)? {
            hir::ExprKind::Name(hir::Res::Local(l)) => {
                let dst = *self.map.get(l).ok_or(unchecked)?;
                let want = self.ty(dst);
                let src = self.value(value, &want)?;
                self.emit(InstKind::Copy { dst, src }, at);
            }
            hir::ExprKind::Name(hir::Res::Def(def)) => {
                let (var, want) = self.cx.vars.get(def).cloned().ok_or(unchecked)?;
                let src = self.value(value, &want)?;
                self.emit(InstKind::StoreVar { var, src }, at);
            }
            hir::ExprKind::Field {
                base,
                field: (model, hir::FieldRef::Declared(field)),
            } => {
                let def = self.cx.hir.field(*model, *field).ok_or(unchecked)?;
                let want = self.cx.ty(&def.ty).ok_or(unchecked)?;
                if matches!(want, Ty::Set(_) | Ty::Map(..) | Ty::List(_)) {
                    // A whole value write of a collection field has no op
                    // yet (G12: one op adds or removes one element).
                    return Err(LowerError::Unsupported(at));
                }
                let row = self.row(*base, *model, at)?;
                let value = self.value(value, &want)?;
                let model = self.model(*model, at)?;
                let auth = self.auth();
                self.emit(
                    InstKind::SetField {
                        row,
                        model,
                        field: FieldId(field.0),
                        value,
                        auth,
                    },
                    at,
                );
            }
            hir::ExprKind::Index { base, index } => {
                let Some(hir::ExprKind::Field {
                    base: row,
                    field: (model, hir::FieldRef::Declared(field)),
                }) = self.kind(*base)
                else {
                    return Err(unchecked);
                };
                let def = self.cx.hir.field(*model, *field).ok_or(unchecked)?;
                let Some(Ty::Map(k, v)) = self.cx.ty(&def.ty) else {
                    return Err(unchecked);
                };
                let row = self.row(*row, *model, at)?;
                let key = self.value(*index, &k)?;
                let value = self.value(value, &v)?;
                let model = self.model(*model, at)?;
                let auth = self.auth();
                self.emit(
                    InstKind::MapPut {
                        row,
                        model,
                        field: FieldId(field.0),
                        key,
                        value,
                        auth,
                    },
                    at,
                );
            }
            _ => return Err(unchecked),
        }
        Ok(())
    }

    /// An expression statement: a call, a write or a value that is dropped.
    fn effect(&mut self, e: hir::ExprId) -> Lowered<()> {
        let at = filter::span(self.cx, e);
        let unchecked = LowerError::Unchecked(at);
        let Some(hir::ExprKind::Call { callee, args }) = self.kind(e) else {
            self.expr(e)?;
            return Ok(());
        };
        match self.kind(*callee).ok_or(unchecked)? {
            hir::ExprKind::Name(hir::Res::Def(def)) => {
                self.call(*def, args, false, at)?;
            }
            hir::ExprKind::Name(hir::Res::Std(hir::StdName::Print)) => {
                let [arg] = &**args else {
                    return Err(unchecked);
                };
                let arg = self.expr(*arg)?;
                self.emit(InstKind::Print { arg }, at);
            }
            hir::ExprKind::Property { base, name } => {
                let Some(hir::ExprKind::Field {
                    base: row,
                    field: (model, hir::FieldRef::Declared(field)),
                }) = self.kind(*base)
                else {
                    return Err(LowerError::Unsupported(at));
                };
                let def = self.cx.hir.field(*model, *field).ok_or(unchecked)?;
                let ty = self.cx.ty(&def.ty).ok_or(unchecked)?;
                let [arg] = &**args else {
                    return Err(unchecked);
                };
                let row = self.row(*row, *model, at)?;
                let model = self.model(*model, at)?;
                let field = FieldId(field.0);
                let auth = self.auth();
                let kind = match (&ty, &**name) {
                    (Ty::Set(elem), "add" | "remove") => {
                        let elem = self.value(*arg, elem)?;
                        if &**name == "add" {
                            InstKind::SetAdd {
                                row,
                                model,
                                field,
                                elem,
                                auth,
                            }
                        } else {
                            InstKind::SetRemove {
                                row,
                                model,
                                field,
                                elem,
                                auth,
                            }
                        }
                    }
                    (Ty::Map(key, _), "remove") => {
                        let key = self.value(*arg, key)?;
                        InstKind::MapRemove {
                            row,
                            model,
                            field,
                            key,
                            auth,
                        }
                    }
                    _ => return Err(LowerError::Unsupported(at)),
                };
                self.emit(kind, at);
            }
            _ => return Err(LowerError::Unsupported(at)),
        }
        Ok(())
    }

    /// A call of a source function: a direct call, or from client code a
    /// remote call of a server function (G11). With `value`, the result is
    /// returned: `T`, or `Remote T` for a remote call.
    fn call(
        &mut self,
        def: hir::DefId,
        args: &[hir::ExprId],
        value: bool,
        at: Span,
    ) -> Lowered<Option<Local>> {
        let sig = self
            .cx
            .fns
            .get(&def)
            .cloned()
            .ok_or(LowerError::Unsupported(at))?;
        if sig.params.len() != args.len() {
            return Err(LowerError::Unchecked(at));
        }
        let args = args
            .iter()
            .zip(&sig.params)
            .map(|(&a, want)| self.value(a, want))
            .collect::<Lowered<Vec<_>>>()?;
        let remote = sig.placement == Placement::Server && self.placement == Placement::Client;
        if remote {
            let dst = value.then(|| self.local(Ty::Remote(Box::new(sig.result.clone()))));
            self.emit(
                InstKind::RemoteCall {
                    dst,
                    func: sig.id,
                    args,
                },
                at,
            );
            return Ok(dst);
        }
        let dst = (value && sig.result != Ty::Unit).then(|| self.local(sig.result.clone()));
        self.emit(
            InstKind::Call {
                dst,
                func: sig.id,
                args,
            },
            at,
        );
        Ok(dst)
    }

    // Expressions.

    /// `e` where a value of type `want` is required: `none`, `me` and
    /// collection literals take their type from `want`, other values are
    /// wrapped when `want` is their optional.
    fn value(&mut self, e: hir::ExprId, want: &Ty) -> Lowered<Local> {
        let at = filter::span(self.cx, e);
        let inner = match want {
            Ty::Optional(t) => (**t).clone(),
            t => t.clone(),
        };
        let local = match (self.kind(e), &inner) {
            (Some(hir::ExprKind::NoneValue), _) if matches!(want, Ty::Optional(_)) => {
                return Ok(self.constant(Const::None, want.clone(), at));
            }
            (Some(hir::ExprKind::Me), _) if matches!(want, Ty::Optional(_)) => self.me(at)?,
            (Some(hir::ExprKind::Set(items)), Ty::Set(elem))
            | (Some(hir::ExprKind::List(items)), Ty::List(elem)) => {
                let elems = items
                    .iter()
                    .map(|&i| self.value(i, elem))
                    .collect::<Lowered<Vec<_>>>()?;
                let dst = self.local(inner.clone());
                self.emit(InstKind::Collect { dst, elems }, at);
                dst
            }
            (Some(hir::ExprKind::Map(entries)), Ty::Map(k, v)) => {
                let entries = entries
                    .iter()
                    .map(|&(key, value)| Ok((self.value(key, k)?, self.value(value, v)?)))
                    .collect::<Lowered<Vec<_>>>()?;
                let dst = self.local(inner.clone());
                self.emit(InstKind::CollectMap { dst, entries }, at);
                dst
            }
            _ => self.expr(e)?,
        };
        self.coerce(local, want, at)
    }

    fn expr(&mut self, e: hir::ExprId) -> Lowered<Local> {
        let at = filter::span(self.cx, e);
        let unchecked = LowerError::Unchecked(at);
        let kind = self.kind(e).ok_or(unchecked)?;
        Ok(match kind {
            hir::ExprKind::Int(n) => self.constant(Const::Int(*n), Ty::Int, at),
            hir::ExprKind::Bool(b) => self.constant(Const::Bool(*b), Ty::Bool, at),
            hir::ExprKind::Str(parts) => self.text(parts, at)?,
            hir::ExprKind::Name(res) => self.name(*res, at)?,
            hir::ExprKind::Unary { op, operand } => {
                let (arg, op) = match op {
                    UnaryOp::Not => (self.value(*operand, &Ty::Bool)?, UnOp::Not),
                    UnaryOp::Neg => (self.expr(*operand)?, UnOp::Neg),
                };
                let dst = self.local(self.ty(arg));
                self.emit(InstKind::Unary { dst, op, arg }, at);
                dst
            }
            hir::ExprKind::Binary { op, lhs, rhs } => self.binary(e, *op, *lhs, *rhs, at)?,
            hir::ExprKind::Call { callee, args } => match self.kind(*callee) {
                Some(hir::ExprKind::Name(hir::Res::Def(def))) => {
                    self.call(*def, args, true, at)?.ok_or(unchecked)?
                }
                _ => return Err(LowerError::Unsupported(at)),
            },
            hir::ExprKind::Me => self.me_row(at)?,
            hir::ExprKind::Now => {
                let dst = self.local(Ty::Time);
                self.emit(InstKind::Now { dst }, at);
                dst
            }
            hir::ExprKind::Signed => {
                let dst = self.local(Ty::Bool);
                self.emit(InstKind::Signed { dst }, at);
                dst
            }
            hir::ExprKind::Field {
                base,
                field: (model, field),
            } => self.field(*base, *model, *field, at)?,
            hir::ExprKind::Property { base, name } => {
                let ty = self.expr_ty(e)?;
                let base = self.expr(*base)?;
                let func = match (self.ty(base), &**name) {
                    (Ty::Text | Ty::Set(_) | Ty::List(_) | Ty::Map(..), "len") => StdFn::Len,
                    (Ty::Text, "trim") => StdFn::Trim,
                    (Ty::Time, "clock") => StdFn::Clock,
                    (Ty::Time, "date") => StdFn::Date,
                    _ => return Err(LowerError::Unsupported(at)),
                };
                let dst = self.local(ty);
                self.emit(
                    InstKind::Std {
                        dst: Some(dst),
                        func,
                        args: vec![base],
                    },
                    at,
                );
                dst
            }
            hir::ExprKind::Index { base, index } => {
                let base = self.expr(*base)?;
                let (key, item) = match self.ty(base) {
                    Ty::Map(k, v) => (*k, *v),
                    Ty::List(t) => (Ty::Int, *t),
                    _ => return Err(unchecked),
                };
                let index = self.value(*index, &key)?;
                let dst = self.local(Ty::Optional(Box::new(item)));
                self.emit(InstKind::Index { dst, base, index }, at);
                dst
            }
            hir::ExprKind::In {
                element,
                collection,
                negated,
            } => {
                let coll = self.expr(*collection)?;
                let item = match self.ty(coll) {
                    Ty::Set(t) | Ty::List(t) | Ty::Range(t) => *t,
                    Ty::Map(k, _) => *k,
                    _ => return Err(unchecked),
                };
                // A `none` element is never contained (SYNTAX 4.4).
                let elem = match self.kind(*element) {
                    Some(hir::ExprKind::Me | hir::ExprKind::NoneValue) => {
                        self.value(*element, &Ty::Optional(Box::new(item)))?
                    }
                    _ => {
                        let elem = self.expr(*element)?;
                        if self.ty(elem) == Ty::Optional(Box::new(item.clone())) {
                            elem
                        } else {
                            self.coerce(elem, &item, at)?
                        }
                    }
                };
                let dst = self.local(Ty::Bool);
                self.emit(InstKind::Contains { dst, elem, coll }, at);
                if *negated {
                    let not = self.local(Ty::Bool);
                    self.emit(
                        InstKind::Unary {
                            dst: not,
                            op: UnOp::Not,
                            arg: dst,
                        },
                        at,
                    );
                    not
                } else {
                    dst
                }
            }
            hir::ExprKind::Range { lo, hi } => {
                let ty = self.expr_ty(e)?;
                let Ty::Range(item) = &ty else {
                    return Err(unchecked);
                };
                let lo = self.value(*lo, item)?;
                let hi = self.value(*hi, item)?;
                let dst = self.local(ty.clone());
                self.emit(InstKind::Range { dst, lo, hi }, at);
                dst
            }
            hir::ExprKind::Fallback { value, fallback } => self.fallback(*value, *fallback, at)?,
            hir::ExprKind::Set(_) | hir::ExprKind::List(_) | hir::ExprKind::Map(_) => {
                let ty = self.expr_ty(e)?;
                self.value(e, &ty)?
            }
            hir::ExprKind::Make { model, fields } => self.make(*model, fields, at)?,
            hir::ExprKind::Query(q) => {
                let plan = self.plan(q, at)?;
                let dst = self.local(Ty::Rows(plan.model));
                self.emit(InstKind::Query { dst, plan }, at);
                dst
            }
            // `none` needs the type of its place; `catch` stands only before
            // `??`; words are replaced by `check` (D142).
            hir::ExprKind::NoneValue | hir::ExprKind::Catch { .. } | hir::ExprKind::Word { .. } => {
                return Err(unchecked);
            }
            _ => return Err(LowerError::Unsupported(at)),
        })
    }

    fn text(&mut self, parts: &[hir::StrPart], at: Span) -> Lowered<Local> {
        if parts.iter().all(|p| matches!(p, hir::StrPart::Text(_))) {
            let text = parts
                .iter()
                .map(|p| match p {
                    hir::StrPart::Text(t) => &**t,
                    hir::StrPart::Interp(_) => "",
                })
                .collect();
            return Ok(self.constant(Const::Text(text), Ty::Text, at));
        }
        let mut locals = Vec::new();
        for part in parts {
            let local = match part {
                hir::StrPart::Text(t) => self.constant(Const::Text(t.to_string()), Ty::Text, at),
                hir::StrPart::Interp(e) => {
                    let value = self.expr(*e)?;
                    if self.ty(value) == Ty::Text {
                        value
                    } else {
                        let dst = self.local(Ty::Text);
                        self.emit(InstKind::ToText { dst, src: value }, at);
                        dst
                    }
                }
            };
            locals.push(local);
        }
        let dst = self.local(Ty::Text);
        self.emit(InstKind::Concat { dst, parts: locals }, at);
        Ok(dst)
    }

    fn name(&mut self, res: hir::Res, at: Span) -> Lowered<Local> {
        let unchecked = LowerError::Unchecked(at);
        Ok(match res {
            hir::Res::Local(l) => *self.map.get(&l).ok_or(unchecked)?,
            hir::Res::Field(model, field) => {
                let rule = self.rule.clone().ok_or(unchecked)?;
                if rule.model != Some(model) {
                    return Err(unchecked);
                }
                let ty = self.field_ty(model, field, at)?;
                let dst = self.local(ty);
                self.emit(
                    InstKind::RuleField {
                        dst,
                        state: rule.state,
                        field: field_ref(field),
                    },
                    at,
                );
                dst
            }
            hir::Res::Variant(def, index) => {
                let id = *self.cx.enums.get(&def).ok_or(unchecked)?;
                self.constant(Const::Variant(id, index), Ty::Enum(id), at)
            }
            hir::Res::Def(def) => {
                let (var, ty) = self
                    .cx
                    .vars
                    .get(&def)
                    .cloned()
                    .ok_or(LowerError::Unsupported(at))?;
                let dst = self.local(ty);
                self.emit(InstKind::LoadVar { dst, var }, at);
                dst
            }
            hir::Res::Context(hir::ContextName::Added | hir::ContextName::Removed) => {
                let ty = self
                    .rule
                    .as_ref()
                    .and_then(|r| r.delta.clone())
                    .ok_or(unchecked)?;
                let dir = if res == hir::Res::Context(hir::ContextName::Added) {
                    Delta::Added
                } else {
                    Delta::Removed
                };
                let dst = self.local(ty);
                self.emit(InstKind::RuleDelta { dst, dir }, at);
                dst
            }
            hir::Res::Error => return Err(unchecked),
            _ => return Err(LowerError::Unsupported(at)),
        })
    }

    /// The type a read of the field gives; `serial` reads as `Int?`.
    fn field_ty(&self, model: hir::DefId, field: hir::FieldRef, at: Span) -> Lowered<Ty> {
        let unchecked = LowerError::Unchecked(at);
        Ok(match field {
            hir::FieldRef::Declared(f) => {
                let def = self.cx.hir.field(model, f).ok_or(unchecked)?;
                let ty = self.cx.ty(&def.ty).ok_or(unchecked)?;
                if matches!(def.mode, hir::FieldMode::Serial(_)) {
                    Ty::Optional(Box::new(ty))
                } else {
                    ty
                }
            }
            hir::FieldRef::Implicit(implicit) => match implicit {
                hir::ImplicitField::Id => Ty::Ref(self.model(model, at)?),
                hir::ImplicitField::Made | hir::ImplicitField::Changed => Ty::Time,
                hir::ImplicitField::Author => Ty::Ref(self.cx.user.ok_or(unchecked)?),
                hir::ImplicitField::Pending | hir::ImplicitField::Rejected => Ty::Bool,
            },
        })
    }

    /// `base.field`; `new.field` in an `edit` rule reads the proposed row.
    fn field(
        &mut self,
        base: hir::ExprId,
        model: hir::DefId,
        field: hir::FieldRef,
        at: Span,
    ) -> Lowered<Local> {
        let ty = self.field_ty(model, field, at)?;
        if let Some(hir::ExprKind::Name(hir::Res::Context(hir::ContextName::New))) = self.kind(base)
        {
            if self.rule.as_ref().and_then(|r| r.model) != Some(model) {
                return Err(LowerError::Unchecked(at));
            }
            let dst = self.local(ty);
            self.emit(
                InstKind::RuleField {
                    dst,
                    state: RowState::Proposed,
                    field: field_ref(field),
                },
                at,
            );
            return Ok(dst);
        }
        let row = self.row(base, model, at)?;
        if field == hir::FieldRef::Implicit(hir::ImplicitField::Id) {
            return Ok(row);
        }
        let model = self.model(model, at)?;
        let dst = self.local(ty);
        let auth = self.auth();
        self.emit(
            InstKind::GetField {
                dst,
                row,
                model,
                field: field_ref(field),
                auth,
            },
            at,
        );
        Ok(dst)
    }

    /// A row of `model`: `me` or a reference value.
    fn row(&mut self, e: hir::ExprId, model: hir::DefId, at: Span) -> Lowered<Local> {
        let want = Ty::Ref(self.model(model, at)?);
        let row = match self.kind(e) {
            Some(hir::ExprKind::Me) => self.me_row(at)?,
            _ => self.expr(e)?,
        };
        if self.ty(row) != want {
            return Err(LowerError::Unchecked(at));
        }
        Ok(row)
    }

    /// `me` as `User?`, `none` without a principal.
    fn me(&mut self, at: Span) -> Lowered<Local> {
        let user = self.cx.user.ok_or(LowerError::Unchecked(at))?;
        let dst = self.local(Ty::Optional(Box::new(Ty::Ref(user))));
        self.emit(InstKind::Me { dst }, at);
        Ok(dst)
    }

    /// `me` as a row. Only a predicate lowers it, and only behind the guard
    /// at its entry, so `me` is present here (SYNTAX 4.4). Elsewhere the IR
    /// has no way yet to fail on a missing `me`, so it is not lowered there.
    fn me_row(&mut self, at: Span) -> Lowered<Local> {
        if self.kind != FnKind::Predicate || !self.me_guarded {
            return Err(LowerError::Unsupported(at));
        }
        let me = self.me(at)?;
        let Ty::Optional(user) = self.ty(me) else {
            return Err(LowerError::Unchecked(at));
        };
        let dst = self.local(*user);
        self.emit(InstKind::Unwrap { dst, src: me }, at);
        Ok(dst)
    }

    /// Returns `false` from the predicate when `me` is `none` and continues
    /// in a new block otherwise.
    fn deny_unless_some(&mut self, me: Local, at: Span) {
        let is = self.local(Ty::Bool);
        self.emit(InstKind::IsSome { dst: is, src: me }, at);
        let some = self.new_block();
        let none = self.new_block();
        self.terminate(
            Terminator::Branch {
                cond: is,
                then: BlockId(some as u32),
                otherwise: BlockId(none as u32),
            },
            at,
        );
        self.cur = Some(none);
        let no = self.constant(Const::Bool(false), Ty::Bool, at);
        self.terminate(Terminator::Return(Some(no)), at);
        self.cur = Some(some);
    }

    fn binary(
        &mut self,
        e: hir::ExprId,
        op: BinaryOp,
        lhs: hir::ExprId,
        rhs: hir::ExprId,
        at: Span,
    ) -> Lowered<Local> {
        let op = match op {
            BinaryOp::And | BinaryOp::Or => return self.logic(op == BinaryOp::And, lhs, rhs, at),
            BinaryOp::Eq | BinaryOp::Ne => {
                let (l, r) = self.pair(lhs, rhs, at)?;
                let op = if op == BinaryOp::Eq {
                    BinOp::Eq
                } else {
                    BinOp::Ne
                };
                let dst = self.local(Ty::Bool);
                self.emit(
                    InstKind::Binary {
                        dst,
                        op,
                        lhs: l,
                        rhs: r,
                    },
                    at,
                );
                return Ok(dst);
            }
            BinaryOp::Lt => BinOp::Lt,
            BinaryOp::Le => BinOp::Le,
            BinaryOp::Gt => BinOp::Gt,
            BinaryOp::Ge => BinOp::Ge,
            BinaryOp::Add => BinOp::Add,
            BinaryOp::Sub => BinOp::Sub,
            BinaryOp::Mul => BinOp::Mul,
            BinaryOp::Div => BinOp::Div,
            BinaryOp::Rem => BinOp::Rem,
        };
        let ty = self.expr_ty(e)?;
        let l = self.expr(lhs)?;
        let r = self.expr(rhs)?;
        let dst = self.local(ty);
        self.emit(
            InstKind::Binary {
                dst,
                op,
                lhs: l,
                rhs: r,
            },
            at,
        );
        Ok(dst)
    }

    /// The operands of `==` or `!=` at one type: a `none`, `me` or literal
    /// operand takes the type of the other one, and a plain value is wrapped
    /// when the other one is its optional (`none` equals only `none`).
    fn pair(&mut self, lhs: hir::ExprId, rhs: hir::ExprId, at: Span) -> Lowered<(Local, Local)> {
        let contextual = |fb: &Self, e| {
            matches!(
                fb.kind(e),
                Some(
                    hir::ExprKind::NoneValue
                        | hir::ExprKind::Me
                        | hir::ExprKind::Set(_)
                        | hir::ExprKind::List(_)
                        | hir::ExprKind::Map(_)
                )
            )
        };
        let (l, r) = match (contextual(self, lhs), contextual(self, rhs)) {
            (false, _) => {
                let l = self.expr(lhs)?;
                let r = self.loose(rhs, &self.ty(l))?;
                (l, r)
            }
            (true, false) => {
                let r = self.expr(rhs)?;
                let l = self.loose(lhs, &self.ty(r))?;
                (l, r)
            }
            (true, true) => match (self.kind(lhs), self.kind(rhs)) {
                (Some(hir::ExprKind::Me), Some(hir::ExprKind::Me)) => (self.me(at)?, self.me(at)?),
                _ => return Err(LowerError::Unchecked(at)),
            },
        };
        let (lt, rt) = (self.ty(l), self.ty(r));
        if lt == rt {
            Ok((l, r))
        } else if lt == Ty::Optional(Box::new(rt.clone())) {
            Ok((l, self.coerce(r, &lt, at)?))
        } else if rt == Ty::Optional(Box::new(lt.clone())) {
            Ok((self.coerce(l, &rt, at)?, r))
        } else {
            Err(LowerError::Unchecked(at))
        }
    }

    /// An operand of `==` next to one of type `other`.
    fn loose(&mut self, e: hir::ExprId, other: &Ty) -> Lowered<Local> {
        let optional = match other {
            Ty::Optional(_) => other.clone(),
            t => Ty::Optional(Box::new(t.clone())),
        };
        match self.kind(e) {
            Some(hir::ExprKind::NoneValue | hir::ExprKind::Me) => self.value(e, &optional),
            Some(hir::ExprKind::Set(_) | hir::ExprKind::List(_) | hir::ExprKind::Map(_)) => {
                self.value(e, other)
            }
            _ => self.expr(e),
        }
    }

    /// `a and b`, `a or b`: `b` runs only when it decides the result.
    fn logic(&mut self, and: bool, lhs: hir::ExprId, rhs: hir::ExprId, at: Span) -> Lowered<Local> {
        let dst = self.local(Ty::Bool);
        let l = self.value(lhs, &Ty::Bool)?;
        self.emit(InstKind::Copy { dst, src: l }, at);
        let right = self.new_block();
        let mut open = Vec::new();
        let here = self.leave().ok_or(LowerError::Unchecked(at))?;
        let join_later = here;
        self.cur = Some(right);
        let r = self.value(rhs, &Ty::Bool)?;
        self.emit(InstKind::Copy { dst, src: r }, at);
        open.extend(self.leave());
        // The left block branches to the right side or straight to the join.
        let join = self.new_block();
        for b in open {
            self.set_term(b, Terminator::Jump(BlockId(join as u32)), at);
        }
        let (then, otherwise) = if and { (right, join) } else { (join, right) };
        self.set_term(
            join_later,
            Terminator::Branch {
                cond: l,
                then: BlockId(then as u32),
                otherwise: BlockId(otherwise as u32),
            },
            at,
        );
        self.cur = Some(join);
        Ok(dst)
    }

    /// `v ?? f` on an optional, `v ?? f` and `v catch K ?? f` on a remote
    /// value (G11).
    fn fallback(&mut self, value: hir::ExprId, fallback: hir::ExprId, at: Span) -> Lowered<Local> {
        let unchecked = LowerError::Unchecked(at);
        let (value, kinds) = match self.kind(value) {
            Some(hir::ExprKind::Catch { value, kinds }) => {
                let kinds = kinds
                    .iter()
                    .map(|k| CallErrorKind::from_name(k).ok_or(unchecked))
                    .collect::<Lowered<Vec<_>>>()?;
                (*value, Some(kinds))
            }
            _ => (value, None),
        };
        let v = self.expr(value)?;
        match (self.ty(v), kinds) {
            (Ty::Remote(item), kinds) => {
                let f = self.value(fallback, &item)?;
                let dst = self.local(*item);
                self.emit(
                    InstKind::Settle {
                        dst,
                        remote: v,
                        caught: Caught::new(kinds.unwrap_or_default()),
                        fallback: f,
                    },
                    at,
                );
                Ok(dst)
            }
            (Ty::Optional(item), None) => {
                let dst = self.local((*item).clone());
                let is = self.local(Ty::Bool);
                self.emit(InstKind::IsSome { dst: is, src: v }, at);
                let some = self.new_block();
                let none = self.new_block();
                self.terminate(
                    Terminator::Branch {
                        cond: is,
                        then: BlockId(some as u32),
                        otherwise: BlockId(none as u32),
                    },
                    at,
                );
                let mut open = Vec::new();
                self.cur = Some(some);
                self.emit(InstKind::Unwrap { dst, src: v }, at);
                open.extend(self.leave());
                self.cur = Some(none);
                let f = self.value(fallback, &item)?;
                self.emit(InstKind::Copy { dst, src: f }, at);
                open.extend(self.leave());
                self.join(open, at);
                Ok(dst)
            }
            _ => Err(unchecked),
        }
    }

    /// `make Model { ... }`: every declared field except `serial` ones, in
    /// field order; a field left out takes its default, evaluated here
    /// (D116: the server never fills a default of a client op).
    fn make(
        &mut self,
        model: hir::DefId,
        given: &[(hir::FieldId, hir::ExprId)],
        at: Span,
    ) -> Lowered<Local> {
        let unchecked = LowerError::Unchecked(at);
        let Some(hir::DefKind::Model(def)) = self.cx.hir.def(model).map(|d| &d.kind) else {
            return Err(unchecked);
        };
        let mut fields = Vec::new();
        for (i, field) in def.fields.iter().enumerate() {
            let named: Vec<hir::ExprId> = given
                .iter()
                .filter(|(f, _)| f.index() == i)
                .map(|(_, e)| *e)
                .collect();
            let serial = matches!(field.mode, hir::FieldMode::Serial(_));
            let value = match (named.as_slice(), serial, field.default) {
                ([], true, _) => continue,
                ([e], false, _) => *e,
                ([], false, Some(default)) => default,
                _ => return Err(unchecked),
            };
            let ty = self.cx.ty(&field.ty).ok_or(unchecked)?;
            let local = self.value(value, &ty)?;
            fields.push((FieldId(i as u32), local));
        }
        let model = self.model(model, at)?;
        let dst = self.local(Ty::Ref(model));
        let auth = self.auth();
        self.emit(
            InstKind::Make {
                dst: Some(dst),
                model,
                fields,
                auth,
            },
            at,
        );
        Ok(dst)
    }

    /// The plan of a query with the `see` filter of the code's side.
    fn plan(&mut self, q: &hir::Query, at: Span) -> Lowered<QueryPlan> {
        let model = self.model(q.model, at)?;
        let cx = self.cx;
        let filter = match q.filter {
            Some(f) => Some(filter::filter(cx, q.model, f, self)?),
            None => None,
        };
        let sort = match q.sort {
            None => None,
            // The order without `sort` is by id ascending (SPEC 12.10).
            Some((hir::FieldRef::Implicit(hir::ImplicitField::Id), false)) => None,
            Some((hir::FieldRef::Implicit(hir::ImplicitField::Id), true)) => {
                return Err(LowerError::Unsupported(at));
            }
            Some((field, desc)) => Some((field_ref(field), desc)),
        };
        let (take, take_span) = match q.take {
            Some((kind, n)) => {
                let local = self.value(n, &Ty::Int)?;
                let kind = match kind {
                    TakeKind::Limit => Take::First,
                    TakeKind::Last => Take::Last,
                };
                (Some((kind, local)), Some(filter::span(cx, n)))
            }
            None => (None, None),
        };
        let see = match self.placement {
            Placement::Server if self.elevated => See::System,
            Placement::Server => match cx.see.get(model.index()).cloned().flatten() {
                Some(f) => See::Rule(f),
                None => See::DenyAll,
            },
            // Shared code runs no query; `validate` reports it.
            Placement::Client | Placement::Shared => See::Replica,
        };
        Ok(QueryPlan {
            model,
            filter,
            see,
            sort,
            take,
            take_span,
        })
    }
}

impl Operands for Fb<'_, '_> {
    fn operand(&mut self, e: hir::ExprId) -> Lowered<Operand> {
        if let Some(op) = filter::constant(self.cx, e) {
            return Ok(op);
        }
        Ok(Operand::Local(self.expr(e)?))
    }
}
