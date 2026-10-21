//! Type checker of the HIR (AC-29, `docs/types.md`).
//!
//! [`check`] gives every expression, local and `var` a type and reports
//! values whose type differs from the one their place requires (E0304),
//! conditions that are not `Bool` (E0301) and calls with the wrong number of
//! arguments (E0302). Codes and message forms are those of the v0.1 checker
//! in the crate root.
//!
//! The checker reports only what it can prove. Where the type of an
//! expression is not decided yet (properties other than `len` and `trim`,
//! methods of collections, contextual names, std names other than
//! `Url.parse`), the expression gets [`Ty::Error`] and nothing is reported,
//! so a valid program never fails here. Not yet covered: `E0300` and the
//! ordering of optionals (both stand at the operator, which the HIR does not
//! record), the missing `return` (E0305), placement, the `call` rule of an
//! elevated function (E0306) and the words of D142.

use std::collections::HashMap;

use ostrel_core::{Diagnostic, Span};
use ostrel_syntax::ast::{BinaryOp, UnaryOp};

use crate::codes;
use crate::hir::{
    BlockId, DefId, DefKind, Element, ElementName, ExprId, ExprKind, FieldMode, FieldRef,
    ImplicitField, LocalId, Placement, Program, Res, StdName, StmtId, StmtKind, StrPart, Ty,
    ViewNodeId, ViewNodeKind,
};

/// Checks types and fills [`crate::hir::Expr::ty`], [`crate::hir::Local::ty`]
/// and [`crate::hir::VarDef::ty`].
pub(crate) fn check(program: &mut Program) -> Vec<Diagnostic> {
    let mut cx = Cx::new(program);
    cx.run();
    let Cx {
        exprs,
        locals,
        vars,
        diags,
        ..
    } = cx;
    for (expr, ty) in program.exprs.iter_mut().zip(exprs) {
        expr.ty = Some(ty.unwrap_or(Ty::Error));
    }
    for (local, ty) in program.locals.iter_mut().zip(locals) {
        local.ty = Some(ty.unwrap_or(Ty::Error));
    }
    for (def, ty) in vars {
        if let Some(DefKind::Var(v)) = program.defs.get_mut(def.index()).map(|d| &mut d.kind) {
            v.ty = Some(ty);
        }
    }
    diags
}

/// Where the code being checked runs, as far as declarations show it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    /// A `server fn`.
    Server,
    /// A view, a handler or a `client fn`: a call of a `server fn` is remote.
    Client,
    /// Anything else; a call of a `server fn` gets no type yet.
    Open,
}

struct Cx<'p> {
    p: &'p Program,
    exprs: Vec<Option<Ty>>,
    locals: Vec<Option<Ty>>,
    vars: HashMap<DefId, Ty>,
    diags: Vec<Diagnostic>,
    /// The std `User` in an app with `auth`.
    user: Option<DefId>,
    /// The result type of the function whose body is checked.
    result: Option<Ty>,
    side: Side,
}

impl<'p> Cx<'p> {
    fn new(p: &'p Program) -> Self {
        let auth = p
            .app
            .and_then(|a| p.def(a))
            .is_some_and(|d| matches!(&d.kind, DefKind::App(app) if app.auth.is_some()));
        let user = p
            .defs
            .iter()
            .position(|d| &*d.name == "User" && matches!(d.kind, DefKind::Model(_)))
            .filter(|_| auth)
            .map(|i| DefId(u32::try_from(i).unwrap_or(u32::MAX)));
        Cx {
            p,
            exprs: vec![None; p.exprs.len()],
            locals: p.locals.iter().map(|l| l.ty.clone()).collect(),
            vars: HashMap::new(),
            diags: Vec::new(),
            user,
            result: None,
            side: Side::Open,
        }
    }

