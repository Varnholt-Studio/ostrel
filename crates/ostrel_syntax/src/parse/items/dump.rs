//! A stable text form of a parsed module for tests and goldens.
//!
//! Every node is written as an S-expression: `(+ 1 (* 2 3))` for an
//! expression, one line per item with its parts, and an indented tree for
//! blocks and views. Ranges are not written; tests that need them check them
//! directly.

use crate::ast::{
    AppItemKind, AttrKind, BlockId, Cond, Element, ElementChild, ExprId, ExprKind, FieldMode,
    Handler, Item, MemberKind, Module, Placement, StmtId, StmtKind, StrPart, TyId, TypedParam,
    UnaryOp, ViewNodeId, ViewNodeKind,
};
use std::fmt::Write;

/// The whole module, one item after the other.
pub(in crate::parse) fn module(m: &Module) -> String {
    let mut out = String::new();
    for item in m.items() {
        item_into(m, item, &mut out);
    }
    out
}

/// An expression as an S-expression, for example `(+ 1 (* 2 3))`.
pub(in crate::parse) fn expr(m: &Module, id: ExprId) -> String {
    let Some(e) = m.expr(id) else {
        return "<missing>".into();
    };
    let list = |head: &str, ids: &[ExprId]| {
        let mut out = format!("({head}");
        for id in ids {
            out.push(' ');
            out.push_str(&expr(m, *id));
        }
        out + ")"
    };
    match &e.kind {
        ExprKind::Int(v) => v.to_string(),
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Name(name) => name.text.to_string(),
        ExprKind::Keyword(k) => k.as_str().to_string(),
        ExprKind::Str(parts) => {
            let parts: Vec<String> = parts
                .iter()
                .map(|part| match part {
                    StrPart::Text(t) => format!("{t:?}"),
                    StrPart::Interp(i) => format!("{{{}}}", expr(m, *i)),
                })
                .collect();
            format!("(str {})", parts.join(" "))
        }
        ExprKind::Unary { op, operand } => {
            let op = match op {
                UnaryOp::Neg => "neg",
                UnaryOp::Not => "not",
            };
            format!("({op} {})", expr(m, *operand))
        }
        ExprKind::Binary { op, lhs, rhs } => {
            format!("({} {} {})", op.as_str(), expr(m, *lhs), expr(m, *rhs))
        }
        ExprKind::Call { callee, args } => {
            let mut ids = vec![*callee];
            ids.extend(args.iter().copied());
            list("call", &ids)
        }
        ExprKind::Paren(inner) => format!("(paren {})", expr(m, *inner)),
        ExprKind::Field { base, name } => format!("(. {} {})", expr(m, *base), name.text),
        ExprKind::Index { base, index } => list("index", &[*base, *index]),
        ExprKind::In {
            element,
            collection,
            negated,
        } => list(
            if *negated { "not-in" } else { "in" },
            &[*element, *collection],
        ),
        ExprKind::Range { lo, hi } => list("..", &[*lo, *hi]),
        ExprKind::Catch { value, kinds } => {
            let kinds: Vec<&str> = kinds.iter().map(|k| &*k.text).collect();
            format!("(catch {} {})", expr(m, *value), kinds.join(" "))
        }
        ExprKind::Fallback { value, fallback } => list("??", &[*value, *fallback]),
        ExprKind::Set(items) => list("set", items),
        ExprKind::List(items) => list("list", items),
        ExprKind::Map(entries) => {
            let mut out = "(map".to_string();
            for entry in entries.iter() {
                let _ = write!(out, " ({} {})", expr(m, entry.key), expr(m, entry.value));
            }
            out + ")"
        }
        ExprKind::Make(make) => {
            let mut out = format!("(make {}", make.ty.text);
            for field in make.fields.iter() {
                let _ = write!(out, " ({} {})", field.name.text, expr(m, field.value));
            }
            out + ")"
        }
        ExprKind::Query(query) => {
            let mut out = format!("(query {}", query.head.text);
            if let Some(filter) = query.filter {
                let _ = write!(out, " (where {})", expr(m, filter));
            }
            if let Some(sort) = &query.sort {
                let desc = if sort.desc { " desc" } else { "" };
                let _ = write!(out, " (sort {}{desc})", sort.field.text);
            }
            if let Some(take) = &query.take {
                let _ = write!(out, " ({} {})", take.kind.as_str(), expr(m, take.count));
            }
            out + ")"
        }
    }
}

/// A type as written: `Name`, `Name[A, B]`, each optionally with `?`.
pub(in crate::parse) fn ty(m: &Module, id: TyId) -> String {
    let Some(t) = m.ty(id) else {
        return "<missing>".into();
    };
    let mut out = t.name.text.to_string();
    if !t.args.is_empty() {
        let args: Vec<String> = t.args.iter().map(|a| ty(m, *a)).collect();
        let _ = write!(out, "[{}]", args.join(", "));
    }
    if t.optional {
        out.push('?');
    }
    out
}

