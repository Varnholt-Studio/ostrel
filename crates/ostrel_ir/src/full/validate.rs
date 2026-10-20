//! The placement and authority contract of the IR, checked by
//! [`Program::validate`](super::Program::validate).
//!
//! The rules are the ones a backend relies on without checking them again:
//! nothing crosses the placement split except a remote call, `as system` power
//! stays inside elevated code, predicates stay pure, every server function has
//! its prologue, and a `make` names its fields exactly as the wire op does.

use ostrel_core::Span;

use super::query::{Filter, Path, QueryPlan, See};
use super::view::{ForSource, Thunk, ViewNode, ViewNodeKind};
use super::{
    Auth, FieldMode, FieldRef, FnKind, FuncId, Function, ImplicitField, InstKind, Placement,
    Program, RuleOp, Ty,
};

/// One broken rule of the contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Problem {
    /// A function, model, rule or var id points outside its list.
    UnknownId,
    /// A `Call` crosses the placement split: shared code calls a placed
    /// function, or one side calls the other (ARCHITECTURE 4.3).
    CrossPlacementCall,
    /// A `RemoteCall` that is not from client code to a server source
    /// function.
    BadRemoteCall,
    /// A client only instruction (`RemoteCall`, `Settle`, `LoadVar`,
    /// `StoreVar`, a query on the replica) outside client code.
    ClientOnly,
    /// A server only instruction (bulk operations, a query under a `see`
    /// rule) outside server code.
    ServerOnly,
    /// Shared code that reads or writes rows or runs a query; shared code is
    /// pure (ARCHITECTURE 4.3). Predicates may read rows under [`Auth::Rule`].
    NotPure,
    /// [`Auth::System`] or [`See::System`] outside elevated code (G13, D34).
    SystemOutsideElevated,
    /// [`Auth::Rule`], [`InstKind::RuleField`] or [`InstKind::RuleDelta`]
    /// outside a predicate, or a predicate that reads rows under another
    /// [`Auth`].
    RuleOutsidePredicate,
    /// A write inside a predicate.
    WriteInPredicate,
    /// A predicate that is not shared or has no `Bool` result.
    BadPredicate,
    /// A server source function without prologue, or a prologue anywhere
    /// else.
    PrologueMismatch,
    /// An elevated function that is not on the server, or an elevated source
    /// function without `call` rule (D34).
    BadElevation,
    /// A `Call` from code that is not elevated to an elevated function
    /// without prologue (a function derived from `as system` code). Its body
    /// would run with [`Auth::System`] without the `call` rule and the audit
    /// entry of an elevated entry; elevation is entered only through the
    /// prologue of an elevated source function (D98, D140 A3).
    ElevatedWithoutEntry,
    /// A prologue that resolves a local that is not a model reference
    /// parameter, or whose `call` rule is not a `call` rule.
    BadPrologue,
    /// The `see` of a bulk operation does not match its [`Auth`], or its row
    /// function is not a derived server function with the same elevation.
    BadBulk,
    /// A `make` that does not name every declared field except `serial`
    /// fields exactly once, in field order (D116).
    IncompleteMake,
    /// `pending` or `rejected` read by code that runs on the server, also in
    /// a filter of a server query or in a `see` rule (ARCHITECTURE 5.1).
    ClientFieldOnServer,
    /// A filter [`Path`] that reads the implicit `id` (that is
    /// [`Filter::RowId`]), or a hop through a field that is neither declared
    /// nor the implicit `author`.
    BadPath,
    /// A rule whose predicate is not a [`FnKind::Predicate`] function.
    RuleWithoutPredicate,
    /// A view thunk, handler, field default or `var` initialiser that does
    /// not run on the client.
    NotOnClient,
}

/// Where the contract is broken and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Violation {
    /// The function, when the violation is inside one.
    pub func: Option<FuncId>,
    /// The instruction or declaration.
    pub span: Span,
    /// What is wrong.
    pub problem: Problem,
}

pub(super) fn validate(program: &Program) -> Vec<Violation> {
    let mut v = Validator {
        program,
        out: Vec::new(),
    };
    v.rules();
    v.models_and_vars();
    for view in &program.views {
        v.view_nodes(&view.nodes);
    }
    for (index, function) in program.functions.iter().enumerate() {
        v.function(FuncId(index as u32), function);
    }
    v.out
}