    fn run(&mut self) {
        let p = self.p;
        // `var`s first: functions and views read their types.
        for (i, def) in p.defs.iter().enumerate() {
            if let DefKind::Var(v) = &def.kind {
                let found = v.value.map(|e| self.expr(e));
                let ty = match (&v.ty, found) {
                    (Some(want), Some(found)) => {
                        if let Some(e) = v.value {
                            self.expect(e, want, &found);
                        }
                        want.clone()
                    }
                    (Some(want), None) => want.clone(),
                    (None, Some(found)) => found,
                    (None, None) => Ty::Error,
                };
                self.vars.insert(DefId(id(i)), ty);
            }
        }
        for def in &p.defs {
            match &def.kind {
                DefKind::Model(m) => {
                    self.side = Side::Open;
                    for f in &m.fields {
                        if let Some(e) = f.default {
                            let found = self.expr(e);
                            self.expect(e, &f.ty, &found);
                        }
                    }
                    for rule in &m.rules {
                        self.condition(rule.cond);
                    }
                    for &c in &m.checks {
                        self.condition(c);
                    }
                }
                DefKind::Fn(f) => {
                    self.side = match f.declared {
                        Some(Placement::Server) => Side::Server,
                        Some(Placement::Client) => Side::Client,
                        _ => Side::Open,
                    };
                    if let Some(rule) = &f.call_rule {
                        self.condition(rule.cond);
                    }
                    self.result = Some(f.result.clone());
                    self.fn_body(f.body, &f.result);
                    self.result = None;
                }
                DefKind::View(v) => {
                    self.side = Side::Client;
                    self.nodes(&v.nodes);
                }
                DefKind::App(_) | DefKind::Enum(_) | DefKind::Var(_) | DefKind::Style => {}
            }
        }
    }

    // Diagnostics.

    fn span(&self, e: ExprId) -> Span {
        self.p
            .expr(e)
            .map_or(Span::point(ostrel_core::FileId::from_raw(0), 0), |x| x.span)
    }

    fn mismatch(&mut self, at: Span, want: &Ty, found: &Ty) {
        let message = format!(
            "expected {}, found {}",
            self.describe(want),
            self.describe(found)
        );
        self.diags
            .push(Diagnostic::error(codes::MISMATCH, at, message));
    }

    /// Reports E0304 at `e` unless `found` fits `want`.
    fn expect(&mut self, e: ExprId, want: &Ty, found: &Ty) {
        if !fits(want, found) {
            let at = self.span(e);
            self.mismatch(at, want, found);
        }
    }

    fn condition(&mut self, e: ExprId) {
        let found = self.expr(e);
        if !fits(&Ty::Bool, &found) {
            let message = format!("condition must be `Bool`, found {}", self.describe(&found));
            let at = self.span(e);
            self.diags
                .push(Diagnostic::error(codes::CONDITION_NOT_BOOL, at, message));
        }
    }

    fn describe(&self, ty: &Ty) -> String {
        match ty {
            Ty::Unit => "nothing".to_string(),
            Ty::Error => "an unknown type".to_string(),
            _ => format!("`{}`", self.name(ty)),
        }
    }

    fn name(&self, ty: &Ty) -> String {
        let def = |d: &DefId| {
            self.p
                .def(*d)
                .map_or("?".to_string(), |d| d.name.to_string())
        };
        match ty {
            Ty::Bool => "Bool".into(),
            Ty::Int => "Int".into(),
            Ty::Float => "Float".into(),
            Ty::Text => "Text".into(),
            Ty::Time => "Time".into(),
            Ty::Bytes => "Bytes".into(),
            Ty::Rank => "Rank".into(),
            Ty::Url => "Url".into(),
            Ty::Enum(d) | Ty::Ref(d) => def(d),
            // The type of `none` alone.
            Ty::Optional(t) if **t == Ty::Error => "none".into(),
            Ty::Optional(t) => format!("{}?", self.name(t)),
            Ty::Set(t) => format!("Set[{}]", self.name(t)),
            Ty::List(t) => format!("List[{}]", self.name(t)),
            Ty::Map(k, v) => format!("Map[{}, {}]", self.name(k), self.name(v)),
            Ty::Remote(t) => format!("Remote {}", self.name(t)),
            Ty::Rows(d) => format!("rows of {}", def(d)),
            Ty::Range(t) => format!("range of {}", self.name(t)),
            Ty::Unit => "nothing".into(),
            Ty::Error => "?".into(),
        }
    }

    // Statements.

    /// A function body; without branches its last expression is its result
    /// (SYNTAX 4.10).
    fn fn_body(&mut self, body: BlockId, result: &Ty) {
        let Some(block) = self.p.block(body) else {
            return;
        };
        let branches = block.stmts.iter().any(|&s| self.branches(s));
        let last = block.stmts.last().copied();
        for &s in block.stmts.iter() {
            match self.p.stmt(s).map(|st| &st.kind) {
                Some(StmtKind::Expr(e)) if Some(s) == last && !branches && *result != Ty::Unit => {
                    let found = self.expr(*e);
                    self.expect(*e, result, &found);
                }
                _ => self.stmt(s),
            }
        }
    }

