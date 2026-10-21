//! `see` rules and `where` clauses as filters of the pushdown subset
//! (ARCHITECTURE 9): comparisons, `and`, `or`, `not`, constants, `me`,
//! `signed`, field access with at most two reference hops, `x in setField`,
//! `x in mapField`, `mapField[key]` and `e ?? constant`.
//!
//! A sub expression that reads no field of the row is a value fixed before
//! the query runs: in a `where` clause the body evaluates it into a local
//! ([`Operands`]), in a `see` rule it must be a constant, `me` or `signed`.
//! Anything else that reads the row is outside the subset, which `check`
//! rejects, so it is [`LowerError::Unchecked`].

use ostrel_sema::hir;
use ostrel_syntax::ast::{BinaryOp, UnaryOp};

use super::super::Const;
use super::super::query::{CmpOp, Filter, Hop, MAX_FILTER_NODES, MAX_HOPS, Operand, Path};
use super::{Cx, LowerError, Lowered, field_ref};

/// Turns a sub expression that reads no field of the row into an operand.
pub(super) trait Operands {
    fn operand(&mut self, e: hir::ExprId) -> Lowered<Operand>;
}

/// The `see` rule of the model `row`. A rule that mentions `me` is false
/// as a whole without a principal (SYNTAX 4.4), also under `!=`, `not` and
/// `not in`, so it becomes `And[me != none, f]` (D179).
pub(super) fn see_rule(cx: &Cx<'_>, row: hir::DefId, cond: hir::ExprId) -> Lowered<Filter> {
    let filter = Translate {
        cx,
        row,
        ops: &mut Consts { cx },
    }
    .filter(cond)?;
    let filter = if mentions_me(cx.hir, cond) {
        let signed_in = Filter::Cmp(
            CmpOp::Ne,
            Box::new(Filter::Value(Operand::Me)),
            Box::new(Filter::Value(Operand::Const(Const::None))),
        );
        Filter::And(vec![signed_in, filter])
    } else {
        filter
    };
    bounded(cx, cond, filter)
}

/// The filter of `e` over rows of the model `row`, with at most
/// [`MAX_FILTER_NODES`] nodes.
pub(super) fn filter(
    cx: &Cx<'_>,
    row: hir::DefId,
    e: hir::ExprId,
    ops: &mut dyn Operands,
) -> Lowered<Filter> {
    let filter = Translate { cx, row, ops }.filter(e)?;
    bounded(cx, e, filter)
}

fn bounded(cx: &Cx<'_>, e: hir::ExprId, filter: Filter) -> Lowered<Filter> {
    if filter.nodes() > MAX_FILTER_NODES {
        return Err(LowerError::Unchecked(span(cx, e)));
    }
    Ok(filter)
}

/// True when `e` mentions `me` anywhere.
pub(super) fn mentions_me(hir: &hir::Program, e: hir::ExprId) -> bool {
    match hir.expr(e).map(|x| &x.kind) {
        None => false,
        Some(hir::ExprKind::Me) => true,
        Some(kind) => children(kind).into_iter().any(|c| mentions_me(hir, c)),
    }
}

/// The constant operands of a `see` rule.
struct Consts<'a, 'h> {
    cx: &'a Cx<'h>,
}

impl Operands for Consts<'_, '_> {
    fn operand(&mut self, e: hir::ExprId) -> Lowered<Operand> {
        constant(self.cx, e).ok_or(LowerError::Unchecked(span(self.cx, e)))
    }
}

/// The operand of a literal, a variant, `none`, `me` or `signed`.
pub(super) fn constant(cx: &Cx<'_>, e: hir::ExprId) -> Option<Operand> {
    let expr = cx.hir.expr(e)?;
    Some(match &expr.kind {
        hir::ExprKind::Int(n) => Operand::Const(Const::Int(*n)),
        hir::ExprKind::Bool(b) => Operand::Const(Const::Bool(*b)),
        hir::ExprKind::Str(parts) => {
            let mut text = String::new();
            for part in parts.iter() {
                match part {
                    hir::StrPart::Text(t) => text.push_str(t),
                    hir::StrPart::Interp(_) => return None,
                }
            }
            Operand::Const(Const::Text(text))
        }
        hir::ExprKind::Name(hir::Res::Variant(def, index)) => {
            Operand::Const(Const::Variant(*cx.enums.get(def)?, *index))
        }
        hir::ExprKind::NoneValue => Operand::Const(Const::None),
        hir::ExprKind::Me => Operand::Me,
        hir::ExprKind::Signed => Operand::Signed,
        hir::ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => match cx.hir.expr(*operand).map(|x| &x.kind) {
            Some(hir::ExprKind::Int(n)) => Operand::Const(Const::Int(n.checked_neg()?)),
            _ => return None,
        },
        _ => return None,
    })
}

