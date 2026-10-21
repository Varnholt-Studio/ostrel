//! Type checker of the HIR (AC-29, `docs/types.md`).
//!
//! [`check`] gives every expression, local and `var` a type and reports
//! values whose type differs from the one their place requires (E0304),
//! conditions that are not `Bool` (E0301) and calls with the wrong number of
//! arguments (E0302). Codes and message forms are those of the v0.1 checker
//! in the crate root.
//!
//! It also decides the words of D142 by their expected type, reports the
//! `call` rule of an elevated function that does not bind `me` to row data
//! (E0306), a constant range whose lower bound is above its upper bound
//! (E0311, SPEC 12.11) and a constant negative query count (E0312, SPEC
//! 12.10). A missing `return` (E0303, E0305) is reported by `resolve`, which
//! knows where the function name stands.
//!
//! The checker reports only what it can prove. Where the type of an
//! expression is not decided yet (properties other than `len` and `trim`,
//! methods of collections, contextual names, std names other than
//! `Url.parse`, attribute values of std elements other than `img src` and
//! `link href`), the expression gets [`Ty::Error`] and nothing is reported,
//! so a valid program never fails here. Not yet covered: `E0300` and the
//! ordering of optionals (both stand at the operator, which the HIR does not
//! record) and placement.

use std::collections::HashMap;

use ostrel_core::{Code, Diagnostic, Span};
use ostrel_syntax::ast::{BinaryOp, UnaryOp};

use crate::codes;
use crate::hir::{
    BlockId, DefId, DefKind, Element, ElementName, ExprId, ExprKind, FieldMode, FieldRef,
    ImplicitField, LocalId, Placement, Program, Res, StdName, StmtId, StmtKind, StrPart, Ty,
    ViewNodeId, ViewNodeKind,
};

/// The `call` rule of an `as system` function binds `me` to nothing (D124,
/// SPEC 12.6).
pub(crate) const UNBOUND_ME: Code = Code::new(306);
/// A range with constant bounds whose lower bound is above its upper bound
/// (SPEC 12.11).
pub(crate) const EMPTY_RANGE: Code = Code::new(311);
/// A constant negative count after `limit` or `last` (SPEC 12.10).
pub(crate) const NEGATIVE_COUNT: Code = Code::new(312);