    fn branches(&self, s: StmtId) -> bool {
        matches!(
            self.p.stmt(s).map(|st| &st.kind),
            Some(StmtKind::If { .. } | StmtKind::IfLet { .. } | StmtKind::For { .. })
        )
    }

    fn block(&mut self, b: BlockId) {
        if let Some(block) = self.p.block(b) {
            for &s in block.stmts.iter() {
                self.stmt(s);
            }
        }
    }

    fn stmt(&mut self, s: StmtId) {
        let Some(stmt) = self.p.stmt(s) else {
            return;
        };
        match &stmt.kind {
            StmtKind::Let { local, value } => {
                let ty = self.expr(*value);
                self.set_local(*local, ty);
            }
            StmtKind::If {
                cond,
                then_block,
                else_block,
            } => {
                self.condition(*cond);
                self.block(*then_block);
                if let Some(b) = else_block {
                    self.block(*b);
                }
            }
            StmtKind::Return(e) => {
                let found = self.expr(*e);
                match self.result.clone() {
                    Some(Ty::Unit) if !matches!(found, Ty::Unit | Ty::Error) => {
                        let message = format!(
                            "this function returns nothing, found {}",
                            self.describe(&found)
                        );
                        self.diags
                            .push(Diagnostic::error(codes::MISMATCH, stmt.span, message));
                    }
                    Some(want) => self.expect(*e, &want, &found),
                    None => {}
                }
            }
            StmtKind::Expr(e) | StmtKind::Drop(e) => {
                self.expr(*e);
            }
            StmtKind::IfLet {
                local,
                value,
                then_block,
                else_block,
            } => {
                let ty = unwrap_optional(self.expr(*value));
                self.set_local(*local, ty);
                self.block(*then_block);
                if let Some(b) = else_block {
                    self.block(*b);
                }
            }
            StmtKind::For {
                local,
                source,
                body,
            } => {
                let ty = self.item_of(*source);
                self.set_local(*local, ty);
                self.block(*body);
            }
            StmtKind::Assign { target, value } => self.assign(*target, *value),
        }
    }

    fn assign(&mut self, target: ExprId, value: ExprId) {
        let want = self.expr(target);
        let want = match self.p.expr(target).map(|e| &e.kind) {
            // A `serial` field reads as `Int?` but is written as declared.
            Some(ExprKind::Field {
                field: (m, FieldRef::Declared(f)),
                ..
            }) => self.p.field(*m, *f).map_or(Ty::Error, |f| f.ty.clone()),
            _ => want,
        };
        let found = self.expr(value);
        self.expect(value, &want, &found);
    }

    fn set_local(&mut self, local: LocalId, ty: Ty) {
        if let Some(slot) = self.locals.get_mut(local.index()) {
            *slot = Some(ty);
        }
    }

    /// The type of one item a `for` over `source` binds.
    fn item_of(&mut self, source: ExprId) -> Ty {
        let ty = self.expr(source);
        match ty {
            Ty::Rows(m) => Ty::Ref(m),
            Ty::Set(t) | Ty::List(t) | Ty::Range(t) => *t,
            _ => match self.p.expr(source).map(|e| &e.kind) {
                Some(ExprKind::Name(Res::Def(d))) => match self.p.def(*d).map(|d| &d.kind) {
                    Some(DefKind::Enum(_)) => Ty::Enum(*d),
                    Some(DefKind::Model(_)) => Ty::Ref(*d),
                    _ => Ty::Error,
                },
                _ => Ty::Error,
            },
        }
    }

    // View nodes.

    fn nodes(&mut self, nodes: &[ViewNodeId]) {
        for &n in nodes {
            self.node(n);
        }
    }

    fn node(&mut self, n: ViewNodeId) {
        let Some(node) = self.p.view_node(n) else {
            return;
        };
        match &node.kind {
            ViewNodeKind::Element(e) => self.element(e),
            ViewNodeKind::If {
                cond,
                bind,
                then_nodes,
                else_nodes,
            } => {
                match bind {
                    Some(local) => {
                        let ty = unwrap_optional(self.expr(*cond));
                        self.set_local(*local, ty);
                    }
                    None => self.condition(*cond),
                }
                self.nodes(then_nodes);
                self.nodes(else_nodes);
            }
            ViewNodeKind::For {
                local,
                source,
                nodes,
            } => {
                let ty = self.item_of(*source);
                self.set_local(*local, ty);
                self.nodes(nodes);
            }
        }
    }