pub(super) fn span(cx: &Cx<'_>, e: hir::ExprId) -> ostrel_core::Span {
    cx.hir.expr(e).map_or(
        ostrel_core::Span::point(ostrel_core::FileId::from_raw(0), 0),
        |x| x.span,
    )
}

/// True when `e` reads a field of the row in scope: a bare field name or a
/// contextual name.
pub(super) fn reads_row(hir: &hir::Program, e: hir::ExprId) -> bool {
    let Some(expr) = hir.expr(e) else {
        return false;
    };
    match &expr.kind {
        hir::ExprKind::Name(hir::Res::Field(..) | hir::Res::Context(_)) => true,
        kind => children(kind).into_iter().any(|c| reads_row(hir, c)),
    }
}

/// The direct sub expressions of an expression.
fn children(kind: &hir::ExprKind) -> Vec<hir::ExprId> {
    use hir::ExprKind as K;
    match kind {
        K::Str(parts) => parts
            .iter()
            .filter_map(|p| match p {
                hir::StrPart::Interp(e) => Some(*e),
                hir::StrPart::Text(_) => None,
            })
            .collect(),
        K::Unary { operand, .. } => vec![*operand],
        K::Binary { lhs, rhs, .. } => vec![*lhs, *rhs],
        K::Call { callee, args } => std::iter::once(*callee)
            .chain(args.iter().copied())
            .collect(),
        K::Field { base, .. } | K::Property { base, .. } => vec![*base],
        K::Index { base, index } => vec![*base, *index],
        K::In {
            element,
            collection,
            ..
        } => vec![*element, *collection],
        K::Range { lo, hi } => vec![*lo, *hi],
        K::Catch { value, .. } => vec![*value],
        K::Fallback { value, fallback } => vec![*value, *fallback],
        K::Set(items) | K::List(items) => items.to_vec(),
        K::Map(entries) => entries.iter().flat_map(|(k, v)| [*k, *v]).collect(),
        K::Make { fields, .. } => fields.iter().map(|(_, e)| *e).collect(),
        K::Query(q) => q
            .filter
            .iter()
            .copied()
            .chain(q.take.iter().map(|(_, n)| *n))
            .collect(),
        _ => Vec::new(),
    }
}

struct Translate<'a, 'h, 'o> {
    cx: &'a Cx<'h>,
    row: hir::DefId,
    ops: &'o mut dyn Operands,
}

