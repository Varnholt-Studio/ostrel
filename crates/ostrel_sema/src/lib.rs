//! Name resolution and type checking for the Ostrel programming language.
//!
//! This is the minimal checker of the v0.1 slice (ARCHITECTURE 3.4, WP T2-1):
//! name resolution with function and block scopes, typing of `Int`, `Text`
//! and `Bool`, call arity, the `fn main()` entry check, the prelude function
//! `print`, and `E0100` for constructs outside the slice.
//!
//! [`check`] reads one syntax tree and returns side tables keyed by the ids of
//! that tree ([`Checked`]) plus all diagnostics. The tables are complete only
//! when no diagnostic is an error; lowering runs only then.
//!
//! Codes and messages follow the catalog `tests/errors/README.md`. One fault
//! gives one diagnostic: an expression whose type is unknown because of an
//! earlier error has the type [`Ty::Error`], which never causes a further
//! diagnostic.

// Trees and source text come from untrusted input: no unwrap, expect, panic or
// unchecked indexing outside of tests.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::collections::HashMap;

use ostrel_core::{Code, Diagnostic, FileId, Span};
use ostrel_syntax::ast::{
    BinaryOp, BlockId, ExprId, ExprKind, FnDecl, Item, Module, StmtId, StmtKind, StrPart,
    TextRange, TypeRef, UnaryOp,
};

/// Diagnostic codes reported by the checker.
pub mod codes {
    use ostrel_core::Code;

    /// Construct outside the v0.1 slice.
    pub const NOT_IN_V0_1: Code = Code::new(100);
    /// Unknown name.
    pub const UNKNOWN_NAME: Code = Code::new(200);
    /// Function declared twice.
    pub const DUPLICATE_FN: Code = Code::new(201);
    /// Parameter declared twice.
    pub const DUPLICATE_PARAM: Code = Code::new(202);
    /// Unknown type name.
    pub const UNKNOWN_TYPE: Code = Code::new(203);
    /// `+` with a `Text` operand.
    pub const TEXT_PLUS: Code = Code::new(300);
    /// Condition that is not `Bool`.
    pub const CONDITION_NOT_BOOL: Code = Code::new(301);
    /// Wrong number of call arguments.
    pub const ARITY: Code = Code::new(302);
    /// Trailing expression in a body with branches.
    pub const IMPLICIT_RETURN_IN_BRANCH: Code = Code::new(303);
    /// A value of the wrong type. Proposed, not yet in `tests/errors/README.md`.
    pub const MISMATCH: Code = Code::new(304);
    /// A function with a result type can end without `return`. Proposed, not
    /// yet in `tests/errors/README.md`.
    pub const MISSING_RETURN: Code = Code::new(305);
    /// No `fn main()`.
    pub const MISSING_MAIN: Code = Code::new(400);
    /// `fn main` with parameters.
    pub const MAIN_PARAMS: Code = Code::new(401);
    /// `fn main` with a result type.
    pub const MAIN_RESULT: Code = Code::new(402);
}

/// Type of a value in the v0.1 slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    /// No value: the result of a function without `-> T` and of `print`.
    Unit,
    /// Integer in the range of G9 and D24.
    Int,
    /// Immutable text.
    Text,
    /// `true` or `false`.
    Bool,
    /// Unknown because of an earlier diagnostic. Never appears when the
    /// diagnostics contain no error.
    Error,
}

impl Ty {
    fn describe(self) -> &'static str {
        match self {
            Ty::Unit => "nothing",
            Ty::Int => "`Int`",
            Ty::Text => "`Text`",
            Ty::Bool => "`Bool`",
            Ty::Error => "an unknown type",
        }
    }

    fn is_value(self) -> bool {
        matches!(self, Ty::Int | Ty::Text | Ty::Bool)
    }
}

/// Index of a function: its position in [`Module::items`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FnId(pub u32);

/// Index of a local in [`FnInfo::locals`]. Parameters come first, then every
/// `let` in source order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub u32);

/// What a name expression refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Res {
    /// A parameter or `let` of the enclosing function.
    Local(LocalId),
    /// A top level function.
    Fn(FnId),
    /// The prelude function `print`.
    Print,
}

