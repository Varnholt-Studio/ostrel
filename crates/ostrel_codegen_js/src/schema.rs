//! The compiled schema as a JavaScript value (`export const schema`).
//!
//! Form, for the sync client and the store (D132: names on the wire,
//! positions as ids):
//!
//! ```text
//! { app: { name, auth: 'open' | 'invite' | null } | null,
//!   enums: [{ name, variants: [name, ...] }, ...],
//!   models: [{ name, fields: [{ name, type, mode, serialPer?, merge }, ...] }, ...] }
//! ```
//!
//! A type is the name of a scalar (`'Bool'`, `'Int'`, `'Float'`, `'Text'`,
//! `'Time'`, `'Bytes'`, `'Rank'`) or an object with one key: `{ enum: i }`,
//! `{ ref: i }` (an index into `enums` or `models`), `{ optional: T }`,
//! `{ set: T }`, `{ list: T }`, `{ map: [K, V] }`. `mode` is `'plain'`,
//! `'fixed'`, `'server'` or `'serial'`; a serial field adds `serialPer`, the
//! index of the declared field, `'author'`, or `null`. `merge` is `'default'`
//! or `'text'`.

use std::fmt::Write;

use ostrel_core::Span;
use ostrel_ir::full::{FieldMode, FieldRef, ImplicitField, Merge, Program, Registration, Ty};

use crate::EmitError;
use crate::lit;

/// Appends `export const schema` and the enum order table to `out`.
pub(crate) fn emit(program: &Program, out: &mut String) -> Result<(), EmitError> {
    out.push_str("export const schema = freeze({\n  app: ");
    match &program.app {
        None => out.push_str("null"),
        Some(app) => {
            out.push_str("{ name: ");
            lit::string(&app.name, out);
            out.push_str(", auth: ");
            out.push_str(match app.auth {
                None => "null",
                Some(Registration::Open) => "'open'",
                Some(Registration::Invite) => "'invite'",
            });
            out.push_str(" }");
        }
    }
    out.push_str(",\n  enums: [");
    for e in &program.enums {
        out.push_str("\n    { name: ");
        lit::string(&e.name, out);
        out.push_str(", variants: [");
        for (i, variant) in e.variants.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            lit::string(variant, out);
        }
        out.push_str("] },");
    }
    out.push_str("\n  ],\n  models: [");
    for model in &program.models {
        out.push_str("\n    { name: ");
        lit::string(&model.name, out);
        out.push_str(", fields: [");
        for field in &model.fields {
            // The generated client calls the default for a `make` without
            // the field (D116); `make` is not emitted yet.
            if field.default.is_some() {
                return Err(EmitError::Unsupported {
                    span: field.span,
                    what: "field default",
                });
            }
            out.push_str("\n      { name: ");
            lit::string(&field.name, out);
            out.push_str(", type: ");
            ty(program, &field.ty, field.span, out)?;
            out.push_str(", mode: ");
            match field.mode {
                FieldMode::Plain => out.push_str("'plain'"),
                FieldMode::Fixed => out.push_str("'fixed'"),
                FieldMode::Server => out.push_str("'server'"),
                FieldMode::Serial(per) => {
                    out.push_str("'serial', serialPer: ");
                    match per {
                        None => out.push_str("null"),
                        Some(FieldRef::Declared(id)) => {
                            if id.index() >= model.fields.len() {
                                return Err(EmitError::Invalid(field.span));
                            }
                            let _ = write!(out, "{}", id.0);
                        }
                        Some(FieldRef::Implicit(ImplicitField::Author)) => {
                            out.push_str("'author'");
                        }
                        // D136: only a fixed field or `author`; the checker
                        // rejects every other implicit field.
                        Some(FieldRef::Implicit(_)) => return Err(EmitError::Invalid(field.span)),
                    }
                }
            }
            out.push_str(", merge: ");
            out.push_str(match field.merge {
                Merge::Default => "'default'",
                Merge::Text => "'text'",
            });
            out.push_str(" },");
        }
        out.push_str("\n    ] },");
    }
    out.push_str("\n  ],\n});\n\n");
    // Position of each variant, for `<` on enum values (declaration order).
    out.push_str(
        "const ENUM_ORDER = schema.enums.map((e) => new Map(e.variants.map((v, i) => [v, i])));\n",
    );
    Ok(())
}

/// Appends the schema form of a field type.
fn ty(program: &Program, t: &Ty, span: Span, out: &mut String) -> Result<(), EmitError> {
    let invalid = EmitError::Invalid(span);
    match t {
        Ty::Bool => out.push_str("'Bool'"),
        Ty::Int => out.push_str("'Int'"),
        Ty::Float => out.push_str("'Float'"),
        Ty::Text => out.push_str("'Text'"),
        Ty::Time => out.push_str("'Time'"),
        Ty::Bytes => out.push_str("'Bytes'"),
        Ty::Rank => out.push_str("'Rank'"),
        Ty::Enum(id) => {
            if id.index() >= program.enums.len() {
                return Err(invalid);
            }
            let _ = write!(out, "{{ enum: {} }}", id.0);
        }
        Ty::Ref(id) => {
            if id.index() >= program.models.len() {
                return Err(invalid);
            }
            let _ = write!(out, "{{ ref: {} }}", id.0);
        }
        Ty::Optional(inner) => {
            out.push_str("{ optional: ");
            ty(program, inner, span, out)?;
            out.push_str(" }");
        }
        Ty::Set(inner) => {
            out.push_str("{ set: ");
            ty(program, inner, span, out)?;
            out.push_str(" }");
        }
        Ty::List(inner) => {
            out.push_str("{ list: ");
            ty(program, inner, span, out)?;
            out.push_str(" }");
        }
        Ty::Map(key, value) => {
            out.push_str("{ map: [");
            ty(program, key, span, out)?;
            out.push_str(", ");
            ty(program, value, span, out)?;
            out.push_str("] }");
        }
        // Not a field type (`Ty::is_field_type`).
        _ => return Err(invalid),
    }
    Ok(())
}