    fn element(&mut self, e: &Element) {
        if let Some(v) = e.value {
            self.expr(v);
        }
        let std = match &e.name {
            ElementName::Std(name) => Some(&**name),
            ElementName::View(_) => None,
        };
        for (name, value) in e.attrs.iter() {
            let found = self.expr(*value);
            // URLs of `img` and `link` (SPEC 12.13).
            if matches!(
                (std, &**name),
                (Some("img"), "src") | (Some("link"), "href")
            ) {
                self.expect(*value, &Ty::Url, &found);
            }
        }
        for (_, cond) in e.styles.iter() {
            if let Some(c) = cond {
                self.condition(*c);
            }
        }
        for h in e.handlers.iter() {
            match h.assign {
                Some(value) => self.assign(h.action, value),
                None => {
                    self.expr(h.action);
                }
            }
        }
        self.nodes(&e.children);
    }

    // Expressions.

    fn expr(&mut self, e: ExprId) -> Ty {
        let ty = self.infer(e);
        if let Some(slot) = self.exprs.get_mut(e.index()) {
            *slot = Some(ty.clone());
        }
        ty
    }

    fn infer(&mut self, e: ExprId) -> Ty {
        let p = self.p;
        let Some(expr) = p.expr(e) else {
            return Ty::Error;
        };
        match &expr.kind {
            ExprKind::Int(_) => Ty::Int,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Str(parts) => {
                for part in parts.iter() {
                    if let StrPart::Interp(x) = part {
                        self.expr(*x);
                    }
                }
                Ty::Text
            }
            ExprKind::Name(res) => self.name_ty(*res),
            ExprKind::Word { lexical, .. } => match lexical {
                Res::Variant(d, _) => Ty::Enum(*d),
                _ => Ty::Error,
            },
            ExprKind::Unary { op, operand } => {
                let want = match op {
                    UnaryOp::Neg => Ty::Int,
                    UnaryOp::Not => Ty::Bool,
                };
                let found = self.expr(*operand);
                if *op == UnaryOp::Neg && found == Ty::Float {
                    return Ty::Float;
                }
                self.expect(*operand, &want, &found);
                want
            }
            ExprKind::Binary { op, lhs, rhs } => self.binary(*op, *lhs, *rhs),
            ExprKind::Call { callee, args } => self.call(*callee, args),
            ExprKind::NoneValue => Ty::Optional(Box::new(Ty::Error)),
            ExprKind::Me => self.user.map_or(Ty::Error, Ty::Ref),
            ExprKind::Now => Ty::Time,
            ExprKind::Signed => Ty::Bool,
            ExprKind::Field { base, field } => {
                self.expr(*base);
                self.field_ty(field.0, field.1)
            }
            ExprKind::Property { base, name } => {
                let base = self.expr(*base);
                match (&base, &**name) {
                    (Ty::Text, "len") | (Ty::Set(_) | Ty::List(_) | Ty::Map(..), "len") => Ty::Int,
                    (Ty::Text, "trim") => Ty::Text,
                    _ => Ty::Error,
                }
            }
            ExprKind::Index { base, index } => {
                let base = self.expr(*base);
                let found = self.expr(*index);
                match base {
                    Ty::Map(k, v) => {
                        self.expect(*index, &k, &found);
                        Ty::Optional(v)
                    }
                    _ => Ty::Error,
                }
            }
            ExprKind::In {
                element,
                collection,
                ..
            } => {
                // SYNTAX 4.4: a none element is not in the collection.
                let found = unwrap_optional(self.expr(*element));
                if let Ty::Set(t) | Ty::List(t) | Ty::Range(t) = self.expr(*collection) {
                    self.expect(*element, &t, &found);
                }
                Ty::Bool
            }
            ExprKind::Range { lo, hi } => {
                let l = self.expr(*lo);
                let h = self.expr(*hi);
                if l == Ty::Error {
                    Ty::Range(Box::new(h))
                } else {
                    self.expect(*hi, &l, &h);
                    Ty::Range(Box::new(l))
                }
            }
            ExprKind::Catch { value, .. } => self.expr(*value),
            ExprKind::Fallback { value, fallback } => {
                let inner = match self.expr(*value) {
                    Ty::Optional(t) | Ty::Remote(t) => *t,
                    other => other,
                };
                let found = self.expr(*fallback);
                if inner == Ty::Error {
                    found
                } else {
                    self.expect(*fallback, &inner, &found);
                    inner
                }
            }
            ExprKind::Set(items) => Ty::Set(Box::new(self.items(items))),
            ExprKind::List(items) => Ty::List(Box::new(self.items(items))),
            ExprKind::Map(entries) => {
                let keys: Vec<ExprId> = entries.iter().map(|(k, _)| *k).collect();
                let values: Vec<ExprId> = entries.iter().map(|(_, v)| *v).collect();
                let k = self.items(&keys);
                let v = self.items(&values);
                Ty::Map(Box::new(k), Box::new(v))
            }
            ExprKind::Make { model, fields } => {
                for &(f, value) in fields.iter() {
                    let want = self.p.field(*model, f).map_or(Ty::Error, |f| f.ty.clone());
                    let found = self.expr(value);
                    self.expect(value, &want, &found);
                }
                Ty::Ref(*model)
            }
            ExprKind::Query(q) => {
                if let Some(f) = q.filter {
                    self.condition(f);
                }
                if let Some((_, n)) = q.take {
                    let found = self.expr(n);
                    self.expect(n, &Ty::Int, &found);
                }
                Ty::Rows(q.model)
            }
        }
    }

