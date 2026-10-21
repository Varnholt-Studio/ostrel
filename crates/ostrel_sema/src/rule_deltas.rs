//! The directions a `Set` or `Map` field rule admits (D187, ARCHITECTURE
//! 5.5 and G12).
//!
//! A field rule `edit f if C` on a `Set` or `Map` field admits an op of the
//! direction `added` (or `removed`) only when `C` mentions that contextual
//! name somewhere: also under `not`, as a call argument or through `.len`. An
//! op of an unmentioned direction is denied before `C` runs, so no other atom
//! (`signed`, `me`, a constant) can admit it.
//!
//! [`rule_deltas`] is the one place that decides this. Lowering stores its
//! result in the IR, where every rule evaluator checks it before the
//! predicate, and the AC-55 warning (E0316) uses it too, so the warning and
//! the denial cannot disagree.

use crate::hir::{ContextName, DefId, DefKind, ExprId, ExprKind, Program, Res, Rule, StrPart, Ty};

/// The directions a field rule on a `Set` or `Map` field mentions, and so the
/// only directions it can admit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RuleDeltas {
    /// The condition mentions `added`.
    pub added: bool,
    /// The condition mentions `removed`.
    pub removed: bool,
}

/// The directions `rule` of `model` admits: `Some` exactly when the rule is a
/// field rule on a field of type `Set` or `Map`, `None` for every other rule
/// and for a rule whose field is not a declared field of `model`.
pub fn rule_deltas(program: &Program, model: DefId, rule: &Rule) -> Option<RuleDeltas> {
    let field = rule.field?;
    let DefKind::Model(def) = &program.def(model)?.kind else {
        return None;
    };
    let field = def.fields.get(field.index())?;
    if !matches!(field.ty, Ty::Set(_) | Ty::Map(..)) {
        return None;
    }
    Some(mentioned(program, rule.cond))
}