impl Translate<'_, '_, '_> {
    fn filter(&mut self, e: hir::ExprId) -> Lowered<Filter> {
        let hir = self.cx.hir;
        let at = span(self.cx, e);
        let unchecked = LowerError::Unchecked(at);
        let expr = hir.expr(e).ok_or(unchecked)?;
        if !reads_row(hir, e) {
            return Ok(Filter::Value(self.ops.operand(e)?));
        }
        Ok(match &expr.kind {
            hir::ExprKind::Binary { op, lhs, rhs } => {
                let cmp = match op {
                    BinaryOp::And => {
                        return Ok(Filter::And(vec![self.filter(*lhs)?, self.filter(*rhs)?]));
                    }
                    BinaryOp::Or => {
                        return Ok(Filter::Or(vec![self.filter(*lhs)?, self.filter(*rhs)?]));
                    }
                    BinaryOp::Eq => CmpOp::Eq,
                    BinaryOp::Ne => CmpOp::Ne,
                    BinaryOp::Lt => CmpOp::Lt,
                    BinaryOp::Le => CmpOp::Le,
                    BinaryOp::Gt => CmpOp::Gt,
                    BinaryOp::Ge => CmpOp::Ge,
                    BinaryOp::Add
                    | BinaryOp::Sub
                    | BinaryOp::Mul
                    | BinaryOp::Div
                    | BinaryOp::Rem => return Err(unchecked),
                };
                Filter::Cmp(
                    cmp,
                    Box::new(self.filter(*lhs)?),
                    Box::new(self.filter(*rhs)?),
                )
            }
            hir::ExprKind::Unary {
                op: UnaryOp::Not,
                operand,
            } => Filter::Not(Box::new(self.filter(*operand)?)),
            hir::ExprKind::In {
                element,
                collection,
                negated,
            } => {
                let (path, ty) = self.path(*collection)?;
                let elem = Box::new(self.filter(*element)?);
                let found = match ty {
                    hir::Ty::Set(_) => Filter::InSet { elem, set: path },
                    hir::Ty::Map(..) => Filter::InMap {
                        key: elem,
                        map: path,
                    },
                    _ => return Err(unchecked),
                };
                if *negated {
                    Filter::Not(Box::new(found))
                } else {
                    found
                }
            }
            hir::ExprKind::Index { base, index } => {
                let (map, ty) = self.path(*base)?;
                if !matches!(ty, hir::Ty::Map(..)) {
                    return Err(unchecked);
                }
                Filter::MapGet {
                    map,
                    key: Box::new(self.filter(*index)?),
                }
            }
            hir::ExprKind::Fallback { value, fallback } => {
                if reads_row(hir, *fallback) {
                    return Err(unchecked);
                }
                let value = self.filter(*value)?;
                Filter::Coalesce(Box::new(value), self.ops.operand(*fallback)?)
            }
            hir::ExprKind::Name(hir::Res::Field(..)) | hir::ExprKind::Field { .. } => {
                let (hops, field, _) = self.hops(e)?;
                match field {
                    hir::FieldRef::Implicit(hir::ImplicitField::Id) => Filter::RowId(hops),
                    field => Filter::Field(Path {
                        hops,
                        field: field_ref(field),
                    }),
                }
            }
            _ => return Err(unchecked),
        })
    }

    /// A field path that is not the row id, with the HIR type of its field.
    fn path(&self, e: hir::ExprId) -> Lowered<(Path, hir::Ty)> {
        let unchecked = LowerError::Unchecked(span(self.cx, e));
        let (hops, field, model) = self.hops(e)?;
        let hir::FieldRef::Declared(id) = field else {
            return Err(unchecked);
        };
        let ty = self.cx.hir.field(model, id).ok_or(unchecked)?.ty.clone();
        Ok((
            Path {
                hops,
                field: field_ref(field),
            },
            ty,
        ))
    }

    /// The hops, the field read at their end and the model of that field.
    fn hops(&self, e: hir::ExprId) -> Lowered<(Vec<Hop>, hir::FieldRef, hir::DefId)> {
        let cx = self.cx;
        let unchecked = LowerError::Unchecked(span(cx, e));
        match cx.hir.expr(e).map(|x| &x.kind) {
            Some(hir::ExprKind::Name(hir::Res::Field(model, field))) if *model == self.row => {
                Ok((Vec::new(), *field, *model))
            }
            Some(hir::ExprKind::Field {
                base,
                field: (model, field),
            }) => {
                let (mut hops, via, at) = self.hops(*base)?;
                let target = match via {
                    hir::FieldRef::Declared(id) => match &cx.hir.field(at, id).ok_or(unchecked)?.ty
                    {
                        hir::Ty::Ref(t) => *t,
                        hir::Ty::Optional(inner) => match **inner {
                            hir::Ty::Ref(t) => t,
                            _ => return Err(unchecked),
                        },
                        _ => return Err(unchecked),
                    },
                    hir::FieldRef::Implicit(hir::ImplicitField::Author) => {
                        cx.user_def().ok_or(unchecked)?
                    }
                    hir::FieldRef::Implicit(_) => return Err(unchecked),
                };
                if target != *model || hops.len() == MAX_HOPS {
                    return Err(unchecked);
                }
                hops.push(Hop {
                    field: field_ref(via),
                    model: *cx.models.get(&target).ok_or(unchecked)?,
                });
                Ok((hops, *field, *model))
            }
            _ => Err(unchecked),
        }
    }
}