struct Validator<'p> {
    program: &'p Program,
    out: Vec<Violation>,
}

impl Validator<'_> {
    fn report(&mut self, func: Option<FuncId>, span: Span, problem: Problem) {
        self.out.push(Violation {
            func,
            span,
            problem,
        });
    }

    fn rules(&mut self) {
        for rule in &self.program.rules {
            match self.program.function(rule.pred) {
                Some(f) if f.kind == FnKind::Predicate => {}
                Some(_) => self.report(None, rule.span, Problem::RuleWithoutPredicate),
                None => self.report(None, rule.span, Problem::UnknownId),
            }
        }
    }

    fn on_client(&mut self, func: FuncId, span: Span) {
        match self.program.function(func) {
            Some(f) if f.placement != Placement::Server => {}
            Some(_) => self.report(None, span, Problem::NotOnClient),
            None => self.report(None, span, Problem::UnknownId),
        }
    }

    fn models_and_vars(&mut self) {
        let program = self.program;
        for model in &program.models {
            if let Some(see) = &model.see {
                let mut problems = Vec::new();
                filter_problems(see, true, &mut problems);
                for problem in problems {
                    self.report(None, model.span, problem);
                }
            }
            for rule in &model.rules {
                if program.rule(*rule).is_none() {
                    self.report(None, model.span, Problem::UnknownId);
                }
            }
            for field in &model.fields {
                if let Some(default) = field.default {
                    self.on_client(default, field.span);
                }
            }
        }
        for var in &program.vars {
            if let Some(init) = var.init {
                self.on_client(init, var.span);
            }
        }
    }

    fn thunk(&mut self, thunk: &Thunk, span: Span) {
        self.on_client(thunk.func, span);
    }

    fn view_nodes(&mut self, nodes: &[ViewNode]) {
        for node in nodes {
            match &node.kind {
                ViewNodeKind::Element(element) => {
                    let all = element
                        .value
                        .iter()
                        .chain(element.attrs.iter().map(|(_, t)| t))
                        .chain(element.styles.iter().filter_map(|(_, t)| t.as_ref()))
                        .chain(element.handlers.iter().map(|h| &h.action));
                    for thunk in all {
                        self.thunk(thunk, node.span);
                    }
                    self.view_nodes(&element.children);
                }
                ViewNodeKind::Use { args, .. } => {
                    for thunk in args {
                        self.thunk(thunk, node.span);
                    }
                }
                ViewNodeKind::If {
                    cond,
                    then_nodes,
                    else_nodes,
                    ..
                } => {
                    self.thunk(cond, node.span);
                    self.view_nodes(then_nodes);
                    self.view_nodes(else_nodes);
                }
                ViewNodeKind::For { source, nodes, .. } => {
                    match source {
                        ForSource::Live { plan, .. } => {
                            if plan.see != See::Replica {
                                self.report(None, node.span, Problem::ClientOnly);
                            }
                            let mut problems = Vec::new();
                            plan_problems(plan, false, &mut problems);
                            for problem in problems {
                                self.report(None, node.span, problem);
                            }
                        }
                        ForSource::Value(thunk) => self.thunk(thunk, node.span),
                    }
                    self.view_nodes(nodes);
                }
            }
        }
    }

    fn function(&mut self, id: FuncId, f: &Function) {
        let here = Some(id);
        let server_source = f.kind == FnKind::Source && f.placement == Placement::Server;
        if server_source != f.prologue.is_some() {
            self.report(here, f.span, Problem::PrologueMismatch);
        }
        if f.elevated
            && (f.placement != Placement::Server
                || (f.kind == FnKind::Source
                    && f.prologue.as_ref().is_some_and(|p| p.call_rule.is_none())))
        {
            self.report(here, f.span, Problem::BadElevation);
        }
        if f.kind == FnKind::Predicate && (f.placement != Placement::Shared || f.result != Ty::Bool)
        {
            self.report(here, f.span, Problem::BadPredicate);
        }
        if let Some(prologue) = &f.prologue {
            let resolve_ok = prologue
                .resolve
                .iter()
                .all(|local| local.0 < f.params && matches!(f.local_ty(*local), Some(Ty::Ref(_))));
            let rule_ok = prologue.call_rule.is_none_or(|rule| {
                self.program
                    .rule(rule)
                    .is_some_and(|r| r.op == RuleOp::Call)
            });
            if !resolve_ok || !rule_ok {
                self.report(here, f.span, Problem::BadPrologue);
            }
        }
        for block in &f.blocks {
            for inst in &block.insts {
                for problem in self.inst(f, &inst.kind) {
                    self.report(here, inst.span, problem);
                }
            }
        }
    }

    /// The problems of one instruction of `f`.
    fn inst(&self, f: &Function, kind: &InstKind) -> Vec<Problem> {
        let mut out = Vec::new();
        let program = self.program;
        let predicate = f.kind == FnKind::Predicate;
        let pure = f.placement == Placement::Shared;
        let client = f.placement == Placement::Client;
        let server = f.placement == Placement::Server;
        let auth = |auth: Auth, write: bool, out: &mut Vec<Problem>| {
            if write && predicate {
                out.push(Problem::WriteInPredicate);
            } else if pure && !(predicate && auth == Auth::Rule) {
                out.push(Problem::NotPure);
            }
            match auth {
                Auth::System if !f.elevated => out.push(Problem::SystemOutsideElevated),
                Auth::Rule if !predicate || write => out.push(Problem::RuleOutsidePredicate),
                Auth::Caller if predicate => out.push(Problem::RuleOutsidePredicate),
                _ => {}
            }
        };
        let see = |see: &See, out: &mut Vec<Problem>| match see {
            See::Replica if !client => out.push(Problem::ClientOnly),
            See::Rule(_) | See::DenyAll if !server => out.push(Problem::ServerOnly),
            See::System if !server => out.push(Problem::ServerOnly),
            See::System if !f.elevated => out.push(Problem::SystemOutsideElevated),
            _ => {}
        };
        let client_field = |field: &FieldRef| {
            !client && matches!(field, FieldRef::Implicit(i) if i.is_client_only())
        };
        match kind {
            InstKind::Call { func, .. } => match program.function(*func) {
                None => out.push(Problem::UnknownId),
                Some(callee) => {
                    let ok = callee.placement == Placement::Shared
                        || (callee.placement == f.placement && !pure);
                    if !ok {
                        out.push(Problem::CrossPlacementCall);
                    }
                    // An elevated function off the server is already
                    // `BadElevation`; it is not reported twice.
                    if callee.elevated
                        && callee.placement == Placement::Server
                        && callee.prologue.is_none()
                        && !f.elevated
                    {
                        out.push(Problem::ElevatedWithoutEntry);
                    }
                }
            },
            InstKind::RemoteCall { func, .. } => {
                if !client {
                    out.push(Problem::ClientOnly);
                }
                match program.function(*func) {
                    None => out.push(Problem::UnknownId),
                    Some(callee)
                        if callee.placement == Placement::Server
                            && callee.kind == FnKind::Source => {}
                    Some(_) => out.push(Problem::BadRemoteCall),
                }
            }
            InstKind::Settle { .. } | InstKind::LoadVar { .. } | InstKind::StoreVar { .. }
                if !client =>
            {
                out.push(Problem::ClientOnly);
            }
            InstKind::RuleField { field, .. } => {
                if !predicate {
                    out.push(Problem::RuleOutsidePredicate);
                }
                if client_field(field) {
                    out.push(Problem::ClientFieldOnServer);
                }
            }
            InstKind::RuleDelta { .. } if !predicate => {
                out.push(Problem::RuleOutsidePredicate);
            }
            InstKind::GetField {
                model,
                field,
                auth: a,
                ..
            } => {
                auth(*a, false, &mut out);
                if program.model(*model).is_none() {
                    out.push(Problem::UnknownId);
                }
                if client_field(field) {
                    out.push(Problem::ClientFieldOnServer);
                }
            }
            InstKind::Make {
                model,
                fields,
                auth: a,
                ..
            } => {
                auth(*a, true, &mut out);
                match program.model(*model) {
                    None => out.push(Problem::UnknownId),
                    Some(m) => {
                        let expected = m
                            .fields
                            .iter()
                            .enumerate()
                            .filter(|(_, field)| !matches!(field.mode, FieldMode::Serial(_)))
                            .map(|(i, _)| i as u32);
                        let named = fields.iter().map(|(field, _)| field.0);
                        if !expected.eq(named) {
                            out.push(Problem::IncompleteMake);
                        }
                    }
                }
            }
            InstKind::SetField { model, auth: a, .. }
            | InstKind::SetAdd { model, auth: a, .. }
            | InstKind::SetRemove { model, auth: a, .. }
            | InstKind::MapPut { model, auth: a, .. }
            | InstKind::MapRemove { model, auth: a, .. }
            | InstKind::Drop { model, auth: a, .. } => {
                auth(*a, true, &mut out);
                if program.model(*model).is_none() {
                    out.push(Problem::UnknownId);
                }
            }
            InstKind::Query { plan, .. } => {
                if pure {
                    out.push(Problem::NotPure);
                }
                see(&plan.see, &mut out);
                plan_problems(plan, !client, &mut out);
            }
            InstKind::BulkDrop { plan, auth: a } | InstKind::BulkEdit { plan, auth: a, .. } => {
                if !server {
                    out.push(Problem::ServerOnly);
                }
                auth(*a, true, &mut out);
                see(&plan.see, &mut out);
                plan_problems(plan, true, &mut out);
                let see_fits = match a {
                    Auth::System => plan.see == See::System,
                    Auth::Caller => matches!(plan.see, See::Rule(_) | See::DenyAll),
                    Auth::Rule => false,
                };
                let row_fn_fits = match kind {
                    InstKind::BulkEdit { row_fn, .. } => {
                        program.function(*row_fn).is_some_and(|r| {
                            r.placement == Placement::Server
                                && r.kind == FnKind::Derived
                                && r.elevated == (*a == Auth::System)
                        })
                    }
                    _ => true,
                };
                if !see_fits || !row_fn_fits {
                    out.push(Problem::BadBulk);
                }
            }
            _ => {}
        }
        out
    }
}