    /// The common type of the elements of a literal: the first known element
    /// type, optional when any element is `none` or optional (SYNTAX 4.4),
    /// whatever its position. E0304 at each element that does not fit it.
    fn items(&mut self, items: &[ExprId]) -> Ty {
        let found: Vec<Ty> = items.iter().map(|&item| self.expr(item)).collect();
        let optional = found.iter().any(|t| matches!(t, Ty::Optional(_)));
        let inner: Vec<Ty> = found.into_iter().map(unwrap_optional).collect();
        let common = inner
            .iter()
            .find(|t| **t != Ty::Error)
            .cloned()
            .unwrap_or(Ty::Error);
        for (&item, found) in items.iter().zip(&inner) {
            self.expect(item, &common, found);
        }
        if optional {
            Ty::Optional(Box::new(common))
        } else {
            common
        }
    }

    fn name_ty(&self, res: Res) -> Ty {
        match res {
            Res::Local(l) => self
                .locals
                .get(l.index())
                .cloned()
                .flatten()
                .unwrap_or(Ty::Error),
            Res::Def(d) => match self.p.def(d).map(|x| &x.kind) {
                Some(DefKind::Var(_)) => self.vars.get(&d).cloned().unwrap_or(Ty::Error),
                _ => Ty::Error,
            },
            Res::Field(m, f) => self.field_ty(m, f),
            Res::Variant(d, _) => Ty::Enum(d),
            Res::Context(_) | Res::Std(_) | Res::Error => Ty::Error,
        }
    }

    /// The type a read of the field gives.
    fn field_ty(&self, model: DefId, field: FieldRef) -> Ty {
        match field {
            FieldRef::Declared(f) => match self.p.field(model, f) {
                // `serial` is `Int?` until synced (SYNTAX 4.2).
                Some(f) if matches!(f.mode, FieldMode::Serial(_)) => {
                    Ty::Optional(Box::new(f.ty.clone()))
                }
                Some(f) => f.ty.clone(),
                None => Ty::Error,
            },
            FieldRef::Implicit(ImplicitField::Id) => Ty::Ref(model),
            FieldRef::Implicit(ImplicitField::Made | ImplicitField::Changed) => Ty::Time,
            FieldRef::Implicit(ImplicitField::Author) => self.user.map_or(Ty::Error, Ty::Ref),
            FieldRef::Implicit(ImplicitField::Pending | ImplicitField::Rejected) => Ty::Bool,
        }
    }