fn params(m: &Module, params: &[TypedParam]) -> String {
    let params: Vec<String> = params
        .iter()
        .map(|p| format!("{}: {}", p.name.text, ty(m, p.ty)))
        .collect();
    format!("({})", params.join(", "))
}

fn line(out: &mut String, depth: usize, text: &str) {
    let _ = writeln!(out, "{}{text}", "  ".repeat(depth));
}

fn item_into(m: &Module, item: &Item, out: &mut String) {
    match item {
        Item::Fn(decl) => {
            let params: Vec<String> = decl
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name.text, p.ty.name.text))
                .collect();
            let result = decl
                .result
                .as_ref()
                .map_or(String::new(), |r| format!(" -> {}", r.name.text));
            line(
                out,
                0,
                &format!("fn {}({}){result}", decl.name.text, params.join(", ")),
            );
            block_into(m, decl.body, 1, out);
        }
        Item::PlacedFn(decl) => {
            let result = decl
                .result
                .map_or(String::new(), |r| format!(" -> {}", ty(m, r)));
            let elevated = if decl.elevated.is_some() {
                " as system"
            } else {
                ""
            };
            let placement = match decl.placement {
                Placement::Inferred => "inferred",
                other => other.as_str(),
            };
            line(
                out,
                0,
                &format!(
                    "placed-fn [{placement}] {}{}{result}{elevated}",
                    decl.name.text,
                    params(m, &decl.params)
                ),
            );
            if let Some(rule) = &decl.call_rule {
                line(out, 1, &format!("(call-rule {})", expr(m, rule.cond)));
            }
            block_into(m, decl.body, 1, out);
        }
        Item::App(decl) => {
            line(out, 0, &format!("app {}", decl.name.text));
            for app_item in decl.items.iter() {
                let text = match &app_item.kind {
                    AppItemKind::Auth { scheme, mode } => match mode {
                        Some(mode) => format!("(auth {} {})", scheme.text, mode.text),
                        None => format!("(auth {})", scheme.text),
                    },
                    AppItemKind::Home(view) => format!("(home {})", view.text),
                };
                line(out, 1, &text);
            }
        }
        Item::Enum(decl) => {
            let variants: Vec<&str> = decl.variants.iter().map(|v| &*v.text).collect();
            line(
                out,
                0,
                &format!("enum {} = {}", decl.name.text, variants.join(" | ")),
            );
        }
        Item::Data(decl) => {
            line(out, 0, &format!("data {}", decl.name.text));
            for member in decl.members.iter() {
                let text = match &member.kind {
                    MemberKind::Field(field) => {
                        let mut text = format!("(field {} {}", field.name.text, ty(m, field.ty));
                        if let Some(default) = field.default {
                            let _ = write!(text, " (= {})", expr(m, default));
                        }
                        match &field.mode {
                            Some(FieldMode::Fixed) => text.push_str(" fixed"),
                            Some(FieldMode::Server) => text.push_str(" server"),
                            Some(FieldMode::Serial { per: Some(per) }) => {
                                let _ = write!(text, " (serial per {})", per.text);
                            }
                            Some(FieldMode::Serial { per: None }) => text.push_str(" serial"),
                            None => {}
                        }
                        if let Some(merge) = &field.merge {
                            let _ = write!(text, " (merge {})", merge.text);
                        }
                        text + ")"
                    }
                    MemberKind::Rule(rule) => {
                        let field = rule
                            .field
                            .as_ref()
                            .map_or(String::new(), |f| format!(" {}", f.text));
                        format!("({}{field} {})", rule.verb.as_str(), expr(m, rule.cond))
                    }
                    MemberKind::Check(cond) => format!("(check {})", expr(m, *cond)),
                };
                line(out, 1, &text);
            }
        }
        Item::Var(decl) => {
            let text = match &decl.init {
                crate::ast::VarInit::Typed { ty: t, value } => {
                    let value = value.map_or(String::new(), |v| format!(" = {}", expr(m, v)));
                    format!("var {}: {}{value}", decl.name.text, ty(m, *t))
                }
                crate::ast::VarInit::Inferred(value) => {
                    format!("var {} = {}", decl.name.text, expr(m, *value))
                }
            };
            line(out, 0, &text);
        }
        Item::View(decl) => {
            let params = decl.params.as_ref().map_or(String::new(), |p| params(m, p));
            line(out, 0, &format!("view {}{params}", decl.name.text));
            for node in decl.nodes.iter() {
                view_node_into(m, *node, 1, out);
            }
        }
        Item::Style(decl) => {
            line(
                out,
                0,
                &format!("style {}..{}", decl.body.start(), decl.body.end()),
            );
        }
    }
}