/// Signature and locals of one function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnInfo {
    /// Parameter types in order.
    pub params: Vec<Ty>,
    /// Result type; [`Ty::Unit`] without `-> T`.
    pub result: Ty,
    /// Types of all locals, indexed by [`LocalId`].
    pub locals: Vec<Ty>,
    /// The trailing expression statement that is the implicit result of a
    /// body without branches (SYNTAX 7), if any.
    pub implicit_return: Option<ExprId>,
}

/// Result of [`check`]: side tables keyed by the ids of the checked tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Checked {
    /// One entry per item of the module, in item order; `None` for an item
    /// that is not a function.
    pub fns: Vec<Option<FnInfo>>,
    /// The entry point, if a function named `main` exists.
    pub main: Option<FnId>,
    /// Type of every checked expression.
    pub expr_types: HashMap<ExprId, Ty>,
    /// Resolution of every name expression, callees included.
    pub names: HashMap<ExprId, Res>,
    /// The local each `let` statement binds.
    pub lets: HashMap<StmtId, LocalId>,
}

/// Type names of the language that exist outside the v0.1 slice (E0100).
const LATER_TYPES: &[&str] = &["Float", "Time", "Bytes", "Rank", "List", "Set", "Map"];

/// Checks one script program. `source` is the text the tree was parsed from;
/// it is used only to place diagnostics at tokens the tree has no node for
/// (a binary operator, the `->` of a result type). Run the checker only on a
/// tree without lexer or parser errors.
pub fn check(module: &Module, file: FileId, source: &str) -> (Checked, Vec<Diagnostic>) {
    let mut cx = Cx {
        module,
        file,
        source,
        out: Checked::default(),
        diags: Vec::new(),
        fn_by_name: HashMap::new(),
        scope: Scope::default(),
        locals: Vec::new(),
        result: Ty::Unit,
    };
    cx.declare();
    for (index, item) in module.items().iter().enumerate() {
        #[allow(unreachable_patterns)]
        let decl = match item {
            Item::Fn(decl) => decl,
            _ => continue,
        };
        if let Some(Some(info)) = cx.out.fns.get(index).cloned() {
            let info = cx.check_fn(decl, info);
            if let Some(slot) = cx.out.fns.get_mut(index) {
                *slot = Some(info);
            }
        }
    }
    (cx.out, cx.diags)
}

/// Names visible in the current function, with an undo log for blocks.
#[derive(Default)]
struct Scope {
    bindings: HashMap<Box<str>, Vec<LocalId>>,
    log: Vec<Box<str>>,
}

impl Scope {
    fn bind(&mut self, name: &str, local: LocalId) {
        self.bindings.entry(name.into()).or_default().push(local);
        self.log.push(name.into());
    }

    fn lookup(&self, name: &str) -> Option<LocalId> {
        self.bindings.get(name).and_then(|v| v.last().copied())
    }

    fn mark(&self) -> usize {
        self.log.len()
    }

    fn reset(&mut self, mark: usize) {
        while self.log.len() > mark {
            if let Some(name) = self.log.pop()
                && let Some(stack) = self.bindings.get_mut(&name)
            {
                stack.pop();
            }
        }
    }
}

fn id_u32(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

struct Cx<'a> {
    module: &'a Module,
    file: FileId,
    source: &'a str,
    out: Checked,
    diags: Vec<Diagnostic>,
    fn_by_name: HashMap<&'a str, FnId>,
    scope: Scope,
    locals: Vec<Ty>,
    result: Ty,
}

impl<'a> Cx<'a> {
    fn error(&mut self, code: Code, range: TextRange, message: String) {
        let span = Span::new(self.file, range.start(), range.end());
        self.diags.push(Diagnostic::error(code, span, message));
    }