    fn binary(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Ty {
        let l = self.expr(lhs);
        let r = self.expr(rhs);
        match op {
            BinaryOp::And | BinaryOp::Or => {
                self.expect(lhs, &Ty::Bool, &l);
                self.expect(rhs, &Ty::Bool, &r);
                Ty::Bool
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                if !fits(&l, &r) && !fits(&r, &l) {
                    let at = self.span(rhs);
                    self.mismatch(at, &l, &r);
                }
                Ty::Bool
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                // An optional operand is reported at the operator (AC-33 a),
                // which the HIR does not record yet.
                if plain(&l) && plain(&r) && l != r {
                    let at = self.span(rhs);
                    self.mismatch(at, &l, &r);
                }
                Ty::Bool
            }
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => {
                self.arithmetic(lhs, &l, rhs, &r)
            }
        }
    }

    fn arithmetic(&mut self, lhs: ExprId, l: &Ty, rhs: ExprId, r: &Ty) -> Ty {
        match (l, r) {
            (Ty::Int, Ty::Int) => Ty::Int,
            (Ty::Float, Ty::Float) => Ty::Float,
            // `Time` arithmetic and `+` on `Text` (E0300 at the operator)
            // are not decided here.
            (Ty::Time, _) | (_, Ty::Time) | (Ty::Text, _) | (_, Ty::Text) => Ty::Error,
            (Ty::Error, _) | (_, Ty::Error) => Ty::Error,
            (Ty::Int | Ty::Float, _) => {
                self.expect(rhs, l, r);
                Ty::Error
            }
            _ => {
                self.expect(lhs, &Ty::Int, l);
                Ty::Error
            }
        }
    }

    fn call(&mut self, callee: ExprId, args: &[ExprId]) -> Ty {
        let p = self.p;
        let kind = p.expr(callee).map(|e| &e.kind);
        match kind {
            Some(ExprKind::Name(Res::Def(d))) => {
                if let Some(slot) = self.exprs.get_mut(callee.index()) {
                    *slot = Some(Ty::Error);
                }
                match p.def(*d) {
                    Some(def) => match &def.kind {
                        DefKind::Fn(f) => {
                            let params: Vec<Ty> = f
                                .params
                                .iter()
                                .map(|&l| {
                                    p.local(l).and_then(|l| l.ty.clone()).unwrap_or(Ty::Error)
                                })
                                .collect();
                            self.args(callee, &def.name, &params, args);
                            match (f.declared, self.side) {
                                (Some(Placement::Server), Side::Client) => {
                                    Ty::Remote(Box::new(f.result.clone()))
                                }
                                (Some(Placement::Server), Side::Open) => Ty::Error,
                                _ => f.result.clone(),
                            }
                        }
                        _ => self.unknown_call(args),
                    },
                    None => self.unknown_call(args),
                }
            }
            // `Url.parse(t)` (SPEC 12.13).
            Some(ExprKind::Property { base, name })
                if &**name == "parse"
                    && matches!(
                        p.expr(*base).map(|b| &b.kind),
                        Some(ExprKind::Name(Res::Std(StdName::Url)))
                    ) =>
            {
                if let Some(slot) = self.exprs.get_mut(callee.index()) {
                    *slot = Some(Ty::Error);
                }
                self.expr(*base);
                self.args(callee, "Url.parse", &[Ty::Text], args);
                Ty::Optional(Box::new(Ty::Url))
            }
            _ => {
                self.expr(callee);
                self.unknown_call(args)
            }
        }
    }

    fn unknown_call(&mut self, args: &[ExprId]) -> Ty {
        for &a in args {
            self.expr(a);
        }
        Ty::Error
    }

    fn args(&mut self, callee: ExprId, name: &str, params: &[Ty], args: &[ExprId]) {
        if params.len() != args.len() {
            let n = params.len();
            let plural = if n == 1 { "" } else { "s" };
            let message = format!("`{name}` takes {n} argument{plural}, found {}", args.len());
            let at = self.span(callee);
            self.diags
                .push(Diagnostic::error(codes::ARITY, at, message));
            for &a in args {
                self.expr(a);
            }
            return;
        }
        for (&a, want) in args.iter().zip(params) {
            let found = self.expr(a);
            self.expect(a, want, &found);
        }
    }
}

/// True if a value of type `found` may stand where `want` is required.
/// [`Ty::Error`] fits everything, so an earlier fault causes no further
/// diagnostic.
fn fits(want: &Ty, found: &Ty) -> bool {
    match (want, found) {
        (Ty::Error, _) | (_, Ty::Error) => true,
        (Ty::Optional(w), Ty::Optional(f)) => fits(w, f),
        (Ty::Optional(w), f) => fits(w, f),
        (Ty::Set(w), Ty::Set(f)) | (Ty::List(w), Ty::List(f)) | (Ty::Range(w), Ty::Range(f)) => {
            fits(w, f)
        }
        (Ty::Remote(w), Ty::Remote(f)) => fits(w, f),
        (Ty::Map(wk, wv), Ty::Map(fk, fv)) => fits(wk, fk) && fits(wv, fv),
        _ => want == found,
    }
}

/// A known type that is no optional, remote or unknown value.
fn plain(ty: &Ty) -> bool {
    !matches!(
        ty,
        Ty::Optional(_) | Ty::Remote(_) | Ty::Error | Ty::Unit | Ty::Rows(_) | Ty::Range(_)
    )
}

fn unwrap_optional(ty: Ty) -> Ty {
    match ty {
        Ty::Optional(t) => *t,
        other => other,
    }
}

fn id(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;
