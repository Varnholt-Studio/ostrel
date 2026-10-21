//! Function bodies: one exported JavaScript function per IR function.
//!
//! Locals become `let` variables `l<i>` (parameters first), blocks become the
//! cases of a `switch` inside a loop, so every control flow graph the lowering
//! builds has a direct translation; a function with one block is straight
//! line code.

use std::fmt::Write;

use ostrel_core::Span;
use ostrel_ir::full::{
    BinOp, BlockId, Const, FuncId, Function, Inst, InstKind, Local, Program, Target, Terminator,
    Ty, UnOp,
};

use crate::EmitError;
use crate::lit;

/// Appends `export function f<id>(...) { ... }` to `out`.
pub(crate) fn emit(
    program: &Program,
    id: FuncId,
    function: &Function,
    out: &mut String,
) -> Result<(), EmitError> {
    let invalid = EmitError::Invalid(function.span);
    let params = function.params as usize;
    if params > function.locals.len() || function.blocks.is_empty() {
        return Err(invalid);
    }
    let cx = Cx { program, function };
    let _ = write!(out, "export function f{}(", id.0);
    for i in 0..params {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "l{i}");
    }
    out.push_str(") {\n");
    if function.locals.len() > params {
        out.push_str("  let ");
        for i in params..function.locals.len() {
            if i > params {
                out.push_str(", ");
            }
            let _ = write!(out, "l{i}");
        }
        out.push_str(";\n");
    }
    if let [only] = function.blocks.as_slice()
        && matches!(only.term, Terminator::Return(_))
    {
        for inst in &only.insts {
            out.push_str("  ");
            cx.inst(inst, out)?;
        }
        out.push_str("  ");
        cx.term(&only.term, only.term_span, out)?;
    } else {
        out.push_str("  let b = 0;\n  for (;;) {\n    switch (b) {\n");
        for (index, block) in function.blocks.iter().enumerate() {
            let _ = writeln!(out, "      case {index}: {{");
            for inst in &block.insts {
                out.push_str("        ");
                cx.inst(inst, out)?;
            }
            out.push_str("        ");
            cx.term(&block.term, block.term_span, out)?;
            out.push_str("      }\n");
        }
        out.push_str("    }\n  }\n");
    }
    out.push_str("}\n");
    Ok(())
}

struct Cx<'a> {
    program: &'a Program,
    function: &'a Function,
}

