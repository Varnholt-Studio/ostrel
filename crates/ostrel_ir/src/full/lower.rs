//! Lowering of the checked HIR to the full IR (ARCHITECTURE 3, `lower`, D140).
//!
//! Like the v0.1 slice, an input the checker did not accept is a
//! [`LowerError`], a compiler defect, never a panic. The result is checked
//! with [`Program::validate`] before it is returned, so a lowered program
//! always keeps the placement and authority contract.
//!
//! Lowered: the `app` name and auth mode, enums, models with their fields,
//! the `see` rule as a filter of the pushdown subset (ARCHITECTURE 9), the
//! `make`, `edit` and `drop` rules and the `check` invariants as predicate
//! functions under [`Auth::Rule`](super::Auth::Rule), field defaults and
//! `var` initialisers as client functions without parameters (D116), top
//! level `var`s, `style` positions, and every function with its body: the
//! placement split with [`InstKind::RemoteCall`](super::InstKind::RemoteCall)
//! and [`InstKind::Settle`](super::InstKind::Settle) (G11), the prologue of a
//! server function with model resolution through `see` and its `call` rule
//! (D98, D99, D117), writes under [`Auth::Caller`](super::Auth::Caller) or,
//! in `as system` code, [`Auth::System`](super::Auth::System), queries with
//! the `see` filter of their side, and bulk `drop` (D100).
//!
//! Not lowered yet, reported as [`LowerError::Unsupported`] at the construct:
//! views and handlers, bulk `edit`, `me` as a row outside a predicate, the
//! std calls the IR has no [`StdFn`](super::StdFn) for (`Url.parse`, `keys`,
//! `values`, `days`, `label`), whole value writes of collection fields,
//! several `see` rules on one model and `sort id desc`. A model whose rules
//! are missing stays fail closed: no rule denies (D8).

mod body;
mod filter;

#[cfg(test)]
mod deltas_tests;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use ostrel_core::Span;
use ostrel_sema::hir;

use super::query::Filter;
use super::{App, Enum, EnumId, Field, FieldMode, FieldRef, Merge, Model, ModelId, Program};
use super::{
    FnKind, FuncId, ImplicitField, Placement, Prologue, Registration, Rule, RuleId, RuleOp, Ty,
    Var, Violation,
};

use body::{Fb, RuleCx};

/// Why a HIR could not be lowered. The span points at the construct where
/// this was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LowerError {
    /// A type, name or construct that `check` would have resolved or
    /// rejected: an unknown type, a `see` rule outside the pushdown subset, a
    /// function without `return` on some path.
    Unchecked(Span),
    /// A construct the lowering does not handle yet (see the module
    /// documentation). Never a reason to admit what a rule denies.
    Unsupported(Span),
    /// The lowered program breaks the contract of [`Program::validate`]; the
    /// first violation. For a checked HIR this is a defect of the lowering,
    /// or a placement the checker did not infer yet.
    Contract(Violation),
}

type Lowered<T> = Result<T, LowerError>;

/// Lowers a program on which `hir::resolve` and `hir::check` reported no
/// error.
pub fn lower(program: &hir::Program) -> Result<Program, LowerError> {
    let mut cx = Cx::new(program)?;
    let mut out = Program::default();
    cx.schema(&mut out)?;
    cx.see = cx.sees()?;
    for (model, see) in out.models.iter_mut().zip(&cx.see) {
        model.see = see.clone();
    }
    cx.functions(&mut out)?;
    cx.model_rules(&mut out)?;
    cx.vars(&mut out)?;
    if let Some(first) = out.validate().into_iter().next() {
        return Err(LowerError::Contract(first));
    }
    Ok(out)
}

/// The signature of a source function, known before any body is lowered.
#[derive(Clone, Debug)]
pub(super) struct Sig {
    pub(super) id: FuncId,
    pub(super) placement: Placement,
    pub(super) params: Vec<Ty>,
    pub(super) result: Ty,
}

/// What every body needs: the HIR, its ids in the IR and the signatures.
pub(super) struct Cx<'h> {
    pub(super) hir: &'h hir::Program,
    pub(super) enums: HashMap<hir::DefId, EnumId>,
    pub(super) models: HashMap<hir::DefId, ModelId>,
    pub(super) fns: HashMap<hir::DefId, Sig>,
    pub(super) vars: HashMap<hir::DefId, (super::VarId, Ty)>,
    /// The std `User` in an app with `auth` (G8, D142).
    pub(super) user: Option<ModelId>,
    /// The `see` filter of every model, by [`ModelId`], once lowered.
    pub(super) see: Vec<Option<Filter>>,
}