/// Checks types and fills [`crate::hir::Expr::ty`], [`crate::hir::Local::ty`]
/// and [`crate::hir::VarDef::ty`].
pub(crate) fn check(program: &mut Program) -> Vec<Diagnostic> {
    let mut cx = Cx::new(program);
    cx.run();
    let Cx {
        exprs,
        locals,
        vars,
        words,
        diags,
        ..
    } = cx;
    for (e, res) in words {
        if let Some(expr) = program.exprs.get_mut(e.index()) {
            expr.kind = ExprKind::Name(res);
        }
    }
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
    /// Words of D142 and what they mean; each becomes [`ExprKind::Name`].
    words: Vec<(ExprId, Res)>,
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
            words: Vec::new(),
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
                let ty = match (&v.ty, v.value) {
                    (Some(want), Some(e)) => {
                        self.value(e, want);
                        want.clone()
                    }
                    (Some(want), None) => want.clone(),
                    (None, Some(e)) => self.expr(e),
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
                            self.value(e, &f.ty);
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
                        if f.elevated.is_some() && !self.binds_me(rule.cond, &f.params) {
                            let at = self.span(rule.cond);
                            self.diags.push(Diagnostic::error(
                                UNBOUND_ME,
                                at,
                                "the call rule of an as system function must compare me with a \
                                 parameter or with row data"
                                    .to_string(),
                            ));
                        }
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
                    self.value(*e, result);
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
            StmtKind::Return(e) => match self.result.clone() {
                Some(Ty::Unit) => {
                    let found = self.expr(*e);
                    if !matches!(found, Ty::Unit | Ty::Error) {
                        let message = format!(
                            "this function returns nothing, found {}",
                            self.describe(&found)
                        );
                        self.diags
                            .push(Diagnostic::error(codes::MISMATCH, stmt.span, message));
                    }
                }
                Some(want) => {
                    self.value(*e, &want);
                }
                None => {
                    self.expr(*e);
                }
            },
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
        self.value(value, &want);
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
            // URLs of `img` and `link` (SPEC 12.13); the other attribute
            // types are not declared yet.
            let want = matches!(
                (std, &**name),
                (Some("img"), "src") | (Some("link"), "href")
            )
            .then_some(Ty::Url);
            let found = self.word(*value, want.as_ref(), true);
            if let Some(want) = &want {
                self.expect(*value, want, &found);
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
            ExprKind::Word { .. } => self.word(e, None, false),
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
                // SYNTAX 4.4: a none element is not in the collection; a map
                // holds its keys.
                let item = match self.expr(*collection) {
                    Ty::Set(t) | Ty::List(t) | Ty::Range(t) | Ty::Map(t, _) => Some(*t),
                    _ => None,
                };
                let found = unwrap_optional(self.word(*element, item.as_ref(), false));
                if let Some(t) = &item {
                    self.expect(*element, t, &found);
                }
                Ty::Bool
            }
            ExprKind::Range { lo, hi } => {
                if let (Some(a), Some(b)) = (self.constant(*lo), self.constant(*hi))
                    && a > b
                {
                    let at = self.span(*lo);
                    let message = format!(
                        "the range {a}..{b} holds no value: its lower bound is above its upper \
                         bound"
                    );
                    self.diags.push(Diagnostic::error(EMPTY_RANGE, at, message));
                }
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
                if inner == Ty::Error {
                    self.expr(*fallback)
                } else {
                    // A literal fallback is checked element by element.
                    self.value(*fallback, &inner);
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
                    self.value(value, &want);
                }
                Ty::Ref(*model)
            }
            ExprKind::Query(q) => {
                if let Some(f) = q.filter {
                    self.condition(f);
                }
                if let Some((kind, n)) = q.take {
                    let found = self.expr(n);
                    self.expect(n, &Ty::Int, &found);
                    if let Some(v) = self.constant(n).filter(|v| *v < 0) {
                        let at = self.span(n);
                        let message = format!(
                            "the count of `{}` is {v}; a query count is 0 or more",
                            kind.as_str()
                        );
                        self.diags
                            .push(Diagnostic::error(NEGATIVE_COUNT, at, message));
                    }
                }
                Ty::Rows(q.model)
            }
        }
    }

    /// The common type of the elements of a literal without an expected
    /// type: the [`join`] of all element types, so it does not depend on
    /// their order (SYNTAX 4.4). Elements without a join give one E0304, at
    /// the first element that does not join the ones before it, and the
    /// literal gets [`Ty::Error`].
    fn items(&mut self, items: &[ExprId]) -> Ty {
        let mut common = Ty::Error;
        let mut rest = items.iter();
        while let Some(&item) = rest.next() {
            let found = self.expr(item);
            match join(&common, &found) {
                Some(ty) => common = ty,
                None => {
                    let at = self.span(item);
                    self.mismatch(at, &common, &found);
                    for &rest in rest {
                        self.expr(rest);
                    }
                    return Ty::Error;
                }
            }
        }
        for &item in items {
            self.retype(item, &common);
        }
        common
    }

    /// Gives a literal element (`none` or an inner collection literal) the
    /// common type of its literal, so each expression holds the type it is
    /// used with.
    fn retype(&mut self, e: ExprId, ty: &Ty) {
        let p = self.p;
        match (p.expr(e).map(|x| &x.kind), ty) {
            (Some(ExprKind::NoneValue), Ty::Optional(_)) => {}
            (Some(ExprKind::Set(items)), Ty::Set(t))
            | (Some(ExprKind::List(items)), Ty::List(t)) => {
                for &item in items.iter() {
                    self.retype(item, t);
                }
            }
            (Some(ExprKind::Map(entries)), Ty::Map(k, v)) => {
                for &(a, b) in entries.iter() {
                    self.retype(a, k);
                    self.retype(b, v);
                }
            }
            _ => return,
        }
        if let Some(slot) = self.exprs.get_mut(e.index()) {
            *slot = Some(ty.clone());
        }
    }

    /// Checks `e` where a value of type `want` is required and gives the type
    /// of `e`. A collection literal takes its element types from `want`, so
    /// each element is checked on its own (E0304 at the element).
    fn value(&mut self, e: ExprId, want: &Ty) -> Ty {
        let p = self.p;
        let kind = p.expr(e).map(|x| &x.kind);
        let inner = match want {
            Ty::Optional(t) => &**t,
            t => t,
        };
        let ty = match (kind, inner) {
            (Some(ExprKind::Set(items)), Ty::Set(w))
            | (Some(ExprKind::List(items)), Ty::List(w))
                if **w != Ty::Error =>
            {
                for &item in items.iter() {
                    self.value(item, w);
                }
                inner.clone()
            }
            (Some(ExprKind::Map(entries)), Ty::Map(wk, wv))
                if **wk != Ty::Error && **wv != Ty::Error =>
            {
                for &(k, v) in entries.iter() {
                    self.value(k, wk);
                    self.value(v, wv);
                }
                inner.clone()
            }
            _ => {
                let found = self.word(e, Some(want), false);
                self.expect(e, want, &found);
                if matches!(kind, Some(ExprKind::NoneValue)) && fits(want, &found) {
                    self.retype(e, want);
                }
                return found;
            }
        };
        if let Some(slot) = self.exprs.get_mut(e.index()) {
            *slot = Some(ty.clone());
        }
        ty
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
        // A word takes its type from the other operand (D142).
        let (l, r) = match (self.is_word(lhs), self.is_word(rhs)) {
            (true, false) => {
                let r = self.expr(rhs);
                (self.word(lhs, Some(&r), false), r)
            }
            (false, true) => {
                let l = self.expr(lhs);
                let r = self.word(rhs, Some(&l), false);
                (l, r)
            }
            _ => (self.expr(lhs), self.expr(rhs)),
        };
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

    fn is_word(&self, e: ExprId) -> bool {
        matches!(self.p.expr(e).map(|x| &x.kind), Some(ExprKind::Word { .. }))
    }

    /// Decides a word of D142 by the expected type `want`: the enum of
    /// `want` with a variant of that name wins, else what the name binds
    /// where it stands, else E0200 at the word naming the expected type or
    /// the enums that declare the name. Any other expression is inferred.
    /// An attribute word that binds nothing and has neither an expected type
    /// nor a candidate enum stays undecided (`attribute`): the attribute
    /// types of the std elements are not declared yet.
    fn word(&mut self, e: ExprId, want: Option<&Ty>, attribute: bool) -> Ty {
        let p = self.p;
        let Some(ExprKind::Word { name, lexical }) = p.expr(e).map(|x| &x.kind) else {
            return self.expr(e);
        };
        let want = want.map(|w| match w {
            Ty::Optional(t) => &**t,
            t => t,
        });
        let candidates: Vec<(DefId, u32)> = p
            .defs
            .iter()
            .enumerate()
            .filter_map(|(i, d)| match &d.kind {
                DefKind::Enum(vs) => vs
                    .iter()
                    .position(|v| v == name)
                    .map(|v| (DefId(id(i)), id(v))),
                _ => None,
            })
            .collect();
        let chosen = match want {
            Some(Ty::Enum(d)) => candidates.iter().find(|(c, _)| c == d).copied(),
            _ => None,
        };
        let res = match chosen {
            Some((d, v)) => Res::Variant(d, v),
            None if *lexical != Res::Error => *lexical,
            None if attribute && want.is_none() && candidates.is_empty() => {
                if let Some(slot) = self.exprs.get_mut(e.index()) {
                    *slot = Some(Ty::Error);
                }
                return Ty::Error;
            }
            None => {
                let enums: Vec<String> = candidates
                    .iter()
                    .map(|(d, _)| format!("`{}`", self.name(&Ty::Enum(*d))))
                    .collect();
                let message = match (want.filter(|w| **w != Ty::Error), enums.is_empty()) {
                    (Some(w), true) => {
                        format!("unknown name `{name}`; expected {}", self.describe(w))
                    }
                    (Some(w), false) => format!(
                        "`{name}` is no value of {}; it is a variant of {}",
                        self.describe(w),
                        enums.join(", ")
                    ),
                    (None, true) => format!("unknown name `{name}`"),
                    (None, false) => format!(
                        "`{name}` is a variant of {}; the expected type does not say which",
                        enums.join(" and ")
                    ),
                };
                let at = self.span(e);
                self.diags
                    .push(Diagnostic::error(codes::UNKNOWN_NAME, at, message));
                Res::Error
            }
        };
        self.words.push((e, res));
        let ty = self.name_ty(res);
        if let Some(slot) = self.exprs.get_mut(e.index()) {
            *slot = Some(ty.clone());
        }
        ty
    }

    /// The value of an integer literal, also with unary `-`.
    fn constant(&self, e: ExprId) -> Option<i64> {
        match &self.p.expr(e)?.kind {
            ExprKind::Int(v) => Some(*v),
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => match self.p.expr(*operand)?.kind {
                ExprKind::Int(v) => v.checked_neg(),
                _ => None,
            },
            _ => None,
        }
    }

    /// True if some atom of the `call` rule `cond` (a comparison or an
    /// `in`, under `and`, `or` and `not`) names `me` and also a parameter,
    /// directly or through a hop or an index (D124).
    fn binds_me(&self, cond: ExprId, params: &[LocalId]) -> bool {
        let Some(expr) = self.p.expr(cond) else {
            return false;
        };
        match &expr.kind {
            ExprKind::Binary {
                op: BinaryOp::And | BinaryOp::Or,
                lhs,
                rhs,
            } => self.binds_me(*lhs, params) || self.binds_me(*rhs, params),
            ExprKind::Unary {
                op: UnaryOp::Not,
                operand,
            } => self.binds_me(*operand, params),
            ExprKind::Binary { .. } | ExprKind::In { .. } => {
                self.mentions(cond, &|k| matches!(k, ExprKind::Me))
                    && self.mentions(
                        cond,
                        &|k| matches!(k, ExprKind::Name(Res::Local(l)) if params.contains(l)),
                    )
            }
            _ => false,
        }
    }

    /// True if `e` or an expression inside it satisfies `is`.
    fn mentions(&self, e: ExprId, is: &dyn Fn(&ExprKind) -> bool) -> bool {
        let Some(expr) = self.p.expr(e) else {
            return false;
        };
        is(&expr.kind)
            || children(&expr.kind)
                .into_iter()
                .any(|c| self.mentions(c, is))
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
            self.value(a, want);
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

/// The least type both `a` and `b` fit, if there is one: `none` with `T`
/// gives `T?`, and collections join element by element. [`Ty::Error`] joins
/// with everything. Commutative and associative, so the join of a literal's
/// elements does not depend on their order.
fn join(a: &Ty, b: &Ty) -> Option<Ty> {
    let both = |x: &Ty, y: &Ty| join(x, y).map(Box::new);
    Some(match (a, b) {
        (Ty::Error, t) | (t, Ty::Error) => t.clone(),
        (Ty::Optional(x), Ty::Optional(y)) => Ty::Optional(both(x, y)?),
        (Ty::Optional(x), t) | (t, Ty::Optional(x)) => Ty::Optional(both(x, t)?),
        (Ty::Set(x), Ty::Set(y)) => Ty::Set(both(x, y)?),
        (Ty::List(x), Ty::List(y)) => Ty::List(both(x, y)?),
        (Ty::Map(xk, xv), Ty::Map(yk, yv)) => Ty::Map(both(xk, yk)?, both(xv, yv)?),
        (Ty::Range(x), Ty::Range(y)) => Ty::Range(both(x, y)?),
        (Ty::Remote(x), Ty::Remote(y)) => Ty::Remote(both(x, y)?),
        _ if a == b => a.clone(),
        _ => return None,
    })
}

/// A known type that is no optional, remote or unknown value.
fn plain(ty: &Ty) -> bool {
    !matches!(
        ty,
        Ty::Optional(_) | Ty::Remote(_) | Ty::Error | Ty::Unit | Ty::Rows(_) | Ty::Range(_)
    )
}

/// The expressions directly inside an expression of this kind.
fn children(kind: &ExprKind) -> Vec<ExprId> {
    match kind {
        ExprKind::Str(parts) => parts
            .iter()
            .filter_map(|p| match p {
                StrPart::Interp(x) => Some(*x),
                StrPart::Text(_) => None,
            })
            .collect(),
        ExprKind::Unary { operand, .. } => vec![*operand],
        ExprKind::Binary { lhs, rhs, .. } => vec![*lhs, *rhs],
        ExprKind::Call { callee, args } => std::iter::once(*callee)
            .chain(args.iter().copied())
            .collect(),
        ExprKind::Field { base, .. } | ExprKind::Property { base, .. } => vec![*base],
        ExprKind::Index { base, index } => vec![*base, *index],
        ExprKind::In {
            element,
            collection,
            ..
        } => vec![*element, *collection],
        ExprKind::Range { lo, hi } => vec![*lo, *hi],
        ExprKind::Catch { value, .. } => vec![*value],
        ExprKind::Fallback { value, fallback } => vec![*value, *fallback],
        ExprKind::Set(items) | ExprKind::List(items) => items.to_vec(),
        ExprKind::Map(entries) => entries.iter().flat_map(|&(k, v)| [k, v]).collect(),
        ExprKind::Make { fields, .. } => fields.iter().map(|&(_, v)| v).collect(),
        ExprKind::Query(q) => q.filter.into_iter().chain(q.take.map(|(_, n)| n)).collect(),
        _ => Vec::new(),
    }
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