/// The contextual names `added` and `removed` that occur anywhere in the
/// expression `root`. An expression id that is not in `program` mentions
/// nothing, so a broken tree never admits a direction.
pub fn mentioned(program: &Program, root: ExprId) -> RuleDeltas {
    let mut out = RuleDeltas::default();
    // An explicit stack: conditions come from untrusted source text and may
    // nest deeper than the call stack allows.
    let mut todo = vec![root];
    while let Some(id) = todo.pop() {
        let Some(expr) = program.expr(id) else {
            continue;
        };
        // Exhaustive on purpose: a new kind of expression must decide here
        // which sub expressions it has.
        match &expr.kind {
            ExprKind::Name(Res::Context(ContextName::Added)) => out.added = true,
            ExprKind::Name(Res::Context(ContextName::Removed)) => out.removed = true,
            ExprKind::Name(_)
            | ExprKind::Int(_)
            | ExprKind::Bool(_)
            | ExprKind::NoneValue
            | ExprKind::Me
            | ExprKind::Now
            | ExprKind::Signed => {}
            // `check` replaces every word with a name. A word left over binds
            // nothing yet, so it mentions nothing.
            ExprKind::Word { .. } => {}
            ExprKind::Str(parts) => todo.extend(parts.iter().filter_map(|p| match p {
                StrPart::Interp(e) => Some(*e),
                StrPart::Text(_) => None,
            })),
            ExprKind::Unary { operand, .. } => todo.push(*operand),
            ExprKind::Binary { lhs, rhs, .. } => todo.extend([*lhs, *rhs]),
            ExprKind::Call { callee, args } => {
                todo.push(*callee);
                todo.extend(args.iter().copied());
            }
            ExprKind::Field { base, .. } | ExprKind::Property { base, .. } => todo.push(*base),
            ExprKind::Index { base, index } => todo.extend([*base, *index]),
            ExprKind::In {
                element,
                collection,
                ..
            } => todo.extend([*element, *collection]),
            ExprKind::Range { lo, hi } => todo.extend([*lo, *hi]),
            ExprKind::Catch { value, .. } => todo.push(*value),
            ExprKind::Fallback { value, fallback } => todo.extend([*value, *fallback]),
            ExprKind::Set(items) | ExprKind::List(items) => todo.extend(items.iter().copied()),
            ExprKind::Map(entries) => todo.extend(entries.iter().flat_map(|(k, v)| [*k, *v])),
            ExprKind::Make { fields, .. } => todo.extend(fields.iter().map(|(_, e)| *e)),
            ExprKind::Query(query) => {
                todo.extend(query.filter);
                todo.extend(query.take.iter().map(|(_, n)| *n));
            }
        }
        if out.added && out.removed {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::{Def, Expr, FieldDef, FieldId, FieldMode, Merge, ModelDef, RuleOp};
    use ostrel_core::{FileId, Span};
    use ostrel_syntax::ast::{BinaryOp, UnaryOp};

    fn span() -> Span {
        Span::point(FileId::from_raw(0), 0)
    }

    struct Tree {
        program: Program,
    }

    impl Tree {
        fn add(&mut self, kind: ExprKind) -> ExprId {
            self.program.exprs.push(Expr {
                kind,
                span: span(),
                ty: None,
            });
            ExprId(self.program.exprs.len() as u32 - 1)
        }

        fn ctx(&mut self, name: ContextName) -> ExprId {
            self.add(ExprKind::Name(Res::Context(name)))
        }
    }

    fn tree() -> Tree {
        Tree {
            program: Program::default(),
        }
    }

    #[test]
    fn a_direction_counts_anywhere_in_the_condition() {
        let mut t = tree();
        // added == {me} or signed
        let added = t.ctx(ContextName::Added);
        let me = t.add(ExprKind::Me);
        let set = t.add(ExprKind::Set(Box::new([me])));
        let eq = t.add(ExprKind::Binary {
            op: BinaryOp::Eq,
            lhs: added,
            rhs: set,
        });
        let signed = t.add(ExprKind::Signed);
        let or = t.add(ExprKind::Binary {
            op: BinaryOp::Or,
            lhs: eq,
            rhs: signed,
        });
        assert_eq!(
            mentioned(&t.program, or),
            RuleDeltas {
                added: true,
                removed: false
            }
        );
        // not removed.len == 0, through `not`, `.len` and a call argument
        let removed = t.ctx(ContextName::Removed);
        let len = t.add(ExprKind::Property {
            base: removed,
            name: "len".into(),
        });
        let callee = t.add(ExprKind::Name(Res::Error));
        let call = t.add(ExprKind::Call {
            callee,
            args: Box::new([len]),
        });
        let not = t.add(ExprKind::Unary {
            op: UnaryOp::Not,
            operand: call,
        });
        assert_eq!(
            mentioned(&t.program, not),
            RuleDeltas {
                added: false,
                removed: true
            }
        );
    }

    #[test]
    fn other_atoms_and_new_mention_no_direction() {
        let mut t = tree();
        let signed = t.add(ExprKind::Signed);
        let new = t.ctx(ContextName::New);
        let it = t.ctx(ContextName::It);
        let list = t.add(ExprKind::List(Box::new([signed, new, it])));
        assert_eq!(mentioned(&t.program, list), RuleDeltas::default());
        assert_eq!(mentioned(&t.program, ExprId(999)), RuleDeltas::default());
    }

    fn model_with(field_ty: Ty, cond: ExprId, program: &mut Program) -> (DefId, Rule) {
        let field = FieldDef {
            name: "members".into(),
            span: span(),
            ty: field_ty,
            default: None,
            mode: FieldMode::Plain,
            merge: Merge::Default,
        };
        program.defs.push(Def {
            name: "Room".into(),
            span: span(),
            kind: DefKind::Model(ModelDef {
                fields: vec![field],
                rules: Vec::new(),
                checks: Vec::new(),
            }),
        });
        let rule = Rule {
            op: RuleOp::Edit,
            field: Some(FieldId(0)),
            cond,
            span: span(),
        };
        (DefId(program.defs.len() as u32 - 1), rule)
    }

    #[test]
    fn only_field_rules_on_set_or_map_have_deltas() {
        let mut t = tree();
        let signed = t.add(ExprKind::Signed);
        let (set, rule) = model_with(Ty::Set(Box::new(Ty::Int)), signed, &mut t.program);
        // A rule that mentions neither direction admits none.
        assert_eq!(
            rule_deltas(&t.program, set, &rule),
            Some(RuleDeltas::default())
        );
        let record = Rule {
            field: None,
            ..rule.clone()
        };
        assert_eq!(rule_deltas(&t.program, set, &record), None);
        let missing = Rule {
            field: Some(FieldId(7)),
            ..rule.clone()
        };
        assert_eq!(rule_deltas(&t.program, set, &missing), None);
        let (map, rule) = model_with(
            Ty::Map(Box::new(Ty::Text), Box::new(Ty::Int)),
            signed,
            &mut t.program,
        );
        assert_eq!(
            rule_deltas(&t.program, map, &rule),
            Some(RuleDeltas::default())
        );
        let (int, rule) = model_with(Ty::Int, signed, &mut t.program);
        assert_eq!(rule_deltas(&t.program, int, &rule), None);
    }
}