impl Cx<'_> {
    /// The type of a local; an index out of range is inconsistent IR.
    fn ty(&self, local: Local, span: Span) -> Result<&Ty, EmitError> {
        self.function
            .local_ty(local)
            .ok_or(EmitError::Invalid(span))
    }

    /// The name of a local, after checking that it exists.
    fn local(&self, local: Local, span: Span) -> Result<String, EmitError> {
        self.ty(local, span)?;
        Ok(format!("l{}", local.0))
    }

    fn block(&self, block: BlockId, span: Span) -> Result<u32, EmitError> {
        if block.index() < self.function.blocks.len() {
            Ok(block.0)
        } else {
            Err(EmitError::Invalid(span))
        }
    }

    fn term(&self, term: &Terminator, span: Span, out: &mut String) -> Result<(), EmitError> {
        match term {
            Terminator::Return(None) => out.push_str("return;\n"),
            Terminator::Return(Some(value)) => {
                let value = self.local(*value, span)?;
                let _ = writeln!(out, "return {value};");
            }
            Terminator::Jump(target) => {
                let target = self.block(*target, span)?;
                let _ = writeln!(out, "b = {target}; continue;");
            }
            Terminator::Branch {
                cond,
                then,
                otherwise,
            } => {
                if *self.ty(*cond, span)? != Ty::Bool {
                    return Err(EmitError::Invalid(span));
                }
                let cond = self.local(*cond, span)?;
                let then = self.block(*then, span)?;
                let otherwise = self.block(*otherwise, span)?;
                let _ = writeln!(out, "b = {cond} ? {then} : {otherwise}; continue;");
            }
            Terminator::Next { .. } => {
                return Err(EmitError::Unsupported {
                    span,
                    what: "for loop",
                });
            }
        }
        Ok(())
    }

    fn inst(&self, inst: &Inst, out: &mut String) -> Result<(), EmitError> {
        let span = inst.span;
        let invalid = EmitError::Invalid(span);
        match &inst.kind {
            InstKind::Const { dst, value } => {
                let name = self.local(*dst, span)?;
                let _ = write!(out, "{name} = ");
                self.constant(value, span, out)?;
                out.push_str(";\n");
            }
            InstKind::Copy { dst, src }
            | InstKind::Some { dst, src }
            | InstKind::Unwrap { dst, src } => {
                let dst = self.local(*dst, span)?;
                let src = self.local(*src, span)?;
                let _ = writeln!(out, "{dst} = {src};");
            }
            InstKind::IsSome { dst, src } => {
                let dst = self.local(*dst, span)?;
                let src = self.local(*src, span)?;
                let _ = writeln!(out, "{dst} = {src} !== null;");
            }
            InstKind::Unary { dst, op, arg } => {
                let ty = self.ty(*arg, span)?;
                let dst = self.local(*dst, span)?;
                let arg = self.local(*arg, span)?;
                match (op, ty) {
                    // `0 - x` never gives -0; the range is symmetric (7.4).
                    (UnOp::Neg, Ty::Int) => {
                        let _ = writeln!(out, "{dst} = 0 - {arg};");
                    }
                    (UnOp::Neg, Ty::Float) => {
                        let _ = writeln!(out, "{dst} = -{arg};");
                    }
                    (UnOp::Not, Ty::Bool) => {
                        let _ = writeln!(out, "{dst} = !{arg};");
                    }
                    _ => return Err(invalid),
                }
            }
            InstKind::Binary { dst, op, lhs, rhs } => {
                let ty = self.ty(*lhs, span)?;
                let rhs_ty = self.ty(*rhs, span)?;
                let dst = self.local(*dst, span)?;
                let a = self.local(*lhs, span)?;
                let b = self.local(*rhs, span)?;
                let expr = binary(self.program, *op, ty, rhs_ty, &a, &b, span)?;
                let _ = writeln!(out, "{dst} = {expr};");
            }
            InstKind::ToText { dst, src } => {
                let ty = self.ty(*src, span)?;
                let dst = self.local(*dst, span)?;
                let src = self.local(*src, span)?;
                match ty {
                    Ty::Int => {
                        let _ = writeln!(out, "{dst} = String({src});");
                    }
                    Ty::Bool => {
                        let _ = writeln!(out, "{dst} = {src} ? 'true' : 'false';");
                    }
                    Ty::Text => {
                        let _ = writeln!(out, "{dst} = {src};");
                    }
                    _ => {
                        return Err(EmitError::Unsupported {
                            span,
                            what: "text form of this type",
                        });
                    }
                }
            }
            InstKind::Concat { dst, parts } => {
                let dst = self.local(*dst, span)?;
                let _ = write!(out, "{dst} = text(''");
                for part in parts {
                    if *self.ty(*part, span)? != Ty::Text {
                        return Err(invalid);
                    }
                    let _ = write!(out, " + l{}", part.0);
                }
                out.push_str(");\n");
            }
            InstKind::Now { dst } => {
                let dst = self.local(*dst, span)?;
                let _ = writeln!(out, "{dst} = Date.now();");
            }
            InstKind::Call { dst, func, args } => {
                let callee = self.program.function(*func).ok_or(invalid)?;
                if !callee.placement.runs_on(Target::Client) || callee.params as usize != args.len()
                {
                    return Err(invalid);
                }
                if let Some(dst) = dst {
                    let dst = self.local(*dst, span)?;
                    let _ = write!(out, "{dst} = ");
                }
                let _ = write!(out, "f{}(", func.0);
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&self.local(*arg, span)?);
                }
                out.push_str(");\n");
            }
            other => {
                return Err(EmitError::Unsupported {
                    span,
                    what: unsupported_name(other),
                });
            }
        }
        Ok(())
    }

    fn constant(&self, value: &Const, span: Span, out: &mut String) -> Result<(), EmitError> {
        let invalid = EmitError::Invalid(span);
        match value {
            Const::Int(n) => {
                if !(ostrel_ir::INT_MIN..=ostrel_ir::INT_MAX).contains(n) {
                    return Err(invalid);
                }
                let _ = write!(out, "({n})");
            }
            Const::Float(bits) => lit::float(f64::from_bits(*bits), out).ok_or(invalid)?,
            Const::Text(text) => lit::string(text, out),
            Const::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Const::Variant(e, i) => {
                let name = self
                    .program
                    .enums
                    .get(e.index())
                    .and_then(|e| e.variants.get(*i as usize))
                    .ok_or(invalid)?;
                lit::string(name, out);
            }
            Const::None => out.push_str("null"),
        }
        Ok(())
    }
}