impl<'h> Cx<'h> {
    fn new(hir: &'h hir::Program) -> Lowered<Self> {
        let mut cx = Cx {
            hir,
            enums: HashMap::new(),
            models: HashMap::new(),
            fns: HashMap::new(),
            vars: HashMap::new(),
            user: None,
            see: Vec::new(),
        };
        for (index, def) in hir.defs.iter().enumerate() {
            let def_id = hir::DefId(index as u32);
            match &def.kind {
                hir::DefKind::Enum(_) => {
                    cx.enums.insert(def_id, EnumId(cx.enums.len() as u32));
                }
                hir::DefKind::Model(_) => {
                    cx.models.insert(def_id, ModelId(cx.models.len() as u32));
                }
                _ => {}
            }
        }
        let auth = hir
            .app
            .and_then(|a| hir.def(a))
            .is_some_and(|d| matches!(&d.kind, hir::DefKind::App(app) if app.auth.is_some()));
        if auth {
            cx.user = hir
                .defs
                .iter()
                .enumerate()
                .find(|(_, d)| &*d.name == "User" && matches!(d.kind, hir::DefKind::Model(_)))
                .and_then(|(i, _)| cx.models.get(&hir::DefId(i as u32)).copied());
        }
        // Source functions take the first function ids, in source order, so
        // that every call can name its callee before the callee is lowered.
        let mut next = 0;
        for (index, def) in hir.defs.iter().enumerate() {
            match &def.kind {
                hir::DefKind::Fn(f) => {
                    let params = f
                        .params
                        .iter()
                        .map(|&l| cx.local_ty(l, def.span))
                        .collect::<Lowered<Vec<_>>>()?;
                    let result = cx.ty(&f.result).ok_or(LowerError::Unchecked(def.span))?;
                    cx.fns.insert(
                        hir::DefId(index as u32),
                        Sig {
                            id: FuncId(next),
                            placement: placement(f),
                            params,
                            result,
                        },
                    );
                    next += 1;
                }
                hir::DefKind::Var(v) => {
                    let ty =
                        v.ty.as_ref()
                            .and_then(|t| cx.ty(t))
                            .ok_or(LowerError::Unchecked(def.span))?;
                    let id = super::VarId(cx.vars.len() as u32);
                    cx.vars.insert(hir::DefId(index as u32), (id, ty));
                }
                _ => {}
            }
        }
        Ok(cx)
    }

    /// The type of a local after `check`.
    pub(super) fn local_ty(&self, local: hir::LocalId, at: Span) -> Lowered<Ty> {
        self.hir
            .local(local)
            .and_then(|l| l.ty.as_ref())
            .and_then(|t| self.ty(t))
            .ok_or(LowerError::Unchecked(at))
    }

    /// The HIR definition of the std `User`.
    pub(super) fn user_def(&self) -> Option<hir::DefId> {
        let user = self.user?;
        self.models
            .iter()
            .find(|(_, id)| **id == user)
            .map(|(def, _)| *def)
    }

    /// App, enums, models with their fields, styles; views are not lowered.
    fn schema(&self, out: &mut Program) -> Lowered<()> {
        for def in &self.hir.defs {
            match &def.kind {
                hir::DefKind::App(app) => {
                    out.app = Some(App {
                        name: def.name.to_string(),
                        auth: app.auth.map(|mode| match mode {
                            hir::Registration::Open => Registration::Open,
                            hir::Registration::Invite => Registration::Invite,
                        }),
                        home: None,
                    });
                }
                hir::DefKind::Enum(variants) => out.enums.push(Enum {
                    name: def.name.to_string(),
                    variants: variants.iter().map(|v| v.to_string()).collect(),
                }),
                hir::DefKind::Model(model) => {
                    let fields = model
                        .fields
                        .iter()
                        .map(|field| self.field(field))
                        .collect::<Result<_, _>>()?;
                    out.models.push(Model {
                        name: def.name.to_string(),
                        fields,
                        see: None,
                        rules: Vec::new(),
                        checks: Vec::new(),
                        span: def.span,
                    });
                }
                hir::DefKind::View(_) => return Err(LowerError::Unsupported(def.span)),
                hir::DefKind::Style => out.styles.push(def.span),
                _ => {}
            }
        }
        Ok(())
    }