/// The path problems of a query plan: its `where` filter, its `see` filter
/// and its sort field. `server` is true when the plan runs on the server.
fn plan_problems(plan: &QueryPlan, server: bool, out: &mut Vec<Problem>) {
    if let Some(filter) = &plan.filter {
        filter_problems(filter, server, out);
    }
    if let See::Rule(filter) = &plan.see {
        filter_problems(filter, server, out);
    }
    if let Some((FieldRef::Implicit(field), _)) = plan.sort
        && server
        && field.is_client_only()
    {
        out.push(Problem::ClientFieldOnServer);
    }
}

/// The path problems of one filter, each at most once per node.
fn filter_problems(filter: &Filter, server: bool, out: &mut Vec<Problem>) {
    match filter {
        Filter::Value(_) => {}
        Filter::Field(path) => path_problems(path, server, out),
        Filter::RowId(hops) => {
            for hop in hops {
                hop_problems(hop.field, out);
            }
        }
        Filter::Cmp(_, a, b) => {
            filter_problems(a, server, out);
            filter_problems(b, server, out);
        }
        Filter::And(items) | Filter::Or(items) => {
            for item in items {
                filter_problems(item, server, out);
            }
        }
        Filter::Not(inner) | Filter::Coalesce(inner, _) => filter_problems(inner, server, out),
        Filter::InSet { elem, set } => {
            filter_problems(elem, server, out);
            path_problems(set, server, out);
        }
        Filter::InMap { key, map } | Filter::MapGet { map, key } => {
            filter_problems(key, server, out);
            path_problems(map, server, out);
        }
    }
}

fn path_problems(path: &Path, server: bool, out: &mut Vec<Problem>) {
    for hop in &path.hops {
        hop_problems(hop.field, out);
    }
    match path.field {
        FieldRef::Implicit(ImplicitField::Id) => out.push(Problem::BadPath),
        FieldRef::Implicit(field) if server && field.is_client_only() => {
            out.push(Problem::ClientFieldOnServer);
        }
        _ => {}
    }
}

fn hop_problems(field: FieldRef, out: &mut Vec<Problem>) {
    if !matches!(
        field,
        FieldRef::Declared(_) | FieldRef::Implicit(ImplicitField::Author)
    ) {
        out.push(Problem::BadPath);
    }
}