    /// First pass: signatures, duplicates and the entry check.
    fn declare(&mut self) {
        let module = self.module;
        for (index, item) in module.items().iter().enumerate() {
            #[allow(unreachable_patterns)]
            let decl = match item {
                Item::Fn(decl) => decl,
                _ => {
                    self.error(
                        codes::NOT_IN_V0_1,
                        TextRange::empty(0),
                        "this declaration is not available in v0.1".to_string(),
                    );
                    self.out.fns.push(None);
                    continue;
                }
            };
            let name = &*decl.name.text;
            if self.fn_by_name.contains_key(name) {
                self.error(
                    codes::DUPLICATE_FN,
                    decl.name.range,
                    format!("function `{name}` is already declared"),
                );
            } else {
                self.fn_by_name.insert(name, FnId(id_u32(index)));
            }
            let mut params = Vec::with_capacity(decl.params.len());
            for (i, param) in decl.params.iter().enumerate() {
                let earlier = decl.params.get(..i).unwrap_or_default();
                if earlier.iter().any(|p| p.name.text == param.name.text) {
                    self.error(
                        codes::DUPLICATE_PARAM,
                        param.name.range,
                        format!("parameter `{}` is already declared", param.name.text),
                    );
                }
                params.push(self.resolve_type(&param.ty));
            }
            let result = match &decl.result {
                Some(ty) => self.resolve_type(ty),
                None => Ty::Unit,
            };
            self.out.fns.push(Some(FnInfo {
                locals: params.clone(),
                params,
                result,
                implicit_return: None,
            }));
        }
        let Some(main) = self.fn_by_name.get("main").copied() else {
            self.error(
                codes::MISSING_MAIN,
                TextRange::empty(0),
                "missing `fn main()`; `ostrel run` starts a script program there".to_string(),
            );
            return;
        };
        self.out.main = Some(main);
        if let Some(Item::Fn(decl)) = module.items().get(main.0 as usize) {
            if let Some(first) = decl.params.first() {
                self.error(
                    codes::MAIN_PARAMS,
                    first.name.range,
                    "`fn main` takes no parameters".to_string(),
                );
            }
            if let Some(ty) = &decl.result {
                let at = self.arrow_before(ty.range.start());
                self.error(
                    codes::MAIN_RESULT,
                    at,
                    "`fn main` returns nothing; remove the result type".to_string(),
                );
            }
        }
    }

    fn resolve_type(&mut self, ty: &TypeRef) -> Ty {
        let name = &*ty.name.text;
        match name {
            "Int" => Ty::Int,
            "Text" => Ty::Text,
            "Bool" => Ty::Bool,
            _ if LATER_TYPES.contains(&name) => {
                self.error(
                    codes::NOT_IN_V0_1,
                    ty.range,
                    format!("type `{name}` is not available in v0.1"),
                );
                Ty::Error
            }
            _ => {
                self.error(
                    codes::UNKNOWN_TYPE,
                    ty.range,
                    format!("unknown type `{name}`; v0.1 has `Int`, `Text` and `Bool`"),
                );
                Ty::Error
            }
        }
    }

    fn check_fn(&mut self, decl: &FnDecl, mut info: FnInfo) -> FnInfo {
        self.scope = Scope::default();
        self.locals = info.locals.clone();
        self.result = info.result;
        for (i, param) in decl.params.iter().enumerate() {
            self.scope.bind(&param.name.text, LocalId(id_u32(i)));
        }
        let stmts: &[StmtId] = self.module.block(decl.body).map_or(&[], |b| &b.stmts);
        let has_branches = stmts
            .iter()
            .any(|&s| matches!(self.stmt_kind(s), Some(StmtKind::If { .. })));
        let implicit = match stmts.last().and_then(|&s| self.stmt_kind(s)) {
            Some(&StmtKind::Expr(e)) if !has_branches && info.result != Ty::Unit => Some(e),
            _ => None,
        };
        self.check_block(decl.body, implicit);
        info.implicit_return = implicit;
        info.locals = std::mem::take(&mut self.locals);
        if matches!(info.result, Ty::Unit | Ty::Error)
            || implicit.is_some()
            || self.always_returns(decl.body)
        {
            return info;
        }
        match self.falling_expr(decl.body) {
            Some(e) if has_branches => {
                let range = self.expr_range(e);
                self.error(
                    codes::IMPLICIT_RETURN_IN_BRANCH,
                    range,
                    "add `return`; a function body with branches returns only through `return`"
                        .to_string(),
                );
            }
            _ => self.error(
                codes::MISSING_RETURN,
                decl.name.range,
                format!(
                    "`{}` can end without returning {}; add `return`",
                    decl.name.text,
                    info.result.describe()
                ),
            ),
        }
        info
    }