    /// The `see` rule of every model, by [`ModelId`], before any body:
    /// queries copy it.
    fn sees(&self) -> Lowered<Vec<Option<Filter>>> {
        let mut sees = Vec::new();
        for (index, def) in self.hir.defs.iter().enumerate() {
            let hir::DefKind::Model(model) = &def.kind else {
                continue;
            };
            let mut see = None;
            for rule in model.rules.iter().filter(|r| r.op == hir::RuleOp::See) {
                if see.is_some() {
                    // At most one rule per verb (D180).
                    return Err(LowerError::Unchecked(rule.span));
                }
                see = Some(filter::see_rule(self, hir::DefId(index as u32), rule.cond)?);
            }
            sees.push(see);
        }
        Ok(sees)
    }

    /// Every source function with its prologue and `call` rule.
    fn functions(&self, out: &mut Program) -> Lowered<()> {
        let mut defs: Vec<(hir::DefId, &hir::Def, &hir::FnDef)> = Vec::new();
        for (index, def) in self.hir.defs.iter().enumerate() {
            if let hir::DefKind::Fn(f) = &def.kind {
                defs.push((hir::DefId(index as u32), def, f));
            }
        }
        for (def_id, def, f) in &defs {
            let sig = self
                .fns
                .get(def_id)
                .ok_or(LowerError::Unchecked(def.span))?;
            let mut fb = Fb::new(
                self,
                def.name.to_string(),
                sig.placement,
                FnKind::Source,
                f.elevated.is_some(),
                def.span,
            );
            fb.params(&f.params)?;
            let function = fb.body(f.body, &sig.result)?;
            out.functions.push(function);
        }
        for (def_id, def, f) in &defs {
            let sig = self
                .fns
                .get(def_id)
                .ok_or(LowerError::Unchecked(def.span))?;
            if sig.placement != Placement::Server {
                if let Some(rule) = &f.call_rule {
                    // A `call` rule guards client entries of a server
                    // function only (SYNTAX 4.6).
                    return Err(LowerError::Unchecked(rule.span));
                }
                continue;
            }
            let call_rule = match &f.call_rule {
                Some(rule) => {
                    let mut fb = Fb::new(
                        self,
                        format!("{}.call", def.name),
                        Placement::Shared,
                        FnKind::Predicate,
                        false,
                        rule.span,
                    );
                    fb.params(&f.params)?;
                    let pred = fb.predicate(rule.cond, RuleCx::call())?;
                    Some(push_rule(out, RuleOp::Call, None, None, pred, rule.span))
                }
                None => None,
            };
            let resolve = sig
                .params
                .iter()
                .enumerate()
                .filter(|(_, ty)| matches!(ty, Ty::Ref(_)))
                .map(|(i, _)| super::Local(i as u32))
                .collect();
            if let Some(function) = out.functions.get_mut(sig.id.index()) {
                function.prologue = Some(Prologue { resolve, call_rule });
            }
        }
        if self.hir.app.is_none() {
            out.main = defs
                .iter()
                .find(|(_, def, _)| &*def.name == "main")
                .and_then(|(id, _, _)| self.fns.get(id))
                .map(|sig| sig.id);
        }
        Ok(())
    }

