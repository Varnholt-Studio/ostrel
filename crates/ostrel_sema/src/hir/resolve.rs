//! Name resolution: syntax trees to the HIR (ARCHITECTURE 3, F2, D130).
//!
//! Three passes over all files of one program:
//!
//! 1. Every top level declaration gets its [`DefId`]; top level names and enum
//!    variants are collected, so order in the source never matters.
//! 2. Signatures: field types and modes, parameter types, result types. After
//!    this pass every field of every model is known.
//! 3. Bodies: defaults, rules, checks, function bodies, views and `app` lines.
//!
//! A bare name resolves, first match wins: a field of the model in scope
//! (rules, `check`, `where` and `sort` of a query, so fields win, G14), a
//! contextual name of SYNTAX 6 where it has meaning, a local, a top level
//! definition, an enum variant, the prelude.
//!
//! `base.name` becomes [`ExprKind::Field`] when the model of `base` follows
//! from declarations (a declared parameter type, a loop over a query, `make`,
//! a reference field, `new`), and E0204 if that model has no such field.
//! Otherwise it is [`ExprKind::Property`] and `check` decides with the type
//! of `base`.
//!
//! One fault gives one diagnostic: a name that did not resolve becomes
//! [`Res::Error`], a type that did not resolve [`Ty::Error`], and neither
//! causes a further diagnostic here.

use std::collections::HashMap;

use ostrel_core::{Code, Diagnostic, FileId, Span};
use ostrel_syntax::ast::{self, TextRange};

use super::{
    AppDef, Block, BlockId, ContextName, Def, DefId, DefKind, Element, ElementName, Expr, ExprId,
    ExprKind, FieldDef, FieldId, FieldMode, FieldRef, FnDef, Handler, ImplicitField, Local,
    LocalId, Merge, ModelDef, Placement, Program, Query, Registration, Res, Rule, RuleOp, Source,
    StdName, Stmt, StmtId, StmtKind, StrPart, Ty, VarDef, ViewDef, ViewNode, ViewNodeId,
    ViewNodeKind,
};
use crate::codes;

/// A field name that is neither declared in the model nor implicit, or an
/// implicit field where only declared fields are allowed (SPEC 12.6).
pub(crate) const UNKNOWN_FIELD: Code = Code::new(204);
/// A name declared twice inside one declaration: a field (SPEC 12.6), a
/// variant, a field of `make`, an `app` line.
pub(crate) const DUPLICATE_MEMBER: Code = Code::new(206);
/// A local in scope at a query with the name of a field of the queried type,
/// or the loop variable used in its own query (SPEC 12.10).
pub(crate) const LOCAL_NAMES_FIELD: Code = Code::new(207);
/// A declared name that is not a model where only a model may stand.
pub(crate) const NOT_A_MODEL: Code = Code::new(205);
/// An implicit field as a write target (SPEC 12.6).
pub(crate) const IMPLICIT_WRITE: Code = Code::new(208);
/// A wrong number of type arguments (SPEC 12.6).
pub(crate) const TYPE_ARGS: Code = Code::new(309);

/// Resolves the names of all files of one program and builds its HIR.
pub(super) fn resolve(sources: &[Source<'_>]) -> (Program, Vec<Diagnostic>) {
    let mut rx = Rx::default();
    let items = rx.declare(sources);
    for item in &items {
        rx.signature(item);
    }
    for item in &items {
        rx.body(item);
    }
    (rx.p, rx.diags)
}

/// One top level declaration with the file it stands in.
struct ItemAt<'a> {
    file: FileId,
    module: &'a ast::Module,
    item: &'a ast::Item,
    def: DefId,
}

/// Contextual names bound at the current position (SYNTAX 6).
#[derive(Clone, Copy, Default)]
struct Ctx {
    new: bool,
    added: bool,
    it: bool,
}