/// The JavaScript expression of `a op b` for operands of type `ty` (and `rhs`
/// for the right operand).
fn binary(
    program: &Program,
    op: BinOp,
    ty: &Ty,
    rhs: &Ty,
    a: &str,
    b: &str,
    span: Span,
) -> Result<String, EmitError> {
    let invalid = EmitError::Invalid(span);
    let int_like = |t: &Ty| matches!(t, Ty::Int | Ty::Time);
    Ok(match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
            if *ty == Ty::Float && *rhs == Ty::Float {
                return Err(EmitError::Unsupported {
                    span,
                    what: "Float arithmetic",
                });
            }
            if !int_like(ty) || !int_like(rhs) {
                return Err(invalid);
            }
            match op {
                BinOp::Add => format!("int({a} + {b})"),
                BinOp::Sub => format!("int({a} - {b})"),
                BinOp::Mul => format!("int({a} * {b})"),
                BinOp::Div => format!("idiv({a}, {b})"),
                _ => format!("irem({a}, {b})"),
            }
        }
        BinOp::Eq | BinOp::Ne => {
            if ty != rhs {
                return Err(invalid);
            }
            if !identity_equality(ty) {
                return Err(EmitError::Unsupported {
                    span,
                    what: "equality of this type",
                });
            }
            let js = if op == BinOp::Eq { "===" } else { "!==" };
            format!("{a} {js} {b}")
        }
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            if ty != rhs {
                return Err(invalid);
            }
            let js = match op {
                BinOp::Lt => "<",
                BinOp::Le => "<=",
                BinOp::Gt => ">",
                _ => ">=",
            };
            match ty {
                Ty::Int | Ty::Float | Ty::Time => format!("{a} {js} {b}"),
                // Code point order, never UTF-16 units (D50).
                Ty::Text | Ty::Rank => format!("compareText({a}, {b}) {js} 0"),
                Ty::Enum(e) => {
                    if e.index() >= program.enums.len() {
                        return Err(invalid);
                    }
                    let e = e.0;
                    format!("ENUM_ORDER[{e}].get({a}) {js} ENUM_ORDER[{e}].get({b})")
                }
                _ => return Err(invalid),
            }
        }
    })
}

/// True for the types whose JavaScript values are equal exactly when `===`
/// says so: scalars held as primitives (`-0 === 0` matches `Float` equality),
/// enum names, row ids, and optionals of these with `none` as `null`.
fn identity_equality(ty: &Ty) -> bool {
    match ty {
        Ty::Bool
        | Ty::Int
        | Ty::Float
        | Ty::Text
        | Ty::Time
        | Ty::Rank
        | Ty::Enum(_)
        | Ty::Ref(_) => true,
        Ty::Optional(inner) => identity_equality(inner),
        _ => false,
    }
}

/// The name an [`EmitError::Unsupported`] gives an instruction kind.
fn unsupported_name(kind: &InstKind) -> &'static str {
    match kind {
        InstKind::Collect { .. } | InstKind::CollectMap { .. } => "collection literal",
        InstKind::Range { .. } => "Range",
        InstKind::Contains { .. } => "Contains",
        InstKind::Index { .. } => "Index",
        InstKind::Std { .. } => "Std",
        InstKind::Print { .. } => "Print",
        InstKind::Me { .. } => "Me",
        InstKind::Signed { .. } => "Signed",
        InstKind::RuleField { .. } | InstKind::RuleDelta { .. } => "rule input",
        InstKind::RemoteCall { .. } => "RemoteCall",
        InstKind::Settle { .. } => "Settle",
        InstKind::GetField { .. } => "GetField",
        InstKind::Make { .. }
        | InstKind::SetField { .. }
        | InstKind::SetAdd { .. }
        | InstKind::SetRemove { .. }
        | InstKind::MapPut { .. }
        | InstKind::MapRemove { .. }
        | InstKind::Drop { .. } => "row write",
        InstKind::Query { .. } => "Query",
        InstKind::BulkDrop { .. } | InstKind::BulkEdit { .. } => "bulk write",
        InstKind::IterStart { .. } => "for loop",
        InstKind::LoadVar { .. } | InstKind::StoreVar { .. } => "var",
        _ => "instruction",
    }
}