    /// `make`, `edit` and `drop` rules, `check` invariants and field
    /// defaults.
    fn model_rules(&self, out: &mut Program) -> Lowered<()> {
        for (index, def) in self.hir.defs.iter().enumerate() {
            let hir::DefKind::Model(model) = &def.kind else {
                continue;
            };
            let def_id = hir::DefId(index as u32);
            let id = *self
                .models
                .get(&def_id)
                .ok_or(LowerError::Unchecked(def.span))?;
            // At most one rule per verb and per field of `edit` (D180).
            let mut seen = Vec::new();
            for rule in &model.rules {
                if rule.op != hir::RuleOp::See {
                    let key = (rule.op, rule.field);
                    if seen.contains(&key) {
                        return Err(LowerError::Unchecked(rule.span));
                    }
                    seen.push(key);
                }
                let (op, name) = match rule.op {
                    hir::RuleOp::See => continue,
                    hir::RuleOp::Make => (RuleOp::Make, "make"),
                    hir::RuleOp::Edit => (RuleOp::Edit, "edit"),
                    hir::RuleOp::Drop => (RuleOp::Drop, "drop"),
                    hir::RuleOp::Call => return Err(LowerError::Unchecked(rule.span)),
                };
                let field_def = rule.field.and_then(|f| model.fields.get(f.index()));
                let label = match field_def {
                    Some(f) => format!("{}.{name} {}", def.name, f.name),
                    None => format!("{}.{name}", def.name),
                };
                // `added` and `removed` of a field rule on a collection have
                // the type of the field (G12, D97).
                let delta = match field_def {
                    Some(f) if matches!(f.ty, hir::Ty::Set(_) | hir::Ty::Map(..)) => {
                        Some(self.ty(&f.ty).ok_or(LowerError::Unchecked(f.span))?)
                    }
                    _ => None,
                };
                // In `make` bare names are the new row, in `edit` and `drop`
                // the stored one (SYNTAX 4.4).
                let state = match op {
                    RuleOp::Make => super::RowState::Proposed,
                    _ => super::RowState::Stored,
                };
                let fb = Fb::new(
                    self,
                    label,
                    Placement::Shared,
                    FnKind::Predicate,
                    false,
                    rule.span,
                );
                // The directions the rule admits, from the one function the
                // E0316 warning uses too (D187).
                let deltas =
                    ostrel_sema::rule_deltas::rule_deltas(self.hir, def_id, rule).map(|d| {
                        super::Deltas {
                            added: d.added,
                            removed: d.removed,
                        }
                    });
                if deltas.is_some() != delta.is_some() {
                    return Err(LowerError::Unchecked(rule.span));
                }
                let pred = fb.predicate(rule.cond, RuleCx::row(def_id, state, delta))?;
                let field = rule.field.map(|f| super::FieldId(f.0));
                let rule_id = push_rule(out, op, field, deltas, pred, rule.span);
                if let Some(m) = out.models.get_mut(id.index()) {
                    m.rules.push(rule_id);
                }
            }
            for (i, &cond) in model.checks.iter().enumerate() {
                let at = self.hir.expr(cond).map_or(def.span, |e| e.span);
                let fb = Fb::new(
                    self,
                    format!("{}.check{i}", def.name),
                    Placement::Shared,
                    FnKind::Predicate,
                    false,
                    at,
                );
                let rule = RuleCx::row(def_id, super::RowState::Proposed, None);
                let func = push_function(out, fb.predicate(cond, rule)?);
                if let Some(m) = out.models.get_mut(id.index()) {
                    m.checks.push(func);
                }
            }
            for (i, field) in model.fields.iter().enumerate() {
                let Some(value) = field.default else {
                    continue;
                };
                let ty = self
                    .ty(&field.ty)
                    .ok_or(LowerError::Unchecked(field.span))?;
                let fb = Fb::new(
                    self,
                    format!("{}.{}.default", def.name, field.name),
                    Placement::Client,
                    FnKind::Derived,
                    false,
                    field.span,
                );
                let func = push_function(out, fb.thunk(value, &ty)?);
                if let Some(f) = out
                    .models
                    .get_mut(id.index())
                    .and_then(|m| m.fields.get_mut(i))
                {
                    f.default = Some(func);
                }
            }
        }
        Ok(())
    }

    /// Top level `var`s with their initialisers, in [`super::VarId`] order.
    fn vars(&self, out: &mut Program) -> Lowered<()> {
        let mut vars: Vec<(super::VarId, Var)> = Vec::new();
        for (index, def) in self.hir.defs.iter().enumerate() {
            let hir::DefKind::Var(v) = &def.kind else {
                continue;
            };
            let (id, ty) = self
                .vars
                .get(&hir::DefId(index as u32))
                .cloned()
                .ok_or(LowerError::Unchecked(def.span))?;
            let init = match v.value {
                Some(value) => {
                    let fb = Fb::new(
                        self,
                        format!("{}.init", def.name),
                        Placement::Client,
                        FnKind::Derived,
                        false,
                        def.span,
                    );
                    Some(push_function(out, fb.thunk(value, &ty)?))
                }
                None => None,
            };
            vars.push((
                id,
                Var {
                    name: def.name.to_string(),
                    ty,
                    init,
                    span: def.span,
                },
            ));
        }
        vars.sort_by_key(|(id, _)| *id);
        out.vars = vars.into_iter().map(|(_, v)| v).collect();
        Ok(())
    }