#[derive(Default)]
struct Rx<'a> {
    p: Program,
    diags: Vec<Diagnostic>,
    file: Option<FileId>,
    module: Option<&'a ast::Module>,
    /// Top level names; a second declaration of a name is not listed.
    defs: HashMap<&'a str, DefId>,
    /// Every enum that declares a variant of this name, with its position.
    variants: HashMap<&'a str, Vec<(DefId, u32)>>,
    /// Declared fields per model; a second field of a name is not listed.
    fields: HashMap<DefId, HashMap<&'a str, FieldId>>,
    /// Whether the first `app` has an `auth` line; without it no model has
    /// the implicit `author` (G8, SPEC 12.6).
    auth: bool,
    /// Names of top level `var`s, which count as locals for SPEC 12.10.
    vars: Vec<&'a str>,
    /// Parameters of every function and view, bound when its body starts.
    params: HashMap<DefId, Vec<(&'a str, LocalId)>>,
    /// Visible locals, innermost last.
    scope: Vec<(&'a str, LocalId)>,
    /// The model of the row each local holds, if declarations show it.
    local_models: Vec<Option<DefId>>,
    /// The model whose fields bare names are.
    field_scope: Option<DefId>,
    /// The model whose rule or `check` is being resolved (`new`).
    data: Option<DefId>,
    ctx: Ctx,
    /// Names whose bare use was already reported (SPEC 12.10).
    banned: Vec<&'a str>,
    /// The loop variable of the `for` whose source is resolved next.
    loop_var: Option<&'a str>,
}

fn id(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

fn implicit_field(name: &str) -> Option<ImplicitField> {
    Some(match name {
        "id" => ImplicitField::Id,
        "made" => ImplicitField::Made,
        "changed" => ImplicitField::Changed,
        "author" => ImplicitField::Author,
        "pending" => ImplicitField::Pending,
        "rejected" => ImplicitField::Rejected,
        _ => return None,
    })
}

/// The model of a row of type `ty`.
fn ty_model(ty: &Ty) -> Option<DefId> {
    match ty {
        Ty::Ref(m) => Some(*m),
        Ty::Optional(inner) => ty_model(inner),
        _ => None,
    }
}

/// The model of the rows a `for` over a value of type `ty` binds.
fn elem_model(ty: &Ty) -> Option<DefId> {
    match ty {
        Ty::Set(inner) | Ty::List(inner) => ty_model(inner),
        Ty::Rows(m) => Some(*m),
        _ => None,
    }
}

impl<'a> Rx<'a> {
    fn span(&self, range: TextRange) -> Span {
        let file = self.file.unwrap_or(FileId::from_raw(0));
        Span::new(file, range.start(), range.end())
    }

    fn error(&mut self, code: Code, range: TextRange, message: String) {
        let span = self.span(range);
        self.diags.push(Diagnostic::error(code, span, message));
    }

    fn enter(&mut self, file: FileId, module: &'a ast::Module) {
        self.file = Some(file);
        self.module = Some(module);
    }

    fn def_kind(&self, def: DefId) -> Option<&DefKind> {
        self.p.def(def).map(|d| &d.kind)
    }

    /// The implicit field `name` names in this program.
    fn implicit(&self, name: &str) -> Option<ImplicitField> {
        implicit_field(name).filter(|&f| f != ImplicitField::Author || self.auth)
    }

    fn field_ref(&self, model: DefId, name: &str) -> Option<FieldRef> {
        match self.fields.get(&model).and_then(|f| f.get(name)) {
            Some(&field) => Some(FieldRef::Declared(field)),
            None => self.implicit(name).map(FieldRef::Implicit),
        }
    }

    fn field_ty(&self, model: DefId, field: FieldRef) -> Option<&Ty> {
        match field {
            FieldRef::Declared(f) => self.p.field(model, f).map(|f| &f.ty),
            FieldRef::Implicit(_) => None,
        }
    }

    fn def_name(&self, model: DefId) -> &str {
        self.p.def(model).map_or("", |d| &d.name)
    }

    // Pass 1.

    fn declare(&mut self, sources: &[Source<'a>]) -> Vec<ItemAt<'a>> {
        let mut items = Vec::new();
        for source in sources {
            self.enter(source.file, source.module);
            for item in source.module.items() {
                let def = DefId(id(self.p.defs.len()));
                #[allow(unreachable_patterns)]
                let (name, range): (Option<&'a ast::Ident>, TextRange) = match item {
                    ast::Item::Fn(d) => (Some(&d.name), d.range),
                    ast::Item::PlacedFn(d) => (Some(&d.name), d.range),
                    ast::Item::Data(d) => (Some(&d.name), d.range),
                    ast::Item::Enum(d) => (Some(&d.name), d.range),
                    ast::Item::Var(d) => (Some(&d.name), d.range),
                    ast::Item::View(d) => (Some(&d.name), d.range),
                    ast::Item::App(d) => {
                        if self.p.app.is_some() {
                            self.error(
                                codes::DUPLICATE_FN,
                                d.name.range,
                                "a program has one `app`; this is the second".to_string(),
                            );
                        } else {
                            self.p.app = Some(def);
                            self.auth = d
                                .items
                                .iter()
                                .any(|l| matches!(l.kind, ast::AppItemKind::Auth { .. }));
                        }
                        (None, d.range)
                    }
                    ast::Item::Style(d) => (None, d.range),
                    _ => {
                        self.error(
                            codes::NOT_IN_V0_1,
                            TextRange::empty(0),
                            "this declaration is not available yet".to_string(),
                        );
                        continue;
                    }
                };
                if let Some(name) = name {
                    if self.defs.contains_key(&*name.text) {
                        self.error(
                            codes::DUPLICATE_FN,
                            name.range,
                            format!("`{}` is already declared", name.text),
                        );
                    } else {
                        self.defs.insert(&name.text, def);
                    }
                }
                if let ast::Item::Var(d) = item {
                    self.vars.push(&d.name.text);
                }
                let label = match item {
                    ast::Item::App(d) => &*d.name.text,
                    ast::Item::Style(_) => "style",
                    _ => name.map_or("", |n| &*n.text),
                };
                // The category is known now, so types may name any model or
                // enum; pass 2 fills the rest.
                let kind = match item {
                    ast::Item::Data(_) => DefKind::Model(ModelDef {
                        fields: Vec::new(),
                        rules: Vec::new(),
                        checks: Vec::new(),
                    }),
                    ast::Item::Enum(_) => DefKind::Enum(Box::new([])),
                    _ => DefKind::Style,
                };
                let span = self.span(range);
                self.p.defs.push(Def {
                    name: label.into(),
                    span,
                    kind,
                });
                items.push(ItemAt {
                    file: source.file,
                    module: source.module,
                    item,
                    def,
                });
            }
        }
        items
    }

    // Pass 2.

    fn set_kind(&mut self, def: DefId, kind: DefKind) {
        if let Some(d) = self.p.defs.get_mut(def.index()) {
            d.kind = kind;
        }
    }

    fn signature(&mut self, at: &ItemAt<'a>) {
        self.enter(at.file, at.module);
        let kind = match at.item {
            ast::Item::Enum(d) => {
                let mut seen: Vec<&str> = Vec::new();
                for (i, v) in d.variants.iter().enumerate() {
                    if seen.contains(&&*v.text) {
                        self.error(
                            DUPLICATE_MEMBER,
                            v.range,
                            format!(
                                "variant `{}` is already declared in `{}`",
                                v.text, d.name.text
                            ),
                        );
                        continue;
                    }
                    seen.push(&v.text);
                    self.variants
                        .entry(&v.text)
                        .or_default()
                        .push((at.def, id(i)));
                }
                DefKind::Enum(d.variants.iter().map(|v| v.text.clone()).collect())
            }
            ast::Item::Data(d) => self.model_signature(at.def, d),
            ast::Item::Var(d) => DefKind::Var(VarDef {
                ty: match &d.init {
                    ast::VarInit::Typed { ty, .. } => Some(self.ty(*ty)),
                    ast::VarInit::Inferred(_) => None,
                },
                value: None,
            }),
            ast::Item::Fn(d) => {
                let params: Vec<_> = d
                    .params
                    .iter()
                    .map(|p| (&p.name, self.type_ref(&p.ty)))
                    .collect();
                let params = self.params(at.def, params);
                let result = d.result.as_ref().map_or(Ty::Unit, |r| self.type_ref(r));
                DefKind::Fn(FnDef {
                    declared: None,
                    placement: None,
                    params,
                    result,
                    elevated: None,
                    call_rule: None,
                    body: BlockId(0),
                })
            }
            ast::Item::PlacedFn(d) => {
                let params: Vec<_> = d.params.iter().map(|p| (&p.name, self.ty(p.ty))).collect();
                let params = self.params(at.def, params);
                let result = d.result.map_or(Ty::Unit, |r| self.ty(r));
                let declared = match d.placement {
                    ast::Placement::Inferred => None,
                    ast::Placement::Server => Some(Placement::Server),
                    ast::Placement::Client => Some(Placement::Client),
                };
                DefKind::Fn(FnDef {
                    declared,
                    placement: declared,
                    params,
                    result,
                    elevated: d.elevated.map(|r| self.span(r)),
                    call_rule: None,
                    body: BlockId(0),
                })
            }
            ast::Item::View(d) => {
                let params: Vec<_> = d
                    .params
                    .iter()
                    .flat_map(|ps| ps.iter())
                    .map(|p| (&p.name, self.ty(p.ty)))
                    .collect();
                let params = self.params(at.def, params);
                DefKind::View(ViewDef {
                    params,
                    nodes: Box::new([]),
                })
            }
            ast::Item::App(_) => DefKind::App(AppDef {
                auth: None,
                home: None,
            }),
            _ => DefKind::Style,
        };
        self.set_kind(at.def, kind);
    }

    /// Fields of a model; rules and checks follow in pass 3.
    fn model_signature(&mut self, def: DefId, d: &'a ast::DataDecl) -> DefKind {
        let mut names: HashMap<&'a str, FieldId> = HashMap::new();
        let decls: Vec<(&'a ast::FieldDecl, TextRange)> = d
            .members
            .iter()
            .filter_map(|m| match &m.kind {
                ast::MemberKind::Field(f) => Some((&**f, m.range)),
                _ => None,
            })
            .collect();
        for (i, (f, _)) in decls.iter().enumerate() {
            let implicit = self.implicit(&f.name.text).is_some();
            if names.contains_key(&*f.name.text) || implicit {
                let what = if implicit {
                    "is an implicit field of every row"
                } else {
                    "is already declared"
                };
                self.error(
                    DUPLICATE_MEMBER,
                    f.name.range,
                    format!("field `{}` {what} in `{}`", f.name.text, d.name.text),
                );
            } else {
                names.insert(&f.name.text, FieldId(id(i)));
            }
        }
        self.fields.insert(def, names);
        let mut fields = Vec::with_capacity(decls.len());
        for (f, range) in decls {
            let ty = self.ty(f.ty);
            let mode = match &f.mode {
                None => FieldMode::Plain,
                Some(ast::FieldMode::Fixed) => FieldMode::Fixed,
                Some(ast::FieldMode::Server) => FieldMode::Server,
                Some(ast::FieldMode::Serial { per }) => {
                    FieldMode::Serial(per.as_ref().and_then(|per| {
                        let found = self.field_ref(def, &per.text);
                        if found.is_none() {
                            self.unknown_field(def, per);
                        }
                        found
                    }))
                }
            };
            let merge = match &f.merge {
                None => Merge::Default,
                Some(m) if &*m.text == "text" => Merge::Text,
                Some(m) => {
                    self.error(
                        codes::UNKNOWN_NAME,
                        m.range,
                        format!(
                            "unknown merge strategy `{}`; the strategy is `merge text`",
                            m.text
                        ),
                    );
                    Merge::Default
                }
            };
            let span = self.span(range);
            fields.push(FieldDef {
                name: f.name.text.clone(),
                span,
                ty,
                default: None,
                mode,
                merge,
            });
        }
        DefKind::Model(ModelDef {
            fields,
            rules: Vec::new(),
            checks: Vec::new(),
        })
    }

    fn unknown_field(&mut self, model: DefId, name: &ast::Ident) {
        let hint = if &*name.text == "author" {
            "; every row has `author` only when the `app` has `auth paseto`"
        } else {
            ""
        };
        let message = format!(
            "data type `{}` has no field `{}`{hint}",
            self.def_name(model),
            name.text
        );
        self.error(UNKNOWN_FIELD, name.range, message);
    }

    fn implicit_write(&mut self, name: &ast::Ident) {
        let message = format!("`{}` is an implicit field; the system sets it", name.text);
        self.error(IMPLICIT_WRITE, name.range, message);
    }

    /// Parameters as locals; a repeated name is E0202 and not visible.
    fn params(&mut self, def: DefId, params: Vec<(&'a ast::Ident, Ty)>) -> Box<[LocalId]> {
        let mut bound: Vec<(&'a str, LocalId)> = Vec::with_capacity(params.len());
        for (name, ty) in params {
            if bound.iter().any(|(n, _)| *n == &*name.text) {
                self.error(
                    codes::DUPLICATE_PARAM,
                    name.range,
                    format!("parameter `{}` is already declared", name.text),
                );
            }
            let local = self.new_local(name, Some(ty));
            bound.push((&name.text, local));
        }
        let out = bound.iter().map(|&(_, l)| l).collect();
        self.params.insert(def, bound);
        out
    }

    fn new_local(&mut self, name: &ast::Ident, ty: Option<Ty>) -> LocalId {
        let local = LocalId(id(self.p.locals.len()));
        self.local_models.push(ty.as_ref().and_then(ty_model));
        let span = self.span(name.range);
        self.p.locals.push(Local {
            name: name.text.clone(),
            span,
            ty,
        });
        local
    }

    fn type_ref(&mut self, ty: &ast::TypeRef) -> Ty {
        self.named_ty(&ty.name, &[])
    }

    fn ty(&mut self, id: ast::TyId) -> Ty {
        let Some(module) = self.module else {
            return Ty::Error;
        };
        let Some(t) = module.ty(id) else {
            return Ty::Error;
        };
        let ty = self.named_ty(&t.name, &t.args);
        match ty {
            Ty::Error => Ty::Error,
            ty if t.optional => Ty::Optional(Box::new(ty)),
            ty => ty,
        }
    }

    fn named_ty(&mut self, name: &ast::Ident, args: &[ast::TyId]) -> Ty {
        let want = match &*name.text {
            "Set" | "List" => 1,
            "Map" => 2,
            "Bool" | "Int" | "Float" | "Text" | "Time" | "Bytes" | "Rank" => 0,
            other => match self.defs.get(other).map(|&d| self.def_kind(d)) {
                Some(Some(DefKind::Model(_) | DefKind::Enum(_))) => 0,
                found => {
                    let message = if found.is_some() {
                        format!("`{other}` is not a type")
                    } else {
                        format!("unknown type `{other}`")
                    };
                    self.error(codes::UNKNOWN_TYPE, name.range, message);
                    return Ty::Error;
                }
            },
        };
        if args.len() != want {
            let plural = if want == 1 { "" } else { "s" };
            let message = format!(
                "`{}` takes {want} type argument{plural}, found {}",
                name.text,
                args.len()
            );
            self.error(TYPE_ARGS, name.range, message);
            return Ty::Error;
        }
        let args: Vec<Ty> = args.iter().map(|&a| self.ty(a)).collect();
        if args.contains(&Ty::Error) {
            return Ty::Error;
        }
        let mut args = args.into_iter();
        let mut arg = || Box::new(args.next().unwrap_or(Ty::Error));
        match &*name.text {
            "Bool" => Ty::Bool,
            "Int" => Ty::Int,
            "Float" => Ty::Float,
            "Text" => Ty::Text,
            "Time" => Ty::Time,
            "Bytes" => Ty::Bytes,
            "Rank" => Ty::Rank,
            "Set" => Ty::Set(arg()),
            "List" => Ty::List(arg()),
            "Map" => {
                let key = arg();
                Ty::Map(key, arg())
            }
            other => match self.defs.get(other).map(|&d| (d, self.def_kind(d))) {
                Some((d, Some(DefKind::Model(_)))) => Ty::Ref(d),
                Some((d, Some(DefKind::Enum(_)))) => Ty::Enum(d),
                _ => Ty::Error,
            },
        }
    }

    // Pass 3.

    fn body(&mut self, at: &ItemAt<'a>) {
        self.enter(at.file, at.module);
        self.scope.clear();
        match at.item {
            ast::Item::Data(d) => self.model_body(at.def, d),
            ast::Item::Var(d) => {
                let value = match &d.init {
                    ast::VarInit::Typed { value, .. } => *value,
                    ast::VarInit::Inferred(value) => Some(*value),
                };
                let value = value.map(|v| self.expr(v));
                if let Some(Def {
                    kind: DefKind::Var(var),
                    ..
                }) = self.p.defs.get_mut(at.def.index())
                {
                    var.value = value;
                }
            }
            ast::Item::Fn(d) => self.fn_body(at.def, None, d.body),
            ast::Item::PlacedFn(d) => self.fn_body(at.def, d.call_rule.as_ref(), d.body),
            ast::Item::View(d) => {
                self.bind_params(at.def);
                let nodes = self.nodes(&d.nodes);
                if let Some(Def {
                    kind: DefKind::View(view),
                    ..
                }) = self.p.defs.get_mut(at.def.index())
                {
                    view.nodes = nodes;
                }
            }
            ast::Item::App(d) => self.app(at.def, d),
            _ => {}
        }
    }

    fn bind_params(&mut self, def: DefId) {
        if let Some(params) = self.params.get(&def) {
            self.scope.extend(params.iter().copied());
        }
    }

    fn fn_body(&mut self, def: DefId, call: Option<&'a ast::CallRule>, body: ast::BlockId) {
        self.bind_params(def);
        let call_rule = call.map(|c| {
            let cond = self.expr(c.cond);
            Rule {
                op: RuleOp::Call,
                field: None,
                cond,
                span: self.span(c.range),
            }
        });
        let body = self.block(body);
        if let Some(Def {
            kind: DefKind::Fn(f),
            ..
        }) = self.p.defs.get_mut(def.index())
        {
            f.call_rule = call_rule;
            f.body = body;
        }
    }

    fn model_body(&mut self, def: DefId, d: &'a ast::DataDecl) {
        let mut defaults = Vec::new();
        let mut rules = Vec::new();
        let mut checks = Vec::new();
        let mut field = 0usize;
        for member in d.members.iter() {
            match &member.kind {
                ast::MemberKind::Field(f) => {
                    if let Some(value) = f.default {
                        defaults.push((field, self.expr(value)));
                    }
                    field += 1;
                }
                ast::MemberKind::Rule(r) => {
                    if let Some(rule) = self.rule(def, r, member.range) {
                        rules.push(rule);
                    }
                }
                ast::MemberKind::Check(cond) => {
                    let cond = self.in_model(def, Ctx::default(), *cond);
                    checks.push(cond);
                }
            }
        }
        if let Some(Def {
            kind: DefKind::Model(m),
            ..
        }) = self.p.defs.get_mut(def.index())
        {
            for (i, value) in defaults {
                if let Some(f) = m.fields.get_mut(i) {
                    f.default = Some(value);
                }
            }
            m.rules = rules;
            m.checks = checks;
        }
    }

    fn rule(&mut self, model: DefId, r: &'a ast::RuleDecl, range: TextRange) -> Option<Rule> {
        let op = match r.verb {
            ast::RuleVerb::See => RuleOp::See,
            ast::RuleVerb::Make => RuleOp::Make,
            ast::RuleVerb::Edit => RuleOp::Edit,
            ast::RuleVerb::Drop => RuleOp::Drop,
        };
        let mut ctx = Ctx {
            new: op == RuleOp::Edit,
            ..Ctx::default()
        };
        let field = match &r.field {
            None => None,
            Some(name) => match self.field_ref(model, &name.text) {
                Some(FieldRef::Declared(f)) => {
                    // A field whose type was already reported binds them too,
                    // so the rule gives no follow up diagnostic.
                    ctx.added = matches!(
                        self.p.field(model, f).map(|f| &f.ty),
                        Some(Ty::Set(_) | Ty::Map(..) | Ty::Error)
                    );
                    Some(f)
                }
                Some(FieldRef::Implicit(_)) => {
                    self.implicit_write(name);
                    return None;
                }
                None => {
                    self.unknown_field(model, name);
                    return None;
                }
            },
        };
        let cond = self.in_model(model, ctx, r.cond);
        Some(Rule {
            op,
            field,
            cond,
            span: self.span(range),
        })
    }

    /// Resolves `cond` with the fields of `model` as bare names.
    fn in_model(&mut self, model: DefId, ctx: Ctx, cond: ast::ExprId) -> ExprId {
        let saved = (self.field_scope, self.data, self.ctx);
        self.field_scope = Some(model);
        self.data = Some(model);
        self.ctx = ctx;
        let cond = self.expr(cond);
        (self.field_scope, self.data, self.ctx) = saved;
        cond
    }

    fn app(&mut self, def: DefId, d: &'a ast::AppDecl) {
        let mut app = AppDef {
            auth: None,
            home: None,
        };
        let (mut auth_seen, mut home_seen) = (false, false);
        for line in d.items.iter() {
            match &line.kind {
                ast::AppItemKind::Auth { scheme, mode } => {
                    if std::mem::replace(&mut auth_seen, true) {
                        self.error(
                            DUPLICATE_MEMBER,
                            line.range,
                            "`auth` is already declared in this `app`".to_string(),
                        );
                        continue;
                    }
                    if &*scheme.text != "paseto" {
                        self.error(
                            codes::UNKNOWN_NAME,
                            scheme.range,
                            format!(
                                "unknown auth scheme `{}`; the scheme is `paseto`",
                                scheme.text
                            ),
                        );
                        continue;
                    }
                    app.auth = match mode.as_ref().map(|m| (&*m.text, m.range)) {
                        Some(("open", _)) => Some(Registration::Open),
                        Some(("invite", _)) => Some(Registration::Invite),
                        Some((other, range)) => {
                            self.error(
                                codes::UNKNOWN_NAME,
                                range,
                                format!("unknown registration `{other}`; use `open` or `invite`"),
                            );
                            None
                        }
                        None => {
                            self.error(
                                codes::UNKNOWN_NAME,
                                scheme.range,
                                "`auth paseto` needs `open` or `invite`".to_string(),
                            );
                            None
                        }
                    };
                }
                ast::AppItemKind::Home(view) => {
                    if std::mem::replace(&mut home_seen, true) {
                        self.error(
                            DUPLICATE_MEMBER,
                            line.range,
                            "`home` is already declared in this `app`".to_string(),
                        );
                        continue;
                    }
                    app.home = self.view(view);
                }
            }
        }
        if !home_seen {
            app.home = self
                .defs
                .get("Main")
                .copied()
                .filter(|&v| matches!(self.def_kind(v), Some(DefKind::View(_))));
        }
        self.set_kind(def, DefKind::App(app));
    }

    fn view(&mut self, name: &ast::Ident) -> Option<DefId> {
        match self.defs.get(&*name.text).map(|&d| (d, self.def_kind(d))) {
            Some((d, Some(DefKind::View(_)))) => Some(d),
            found => {
                let message = if found.is_some() {
                    format!("`{}` is not a view", name.text)
                } else {
                    format!("unknown view `{}`", name.text)
                };
                self.error(codes::UNKNOWN_NAME, name.range, message);
                None
            }
        }
    }

    // Statements.

    fn block(&mut self, id: ast::BlockId) -> BlockId {
        let block = self.module.and_then(|m| m.block(id));
        let mark = self.scope.len();
        let stmts: Vec<StmtId> = block
            .map(|b| b.stmts.iter().map(|&s| self.stmt(s)).collect())
            .unwrap_or_default();
        self.scope.truncate(mark);
        let span = self.span(block.map_or(TextRange::empty(0), |b| b.range));
        let out = BlockId(id_of(&self.p.blocks));
        self.p.blocks.push(Block {
            stmts: stmts.into_boxed_slice(),
            span,
        });
        out
    }

    fn bind(&mut self, name: &'a ast::Ident, model: Option<DefId>) -> LocalId {
        let local = self.new_local(name, None);
        if let Some(slot) = self.local_models.get_mut(local.index()) {
            *slot = model;
        }
        self.scope.push((&name.text, local));
        local
    }

    fn stmt(&mut self, id: ast::StmtId) -> StmtId {
        let Some(stmt) = self.module.and_then(|m| m.stmt(id)) else {
            let e = self.error_expr(TextRange::empty(0));
            return self.push_stmt(StmtKind::Expr(e), TextRange::empty(0));
        };
        #[allow(unreachable_patterns)]
        let kind = match &stmt.kind {
            ast::StmtKind::Let { name, value } => {
                let value = self.expr(*value);
                let model = self.model_of(value);
                StmtKind::Let {
                    local: self.bind(name, model),
                    value,
                }
            }
            ast::StmtKind::If {
                cond,
                then_block,
                else_block,
            } => StmtKind::If {
                cond: self.expr(*cond),
                then_block: self.block(*then_block),
                else_block: else_block.map(|b| self.block(b)),
            },
            ast::StmtKind::Return { value } => StmtKind::Return(self.expr(*value)),
            ast::StmtKind::Expr(value) => StmtKind::Expr(self.expr(*value)),
            ast::StmtKind::IfLet {
                name,
                value,
                then_block,
                else_block,
            } => {
                let value = self.expr(*value);
                let mark = self.scope.len();
                let model = self.model_of(value);
                let local = self.bind(name, model);
                let then_block = self.block(*then_block);
                self.scope.truncate(mark);
                StmtKind::IfLet {
                    local,
                    value,
                    then_block,
                    else_block: else_block.map(|b| self.block(b)),
                }
            }
            ast::StmtKind::For { name, source, body } => {
                let source = self.loop_source(name, *source);
                let mark = self.scope.len();
                let model = self.elem_model_of(source);
                let local = self.bind(name, model);
                let body = self.block(*body);
                self.scope.truncate(mark);
                StmtKind::For {
                    local,
                    source,
                    body,
                }
            }
            ast::StmtKind::Drop { source } => StmtKind::Drop(self.expr(*source)),
            ast::StmtKind::Assign { target, value } => {
                let target_at = self.module.and_then(|m| m.expr(*target));
                let target = self.expr(*target);
                if let Some(ast::ExprKind::Field { name, .. }) = target_at.map(|e| &e.kind)
                    && let Some(Expr {
                        kind:
                            ExprKind::Field {
                                field: (_, FieldRef::Implicit(_)),
                                ..
                            },
                        ..
                    }) = self.p.expr(target)
                {
                    self.implicit_write(name);
                }
                StmtKind::Assign {
                    target,
                    value: self.expr(*value),
                }
            }
            _ => {
                self.error(
                    codes::NOT_IN_V0_1,
                    stmt.range,
                    "this statement is not available yet".to_string(),
                );
                StmtKind::Expr(self.error_expr(stmt.range))
            }
        };
        self.push_stmt(kind, stmt.range)
    }

    fn push_stmt(&mut self, kind: StmtKind, range: TextRange) -> StmtId {
        let out = StmtId(id_of(&self.p.stmts));
        let span = self.span(range);
        self.p.stmts.push(Stmt { kind, span });
        out
    }

    fn loop_source(&mut self, name: &'a ast::Ident, source: ast::ExprId) -> ExprId {
        self.loop_var = Some(&name.text);
        let source = self.expr(source);
        self.loop_var = None;
        source
    }

    // View nodes.

    fn nodes(&mut self, ids: &[ast::ViewNodeId]) -> Box<[ViewNodeId]> {
        ids.iter().map(|&n| self.node(n)).collect()
    }

    fn node(&mut self, id: ast::ViewNodeId) -> ViewNodeId {
        let node = self.module.and_then(|m| m.view_node(id));
        let range = node.map_or(TextRange::empty(0), |n| n.range);
        #[allow(unreachable_patterns)]
        let kind = match node.map(|n| &n.kind) {
            Some(ast::ViewNodeKind::Element(e)) => ViewNodeKind::Element(Box::new(self.element(e))),
            Some(ast::ViewNodeKind::If(i)) => {
                let mark = self.scope.len();
                let (cond, bind) = match &i.cond {
                    ast::Cond::Expr(e) => (self.expr(*e), None),
                    ast::Cond::Let { name, value } => {
                        let value = self.expr(*value);
                        let model = self.model_of(value);
                        (value, Some(self.bind(name, model)))
                    }
                };
                let then_nodes = self.nodes(&i.then_nodes);
                self.scope.truncate(mark);
                let else_nodes = i
                    .else_nodes
                    .as_deref()
                    .map(|n| self.nodes(n))
                    .unwrap_or_default();
                ViewNodeKind::If {
                    cond,
                    bind,
                    then_nodes,
                    else_nodes,
                }
            }
            Some(ast::ViewNodeKind::For(f)) => {
                let source = self.loop_source(&f.name, f.source);
                let mark = self.scope.len();
                let model = self.elem_model_of(source);
                let local = self.bind(&f.name, model);
                let nodes = self.nodes(&f.nodes);
                self.scope.truncate(mark);
                ViewNodeKind::For {
                    local,
                    source,
                    nodes,
                }
            }
            _ => {
                self.error(
                    codes::NOT_IN_V0_1,
                    range,
                    "this view node is not available yet".to_string(),
                );
                ViewNodeKind::Element(Box::new(Element {
                    name: ElementName::Std("".into()),
                    value: None,
                    attrs: Box::new([]),
                    styles: Box::new([]),
                    handlers: Box::new([]),
                    children: Box::new([]),
                }))
            }
        };
        let out = ViewNodeId(id_of(&self.p.view_nodes));
        let span = self.span(range);
        self.p.view_nodes.push(ViewNode { kind, span });
        out
    }

    fn element(&mut self, e: &'a ast::Element) -> Element {
        let name = match self.defs.get(&*e.name.text).map(|&d| (d, self.def_kind(d))) {
            Some((d, Some(DefKind::View(_)))) => ElementName::View(d),
            _ => ElementName::Std(e.name.text.clone()),
        };
        let value = e.value.map(|v| self.expr(v));
        let mut attrs = Vec::new();
        let mut styles = Vec::new();
        for attr in e.attrs.iter() {
            match &attr.kind {
                ast::AttrKind::Named { name, value } => {
                    attrs.push((name.text.clone(), self.expr(*value)))
                }
                ast::AttrKind::Style { name, cond } => {
                    styles.push((name.text.clone(), cond.map(|c| self.expr(c))));
                }
            }
        }
        let mut handlers: Vec<Handler> = e.handler.iter().map(|h| self.handler(h)).collect();
        let mut children = Vec::new();
        for child in e.children.iter() {
            match child {
                ast::ElementChild::Node(n) => children.push(self.node(*n)),
                ast::ElementChild::Handler(h) => handlers.push(self.handler(h)),
            }
        }
        Element {
            name,
            value,
            attrs: attrs.into_boxed_slice(),
            styles: styles.into_boxed_slice(),
            handlers: handlers.into_boxed_slice(),
            children: children.into_boxed_slice(),
        }
    }

    fn handler(&mut self, h: &'a ast::Handler) -> Handler {
        let saved = self.ctx;
        self.ctx.it = true;
        let action = self.expr(h.action);
        let assign = h.assign.map(|v| self.expr(v));
        self.ctx = saved;
        Handler {
            event: h.event.text.clone(),
            action,
            assign,
            span: self.span(h.range),
        }
    }

    // Expressions.

    fn push_expr(&mut self, kind: ExprKind, range: TextRange) -> ExprId {
        let out = ExprId(id_of(&self.p.exprs));
        let span = self.span(range);
        self.p.exprs.push(Expr {
            kind,
            span,
            ty: None,
        });
        out
    }

    fn error_expr(&mut self, range: TextRange) -> ExprId {
        self.push_expr(ExprKind::Name(Res::Error), range)
    }

    fn expr(&mut self, id: ast::ExprId) -> ExprId {
        let own_loop_var = self.loop_var.take();
        let Some(expr) = self.module.and_then(|m| m.expr(id)) else {
            return self.error_expr(TextRange::empty(0));
        };
        let range = expr.range;
        #[allow(unreachable_patterns)]
        let kind = match &expr.kind {
            ast::ExprKind::Int(v) => ExprKind::Int(*v),
            ast::ExprKind::Bool(v) => ExprKind::Bool(*v),
            ast::ExprKind::Str(parts) => ExprKind::Str(
                parts
                    .iter()
                    .map(|p| match p {
                        ast::StrPart::Text(t) => StrPart::Text(t.clone()),
                        ast::StrPart::Interp(e) => StrPart::Interp(self.expr(*e)),
                    })
                    .collect(),
            ),
            ast::ExprKind::Name(name) => ExprKind::Name(self.lookup(name)),
            ast::ExprKind::Unary { op, operand } => ExprKind::Unary {
                op: *op,
                operand: self.expr(*operand),
            },
            ast::ExprKind::Binary { op, lhs, rhs } => ExprKind::Binary {
                op: *op,
                lhs: self.expr(*lhs),
                rhs: self.expr(*rhs),
            },
            ast::ExprKind::Call { callee, args } => ExprKind::Call {
                callee: self.expr(*callee),
                args: args.iter().map(|&a| self.expr(a)).collect(),
            },
            ast::ExprKind::Paren(inner) => return self.expr(*inner),
            ast::ExprKind::Keyword(k) => match k {
                ast::KeywordValue::NoneValue => ExprKind::NoneValue,
                ast::KeywordValue::Me => ExprKind::Me,
                ast::KeywordValue::Now => ExprKind::Now,
                ast::KeywordValue::Signed => ExprKind::Signed,
            },
            ast::ExprKind::Field { base, name } => {
                let base = self.expr(*base);
                match self.model_of(base) {
                    Some(model) => match self.field_ref(model, &name.text) {
                        Some(field) => ExprKind::Field {
                            base,
                            field: (model, field),
                        },
                        None => {
                            self.unknown_field(model, name);
                            ExprKind::Name(Res::Error)
                        }
                    },
                    None => ExprKind::Property {
                        base,
                        name: name.text.clone(),
                    },
                }
            }
            ast::ExprKind::Index { base, index } => ExprKind::Index {
                base: self.expr(*base),
                index: self.expr(*index),
            },
            ast::ExprKind::In {
                element,
                collection,
                negated,
            } => ExprKind::In {
                element: self.expr(*element),
                collection: self.expr(*collection),
                negated: *negated,
            },
            ast::ExprKind::Range { lo, hi } => ExprKind::Range {
                lo: self.expr(*lo),
                hi: self.expr(*hi),
            },
            ast::ExprKind::Catch { value, kinds } => ExprKind::Catch {
                value: self.expr(*value),
                kinds: kinds.iter().map(|k| k.text.clone()).collect(),
            },
            ast::ExprKind::Fallback { value, fallback } => ExprKind::Fallback {
                value: self.expr(*value),
                fallback: self.expr(*fallback),
            },
            ast::ExprKind::Set(items) => {
                ExprKind::Set(items.iter().map(|&e| self.expr(e)).collect())
            }
            ast::ExprKind::List(items) => {
                ExprKind::List(items.iter().map(|&e| self.expr(e)).collect())
            }
            ast::ExprKind::Map(entries) => ExprKind::Map(
                entries
                    .iter()
                    .map(|e| (self.expr(e.key), self.expr(e.value)))
                    .collect(),
            ),
            ast::ExprKind::Make(m) => self.make(m),
            ast::ExprKind::Query(q) => self.query(q, own_loop_var),
            _ => {
                self.error(
                    codes::NOT_IN_V0_1,
                    range,
                    "this expression is not available yet".to_string(),
                );
                ExprKind::Name(Res::Error)
            }
        };
        self.push_expr(kind, range)
    }

    /// Resolves a bare name in the current position.
    fn lookup(&mut self, name: &ast::Ident) -> Res {
        let text = &*name.text;
        if let Some(model) = self.field_scope
            && let Some(field) = self.field_ref(model, text)
        {
            return Res::Field(model, field);
        }
        if let Some(model) = self.field_scope
            && implicit_field(text).is_some()
        {
            self.unknown_field(model, name);
            return Res::Error;
        }
        if self.banned.contains(&text) {
            return Res::Error;
        }
        let ctx = self.ctx;
        match text {
            "new" if ctx.new => return Res::Context(ContextName::New),
            "added" if ctx.added => return Res::Context(ContextName::Added),
            "removed" if ctx.added => return Res::Context(ContextName::Removed),
            "it" if ctx.it => return Res::Context(ContextName::It),
            _ => {}
        }
        if let Some(&(_, local)) = self.scope.iter().rev().find(|(n, _)| *n == text) {
            return Res::Local(local);
        }
        if let Some(&def) = self.defs.get(text) {
            return Res::Def(def);
        }
        match self.variants.get(text).map(Vec::as_slice) {
            Some(&[(def, index)]) => return Res::Variant(def, index),
            Some([first, second, ..]) => {
                let message = format!(
                    "`{text}` is a variant of both `{}` and `{}`",
                    self.def_name(first.0),
                    self.def_name(second.0)
                );
                self.error(codes::UNKNOWN_NAME, name.range, message);
                return Res::Error;
            }
            _ => {}
        }
        if text == "print" {
            return Res::Std(StdName::Print);
        }
        self.error(
            codes::UNKNOWN_NAME,
            name.range,
            format!("unknown name `{text}`"),
        );
        Res::Error
    }

    fn make(&mut self, m: &'a ast::MakeExpr) -> ExprKind {
        let model = self.model(
            &m.ty,
            false,
            "`make` needs a data type",
            codes::UNKNOWN_TYPE,
        );
        let mut fields = Vec::with_capacity(m.fields.len());
        let mut seen: Vec<&str> = Vec::new();
        for init in m.fields.iter() {
            let value = self.expr(init.value);
            let Some(model) = model else { continue };
            let name = &*init.name.text;
            match self.field_ref(model, name) {
                Some(FieldRef::Declared(field)) => {
                    if seen.contains(&name) {
                        self.error(
                            DUPLICATE_MEMBER,
                            init.name.range,
                            format!("field `{name}` is already given in this `make`"),
                        );
                    } else {
                        seen.push(name);
                        fields.push((field, value));
                    }
                }
                Some(FieldRef::Implicit(_)) => self.implicit_write(&init.name),
                None => self.unknown_field(model, &init.name),
            }
        }
        match model {
            Some(model) => ExprKind::Make {
                model,
                fields: fields.into_boxed_slice(),
            },
            None => ExprKind::Name(Res::Error),
        }
    }

    /// The model `name` declares (SPEC 12.6): E0205 for a declared name or a
    /// `local` that is no model, `unknown` for an unknown name (E0203 where a
    /// type is expected, E0200 in a query head).
    fn model(
        &mut self,
        name: &ast::Ident,
        local: bool,
        need: &str,
        unknown: Code,
    ) -> Option<DefId> {
        match self.defs.get(&*name.text).map(|&d| (d, self.def_kind(d))) {
            Some((d, Some(DefKind::Model(_)))) if !local => Some(d),
            found => {
                let (code, message) = if local || found.is_some() {
                    (NOT_A_MODEL, format!("{need}; `{}` is not one", name.text))
                } else {
                    (unknown, format!("unknown data type `{}`", name.text))
                };
                self.error(code, name.range, message);
                None
            }
        }
    }

    /// A query source (D69, D133): a query over a model, the variants of an
    /// enum, or a value to iterate.
    fn query(&mut self, q: &'a ast::Query, own_loop_var: Option<&'a str>) -> ExprKind {
        let head = &*q.head.text;
        let clauses = q.filter.is_some() || q.sort.is_some() || q.take.is_some();
        let local = self.scope.iter().any(|(n, _)| *n == head);
        let def = self.defs.get(head).copied().filter(|_| !local);
        match def.map(|d| (d, self.def_kind(d))) {
            Some((model, Some(DefKind::Model(_)))) => {
                ExprKind::Query(Box::new(self.model_query(model, q, own_loop_var)))
            }
            _ if clauses => {
                let _ = self.model(
                    &q.head,
                    local,
                    "a query with `where`, `sort`, `limit` or `last` needs a data type",
                    codes::UNKNOWN_NAME,
                );
                ExprKind::Name(Res::Error)
            }
            _ => ExprKind::Name(self.lookup(&q.head)),
        }
    }

    fn model_query(&mut self, model: DefId, q: &'a ast::Query, own: Option<&'a str>) -> Query {
        let banned = self.clashes(model, q, own);
        let mark = self.banned.len();
        self.banned.extend(banned);
        let outer = self.field_scope;
        self.field_scope = Some(model);
        let filter = q.filter.map(|f| self.expr(f));
        self.field_scope = outer;
        let sort = q
            .sort
            .as_ref()
            .and_then(|s| match self.field_ref(model, &s.field.text) {
                Some(field) => Some((field, s.desc)),
                None => {
                    if !self.banned.contains(&&*s.field.text) {
                        self.unknown_field(model, &s.field);
                    }
                    None
                }
            });
        let take = q.take.as_ref().map(|t| (t.kind, self.expr(t.count)));
        self.banned.truncate(mark);
        Query {
            model,
            filter,
            sort,
            take,
        }
    }

    /// Reports the locals a query may not see (SPEC 12.10) and returns their
    /// names.
    fn clashes(&mut self, model: DefId, q: &'a ast::Query, own: Option<&'a str>) -> Vec<&'a str> {
        let mut uses: Vec<&'a ast::Ident> = Vec::new();
        if let Some(f) = q.filter {
            self.bare_names(f, &mut uses);
        }
        if let Some(s) = &q.sort {
            uses.push(&s.field);
        }
        if let Some(t) = &q.take {
            self.bare_names(t.count, &mut uses);
        }
        let mut names: Vec<&'a str> = Vec::new();
        if q.filter.is_some() || q.sort.is_some() {
            let locals = self
                .scope
                .iter()
                .map(|(n, _)| *n)
                .chain(self.vars.iter().copied())
                .chain(own);
            for name in locals {
                if self.field_ref(model, name).is_some() && !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        if let Some(own) = own
            && !names.contains(&own)
            && uses.iter().any(|u| &*u.text == own)
        {
            names.push(own);
        }
        let mut found: Vec<(TextRange, &'a str)> = names
            .iter()
            .map(|&name| {
                let at = uses
                    .iter()
                    .filter(|u| &*u.text == name)
                    .map(|u| u.range)
                    .min_by_key(|r| r.start())
                    .unwrap_or(q.head.range);
                (at, name)
            })
            .collect();
        found.sort_by_key(|(at, _)| at.start());
        for &(at, name) in &found {
            let message = if self.field_ref(model, name).is_some() {
                format!(
                    "local `{name}` has the name of a field of `{}`; rename the local",
                    self.def_name(model)
                )
            } else {
                format!("loop variable `{name}` is not bound in its own query; rename the local")
            };
            self.error(LOCAL_NAMES_FIELD, at, message);
        }
        names
    }

    /// Bare names of an expression tree, without descending into nested
    /// queries, whose bare names belong to another model.
    fn bare_names(&self, id: ast::ExprId, out: &mut Vec<&'a ast::Ident>) {
        let Some(expr) = self.module.and_then(|m| m.expr(id)) else {
            return;
        };
        let mut each = |ids: &[ast::ExprId]| ids.iter().for_each(|&e| self.bare_names(e, out));
        #[allow(unreachable_patterns)]
        match &expr.kind {
            ast::ExprKind::Name(name) => out.push(name),
            ast::ExprKind::Str(parts) => parts.iter().for_each(|p| {
                if let ast::StrPart::Interp(e) = p {
                    self.bare_names(*e, out);
                }
            }),
            ast::ExprKind::Unary { operand, .. } => each(&[*operand]),
            ast::ExprKind::Binary { lhs, rhs, .. } => each(&[*lhs, *rhs]),
            ast::ExprKind::Call { callee, args } => {
                self.bare_names(*callee, out);
                args.iter().for_each(|&a| self.bare_names(a, out));
            }
            ast::ExprKind::Paren(e) | ast::ExprKind::Field { base: e, .. } => each(&[*e]),
            ast::ExprKind::Index { base, index } => each(&[*base, *index]),
            ast::ExprKind::In {
                element,
                collection,
                ..
            } => each(&[*element, *collection]),
            ast::ExprKind::Range { lo, hi } => each(&[*lo, *hi]),
            ast::ExprKind::Catch { value, .. } => each(&[*value]),
            ast::ExprKind::Fallback { value, fallback } => each(&[*value, *fallback]),
            ast::ExprKind::Set(items) | ast::ExprKind::List(items) => each(items),
            ast::ExprKind::Map(entries) => entries.iter().for_each(|e| each(&[e.key, e.value])),
            ast::ExprKind::Make(m) => m.fields.iter().for_each(|f| self.bare_names(f.value, out)),
            _ => {}
        }
    }

    /// The model of the row `expr` holds, when declarations show it.
    fn model_of(&self, expr: ExprId) -> Option<DefId> {
        match &self.p.expr(expr)?.kind {
            ExprKind::Name(Res::Local(l)) => self.local_models.get(l.index()).copied().flatten(),
            ExprKind::Name(Res::Field(m, f)) | ExprKind::Field { field: (m, f), .. } => {
                self.field_ty(*m, *f).and_then(ty_model)
            }
            ExprKind::Name(Res::Context(ContextName::New)) => self.data,
            ExprKind::Name(Res::Def(d)) => match self.def_kind(*d)? {
                DefKind::Var(v) => v.ty.as_ref().and_then(ty_model),
                _ => None,
            },
            ExprKind::Make { model, .. } => Some(*model),
            ExprKind::Fallback { value, .. } => self.model_of(*value),
            ExprKind::Call { callee, .. } => match &self.p.expr(*callee)?.kind {
                ExprKind::Name(Res::Def(d)) => match self.def_kind(*d)? {
                    DefKind::Fn(f) => ty_model(&f.result),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }

    /// The model of the rows a `for` over `source` binds.
    fn elem_model_of(&self, source: ExprId) -> Option<DefId> {
        match &self.p.expr(source)?.kind {
            ExprKind::Query(q) => Some(q.model),
            ExprKind::Name(Res::Local(l)) => self.p.local(*l)?.ty.as_ref().and_then(elem_model),
            ExprKind::Name(Res::Field(m, f)) | ExprKind::Field { field: (m, f), .. } => {
                self.field_ty(*m, *f).and_then(elem_model)
            }
            _ => None,
        }
    }
}

fn id_of<T>(arena: &[T]) -> u32 {
    id(arena.len())
}

#[cfg(test)]
mod tests;