    fn stmt_kind(&self, id: StmtId) -> Option<&'a StmtKind> {
        self.module.stmt(id).map(|s| &s.kind)
    }

    fn last_stmt(&self, block: BlockId) -> Option<&'a StmtKind> {
        let block = self.module.block(block)?;
        block.stmts.last().and_then(|&s| self.stmt_kind(s))
    }

    /// True if every path through the block ends in `return`.
    fn always_returns(&self, block: BlockId) -> bool {
        let Some(block) = self.module.block(block) else {
            return true;
        };
        block.stmts.iter().any(|&s| match self.stmt_kind(s) {
            Some(StmtKind::Return { .. }) => true,
            Some(&StmtKind::If {
                then_block,
                else_block: Some(else_block),
                ..
            }) => self.always_returns(then_block) && self.always_returns(else_block),
            _ => false,
        })
    }

    /// The trailing expression statement of a path that falls off the end of
    /// `block`, if that path ends in one.
    fn falling_expr(&self, block: BlockId) -> Option<ExprId> {
        match *self.last_stmt(block)? {
            StmtKind::Expr(e) => Some(e),
            StmtKind::If {
                then_block,
                else_block,
                ..
            } => {
                let then_falls = !self.always_returns(then_block);
                if let Some(e) = self.falling_expr(then_block).filter(|_| then_falls) {
                    return Some(e);
                }
                let else_block = else_block?;
                if self.always_returns(else_block) {
                    None
                } else {
                    self.falling_expr(else_block)
                }
            }
            _ => None,
        }
    }

    /// Checks the statements of a block in a new scope. `implicit` is the
    /// trailing expression that is the function result, if any.
    fn check_block(&mut self, id: BlockId, implicit: Option<ExprId>) {
        let Some(block) = self.module.block(id) else {
            return;
        };
        let mark = self.scope.mark();
        for &stmt in block.stmts.iter() {
            self.check_stmt(stmt, implicit);
        }
        self.scope.reset(mark);
    }

    fn check_stmt(&mut self, id: StmtId, implicit: Option<ExprId>) {
        let Some(stmt) = self.module.stmt(id) else {
            return;
        };
        #[allow(unreachable_patterns)]
        match &stmt.kind {
            StmtKind::Let { name, value } => {
                let ty = self.check_expr(*value);
                if ty == Ty::Unit {
                    self.mismatch(*value, "a value", ty);
                }
                let local = LocalId(id_u32(self.locals.len()));
                self.locals
                    .push(if ty == Ty::Unit { Ty::Error } else { ty });
                self.out.lets.insert(id, local);
                self.scope.bind(&name.text, local);
            }
            StmtKind::If {
                cond,
                then_block,
                else_block,
            } => {
                let ty = self.check_expr(*cond);
                if ty != Ty::Bool && ty != Ty::Error {
                    let range = self.expr_range(*cond);
                    self.error(
                        codes::CONDITION_NOT_BOOL,
                        range,
                        format!("condition must be `Bool`, found {}", ty.describe()),
                    );
                }
                self.check_block(*then_block, None);
                if let Some(else_block) = else_block {
                    self.check_block(*else_block, None);
                }
            }
            StmtKind::Return { value } => {
                let found = self.check_expr(*value);
                if self.result == Ty::Unit && found != Ty::Error {
                    let range = self.expr_range(*value);
                    self.error(
                        codes::MISMATCH,
                        range,
                        format!("this function returns nothing, found {}", found.describe()),
                    );
                } else {
                    self.compare(*value, self.result, found);
                }
            }
            StmtKind::Expr(value) => {
                let found = self.check_expr(*value);
                if implicit == Some(*value) {
                    self.compare(*value, self.result, found);
                }
            }
            _ => self.error(
                codes::NOT_IN_V0_1,
                stmt.range,
                "this statement is not available in v0.1".to_string(),
            ),
        }
    }

    /// Reports E0304 at `at` if `found` is not `expected`; the error type
    /// matches everything.
    fn compare(&mut self, at: ExprId, expected: Ty, found: Ty) {
        if found != expected && found != Ty::Error && expected != Ty::Error {
            self.mismatch(at, expected.describe(), found);
        }
    }

    fn mismatch(&mut self, at: ExprId, expected: &str, found: Ty) {
        let range = self.expr_range(at);
        self.error(
            codes::MISMATCH,
            range,
            format!("expected {expected}, found {}", found.describe()),
        );
    }

    fn expr_range(&self, id: ExprId) -> TextRange {
        self.module
            .expr(id)
            .map_or(TextRange::empty(0), |e| e.range)
    }

    fn check_expr(&mut self, id: ExprId) -> Ty {
        let ty = self.infer(id);
        self.out.expr_types.insert(id, ty);
        ty
    }

    fn infer(&mut self, id: ExprId) -> Ty {
        let Some(expr) = self.module.expr(id) else {
            return Ty::Error;
        };
        #[allow(unreachable_patterns)]
        match &expr.kind {
            ExprKind::Int(_) => Ty::Int,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Str(parts) => {
                for part in parts.iter() {
                    if let StrPart::Interp(e) = part
                        && self.check_expr(*e) == Ty::Unit
                    {
                        self.mismatch(*e, "a value", Ty::Unit);
                    }
                }
                Ty::Text
            }
            ExprKind::Name(name) => match self.resolve_name(id, &name.text, name.range) {
                Some(Res::Local(local)) => self.local_ty(local),
                Some(Res::Fn(_) | Res::Print) => {
                    self.error(
                        codes::NOT_IN_V0_1,
                        expr.range,
                        format!(
                            "using function `{}` as a value is not available in v0.1",
                            name.text
                        ),
                    );
                    Ty::Error
                }
                None => Ty::Error,
            },
            ExprKind::Paren(inner) => self.check_expr(*inner),
            ExprKind::Unary { op, operand } => {
                let want = match op {
                    UnaryOp::Neg => Ty::Int,
                    UnaryOp::Not => Ty::Bool,
                };
                let found = self.check_expr(*operand);
                self.compare(*operand, want, found);
                want
            }
            ExprKind::Binary { op, lhs, rhs } => self.binary(*op, *lhs, *rhs),
            ExprKind::Call { callee, args } => self.call(*callee, args),
            _ => {
                self.error(
                    codes::NOT_IN_V0_1,
                    expr.range,
                    "this expression is not available in v0.1".to_string(),
                );
                Ty::Error
            }
        }
    }

    fn local_ty(&self, local: LocalId) -> Ty {
        self.locals
            .get(local.0 as usize)
            .copied()
            .unwrap_or(Ty::Error)
    }

    /// Locals shadow functions, functions shadow the prelude.
    fn resolve_name(&mut self, id: ExprId, name: &str, range: TextRange) -> Option<Res> {
        let res = if let Some(local) = self.scope.lookup(name) {
            Res::Local(local)
        } else if let Some(&f) = self.fn_by_name.get(name) {
            Res::Fn(f)
        } else if name == "print" {
            Res::Print
        } else {
            self.error(codes::UNKNOWN_NAME, range, format!("unknown name `{name}`"));
            return None;
        };
        self.out.names.insert(id, res);
        Some(res)
    }

    fn binary(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Ty {
        let l = self.check_expr(lhs);
        let r = self.check_expr(rhs);
        let (operand, result) = match op {
            BinaryOp::Add if l == Ty::Text || r == Ty::Text => {
                let at = self.operator_after(self.expr_range(lhs).end(), op);
                self.error(
                    codes::TEXT_PLUS,
                    at,
                    "`+` does not join text; join text with interpolation".to_string(),
                );
                return Ty::Error;
            }
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => {
                (Ty::Int, Ty::Int)
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => (Ty::Int, Ty::Bool),
            BinaryOp::And | BinaryOp::Or => (Ty::Bool, Ty::Bool),
            BinaryOp::Eq | BinaryOp::Ne => {
                if l == Ty::Unit {
                    self.mismatch(lhs, "a value", l);
                } else if r == Ty::Unit {
                    self.mismatch(rhs, "a value", r);
                } else if l.is_value() {
                    self.compare(rhs, l, r);
                }
                return Ty::Bool;
            }
        };
        if l != operand && l != Ty::Error {
            self.mismatch(lhs, operand.describe(), l);
        } else {
            self.compare(rhs, operand, r);
        }
        result
    }

    fn call(&mut self, callee: ExprId, args: &[ExprId]) -> Ty {
        let callee_expr = self.module.expr(callee);
        let range = callee_expr.map_or(TextRange::empty(0), |e| e.range);
        let Some(ExprKind::Name(name)) = callee_expr.map(|e| &e.kind) else {
            self.error(
                codes::NOT_IN_V0_1,
                range,
                "calling anything but a function name is not available in v0.1".to_string(),
            );
            self.check_args(args);
            return Ty::Error;
        };
        let (params, result) = match self.resolve_name(callee, &name.text, name.range) {
            Some(Res::Fn(f)) => match self.out.fns.get(f.0 as usize) {
                Some(Some(info)) => (info.params.clone(), info.result),
                _ => (Vec::new(), Ty::Error),
            },
            Some(Res::Print) => (vec![Ty::Error], Ty::Unit),
            Some(Res::Local(local)) => {
                let ty = self.local_ty(local);
                self.out.expr_types.insert(callee, ty);
                if ty != Ty::Error {
                    self.error(
                        codes::MISMATCH,
                        range,
                        format!("expected a function, found {}", ty.describe()),
                    );
                }
                self.check_args(args);
                return Ty::Error;
            }
            None => {
                self.check_args(args);
                return Ty::Error;
            }
        };
        if params.len() != args.len() {
            let n = params.len();
            let plural = if n == 1 { "" } else { "s" };
            self.error(
                codes::ARITY,
                range,
                format!(
                    "`{}` takes {n} argument{plural}, found {}",
                    name.text,
                    args.len()
                ),
            );
            self.check_args(args);
            return result;
        }
        for (&arg, &want) in args.iter().zip(params.iter()) {
            let found = self.check_expr(arg);
            if found == Ty::Unit {
                self.mismatch(arg, "a value", found);
            } else {
                self.compare(arg, want, found);
            }
        }
        result
    }

    fn check_args(&mut self, args: &[ExprId]) {
        for &arg in args {
            self.check_expr(arg);
        }
    }

    /// Range of the binary operator `op` after byte `offset` of the source,
    /// skipping white space and comments. Falls back to an empty range at
    /// `offset` if the source does not match.
    fn operator_after(&self, offset: u32, op: BinaryOp) -> TextRange {
        let bytes = self.source.as_bytes();
        let mut at = offset as usize;
        let mut in_comment = false;
        while let Some(&b) = bytes.get(at) {
            match b {
                b'\n' => in_comment = false,
                b'#' => in_comment = true,
                b' ' | b'\t' | b'\r' => {}
                _ if in_comment => {}
                _ => break,
            }
            at += 1;
        }
        let text = op.as_str();
        match self.source.get(at..) {
            Some(rest) if rest.starts_with(text) => {
                let start = id_u32(at);
                TextRange::new(start, start.saturating_add(id_u32(text.len())))
                    .unwrap_or(TextRange::empty(offset))
            }
            _ => TextRange::empty(offset),
        }
    }

    /// Range of the `->` before a result type that starts at byte `offset`.
    /// Falls back to an empty range at `offset` if the source does not match.
    fn arrow_before(&self, offset: u32) -> TextRange {
        let head = self.source.get(..offset as usize).unwrap_or("");
        match head.trim_end_matches(' ').strip_suffix("->") {
            Some(before) => {
                let start = id_u32(before.len());
                TextRange::new(start, start.saturating_add(2)).unwrap_or(TextRange::empty(offset))
            }
            None => TextRange::empty(offset),
        }
    }
}

#[cfg(test)]
mod tests;