    fn field(&self, field: &hir::FieldDef) -> Lowered<Field> {
        let unchecked = LowerError::Unchecked(field.span);
        let ty = self.ty(&field.ty).ok_or(unchecked)?;
        if !ty.is_field_type() {
            return Err(unchecked);
        }
        let mode = match field.mode {
            hir::FieldMode::Plain => FieldMode::Plain,
            hir::FieldMode::Fixed => FieldMode::Fixed,
            hir::FieldMode::Server => FieldMode::Server,
            hir::FieldMode::Serial(per) => FieldMode::Serial(per.map(field_ref)),
        };
        let merge = match field.merge {
            hir::Merge::Default => Merge::Default,
            hir::Merge::Text => Merge::Text,
        };
        Ok(Field {
            name: field.name.to_string(),
            ty,
            mode,
            merge,
            default: None,
            span: field.span,
        })
    }

    /// The IR type of a HIR type; `None` for `Error`, for `Url` (the IR has
    /// no `Url` type yet) and for a definition that is not an enum or model
    /// of this program.
    pub(super) fn ty(&self, ty: &hir::Ty) -> Option<Ty> {
        let boxed = |inner: &hir::Ty| self.ty(inner).map(Box::new);
        Some(match ty {
            hir::Ty::Bool => Ty::Bool,
            hir::Ty::Int => Ty::Int,
            hir::Ty::Float => Ty::Float,
            hir::Ty::Text => Ty::Text,
            hir::Ty::Time => Ty::Time,
            hir::Ty::Bytes => Ty::Bytes,
            hir::Ty::Rank => Ty::Rank,
            hir::Ty::Enum(def) => Ty::Enum(*self.enums.get(def)?),
            hir::Ty::Ref(def) => Ty::Ref(*self.models.get(def)?),
            hir::Ty::Optional(inner) => Ty::Optional(boxed(inner)?),
            hir::Ty::Set(inner) => Ty::Set(boxed(inner)?),
            hir::Ty::Map(key, value) => Ty::Map(boxed(key)?, boxed(value)?),
            hir::Ty::List(inner) => Ty::List(boxed(inner)?),
            hir::Ty::Remote(inner) => Ty::Remote(boxed(inner)?),
            hir::Ty::Rows(def) => Ty::Rows(*self.models.get(def)?),
            hir::Ty::Range(inner) => Ty::Range(boxed(inner)?),
            hir::Ty::Unit => Ty::Unit,
            _ => return None,
        })
    }
}

/// Appends a function and gives its id.
fn push_function(out: &mut Program, function: super::Function) -> FuncId {
    out.functions.push(function);
    FuncId(out.functions.len() as u32 - 1)
}

/// Appends a predicate and its rule and gives the rule id.
fn push_rule(
    out: &mut Program,
    op: RuleOp,
    field: Option<super::FieldId>,
    deltas: Option<super::Deltas>,
    pred: super::Function,
    span: Span,
) -> RuleId {
    let pred = push_function(out, pred);
    out.rules.push(Rule {
        op,
        field,
        pred,
        deltas,
        span,
    });
    RuleId(out.rules.len() as u32 - 1)
}

/// The placement of a function: the inferred one after `check`, else the
/// declared one. ASSUMPTION (until the checker infers placement, T218): an
/// unmarked function without inferred placement is shared; one that reads
/// rows then fails [`Program::validate`] and gives [`LowerError::Contract`].
fn placement(f: &hir::FnDef) -> Placement {
    match f.placement.or(f.declared) {
        Some(hir::Placement::Server) => Placement::Server,
        Some(hir::Placement::Client) => Placement::Client,
        Some(hir::Placement::Shared) | None => Placement::Shared,
    }
}

/// A HIR field reference as an IR one; field ids keep their position.
pub(super) fn field_ref(field: hir::FieldRef) -> FieldRef {
    match field {
        hir::FieldRef::Declared(id) => FieldRef::Declared(super::FieldId(id.0)),
        hir::FieldRef::Implicit(implicit) => FieldRef::Implicit(match implicit {
            hir::ImplicitField::Id => ImplicitField::Id,
            hir::ImplicitField::Made => ImplicitField::Made,
            hir::ImplicitField::Changed => ImplicitField::Changed,
            hir::ImplicitField::Author => ImplicitField::Author,
            hir::ImplicitField::Pending => ImplicitField::Pending,
            hir::ImplicitField::Rejected => ImplicitField::Rejected,
        }),
    }
}
