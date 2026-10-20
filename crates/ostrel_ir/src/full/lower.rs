//! Lowering of the checked HIR to the full IR (ARCHITECTURE 3, `lower`).
//!
//! Stub. The final shape is `lower(&hir::Program) -> Result<Program,
//! LowerError>`; like the v0.1 slice, an input the checker did not accept is a
//! [`LowerError`], a compiler defect, never a panic. Lowered so far: the `app`
//! name and auth mode, enums, and models with their declared fields, modes and
//! merge strategies, so the compiled schema (names and types for the wire
//! codec, D132) exists. Not lowered yet: function bodies, rules, `check`
//! invariants, defaults, views, vars and styles. A model therefore has no
//! `see` filter and no rule, which denies every read and write (D8): the stub
//! fails closed.

use std::collections::HashMap;

use ostrel_core::Span;
use ostrel_sema::hir;

use super::{App, Enum, EnumId, Field, FieldMode, FieldRef, Merge, Model, ModelId, Program};
use super::{Registration, Ty};

/// Why a HIR could not be lowered: it was not accepted by `hir::check`, or it
/// is inconsistent. The span points at the declaration where this was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowerError {
    /// A type or reference that `check` would have resolved or rejected.
    Unchecked(Span),
}

/// Lowers a program on which `hir::resolve` and `hir::check` reported no
/// error.
pub fn lower(program: &hir::Program) -> Result<Program, LowerError> {
    let mut ids = Ids::default();
    for (index, def) in program.defs.iter().enumerate() {
        let def_id = hir::DefId(index as u32);
        match &def.kind {
            hir::DefKind::Enum(_) => {
                ids.enums.insert(def_id, EnumId(ids.enums.len() as u32));
            }
            hir::DefKind::Model(_) => {
                ids.models.insert(def_id, ModelId(ids.models.len() as u32));
            }
            _ => {}
        }
    }
    let mut out = Program::default();
    for def in &program.defs {
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
                    .map(|field| ids.field(field))
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
            _ => {}
        }
    }
    Ok(out)
}

/// HIR definitions to IR ids.
#[derive(Default)]
struct Ids {
    enums: HashMap<hir::DefId, EnumId>,
    models: HashMap<hir::DefId, ModelId>,
}

impl Ids {
    fn field(&self, field: &hir::FieldDef) -> Result<Field, LowerError> {
        let unchecked = LowerError::Unchecked(field.span);
        let ty = self.ty(&field.ty).ok_or(unchecked)?;
        if !ty.is_field_type() {
            return Err(unchecked);
        }
        let mode = match field.mode {
            hir::FieldMode::Plain => FieldMode::Plain,
            hir::FieldMode::Fixed => FieldMode::Fixed,
            hir::FieldMode::Server => FieldMode::Server,
            hir::FieldMode::Serial(per) => {
                FieldMode::Serial(per.map(|f| FieldRef::Declared(super::FieldId(f.0))))
            }
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

    /// The IR type of a HIR type; `None` for `Error` and for a definition
    /// that is not an enum or model of this program.
    fn ty(&self, ty: &hir::Ty) -> Option<Ty> {
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