fn block_into(m: &Module, id: BlockId, depth: usize, out: &mut String) {
    let Some(block) = m.block(id) else {
        line(out, depth, "<missing block>");
        return;
    };
    for stmt in block.stmts.iter() {
        stmt_into(m, *stmt, depth, out);
    }
}

fn stmt_into(m: &Module, id: StmtId, depth: usize, out: &mut String) {
    let Some(stmt) = m.stmt(id) else {
        line(out, depth, "<missing stmt>");
        return;
    };
    match &stmt.kind {
        StmtKind::Let { name, value } => {
            line(
                out,
                depth,
                &format!("(let {} {})", name.text, expr(m, *value)),
            );
        }
        StmtKind::Return { value } => line(out, depth, &format!("(return {})", expr(m, *value))),
        StmtKind::Expr(value) => line(out, depth, &expr(m, *value)),
        StmtKind::Assign { target, value } => line(
            out,
            depth,
            &format!("(= {} {})", expr(m, *target), expr(m, *value)),
        ),
        StmtKind::Drop { source } => line(out, depth, &format!("(drop {})", expr(m, *source))),
        StmtKind::If {
            cond,
            then_block,
            else_block,
        } => {
            line(out, depth, &format!("if {}", expr(m, *cond)));
            block_into(m, *then_block, depth + 1, out);
            if let Some(else_block) = else_block {
                line(out, depth, "else");
                block_into(m, *else_block, depth + 1, out);
            }
        }
        StmtKind::IfLet {
            name,
            value,
            then_block,
            else_block,
        } => {
            line(
                out,
                depth,
                &format!("if let {} {}", name.text, expr(m, *value)),
            );
            block_into(m, *then_block, depth + 1, out);
            if let Some(else_block) = else_block {
                line(out, depth, "else");
                block_into(m, *else_block, depth + 1, out);
            }
        }
        StmtKind::For { name, source, body } => {
            line(
                out,
                depth,
                &format!("for {} {}", name.text, expr(m, *source)),
            );
            block_into(m, *body, depth + 1, out);
        }
    }
}

fn handler(m: &Module, handler: &Handler) -> String {
    match handler.assign {
        Some(value) => format!(
            "(on {} (= {} {}))",
            handler.event.text,
            expr(m, handler.action),
            expr(m, value)
        ),
        None => format!("(on {} {})", handler.event.text, expr(m, handler.action)),
    }
}

fn element_line(m: &Module, element: &Element) -> String {
    let mut text = format!("<{}", element.name.text);
    if let Some(value) = element.value {
        let _ = write!(text, " {}", expr(m, value));
    }
    for attr in element.attrs.iter() {
        match &attr.kind {
            AttrKind::Named { name, value } => {
                let _ = write!(text, " ({}: {})", name.text, expr(m, *value));
            }
            AttrKind::Style { name, cond } => match cond {
                Some(cond) => {
                    let _ = write!(text, " (~{} {})", name.text, expr(m, *cond));
                }
                None => {
                    let _ = write!(text, " ~{}", name.text);
                }
            },
        }
    }
    if let Some(on) = &element.handler {
        let _ = write!(text, " {}", handler(m, on));
    }
    text + ">"
}

fn view_node_into(m: &Module, id: ViewNodeId, depth: usize, out: &mut String) {
    let Some(node) = m.view_node(id) else {
        line(out, depth, "<missing node>");
        return;
    };
    match &node.kind {
        ViewNodeKind::Element(element) => {
            line(out, depth, &element_line(m, element));
            for child in element.children.iter() {
                match child {
                    ElementChild::Node(id) => view_node_into(m, *id, depth + 1, out),
                    ElementChild::Handler(on) => line(out, depth + 1, &handler(m, on)),
                }
            }
        }
        ViewNodeKind::If(node) => {
            let cond = match &node.cond {
                Cond::Expr(cond) => format!("if {}", expr(m, *cond)),
                Cond::Let { name, value } => format!("if let {} {}", name.text, expr(m, *value)),
            };
            line(out, depth, &cond);
            for id in node.then_nodes.iter() {
                view_node_into(m, *id, depth + 1, out);
            }
            if let Some(else_nodes) = &node.else_nodes {
                line(out, depth, "else");
                for id in else_nodes.iter() {
                    view_node_into(m, *id, depth + 1, out);
                }
            }
        }
        ViewNodeKind::For(node) => {
            line(
                out,
                depth,
                &format!("for {} {}", node.name.text, expr(m, node.source)),
            );
            for id in node.nodes.iter() {
                view_node_into(m, *id, depth + 1, out);
            }
        }
    }
}
