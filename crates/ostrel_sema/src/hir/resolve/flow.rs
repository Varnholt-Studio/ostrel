//! Paths through a function body that end without `return` (SPEC 12.6
//! E0303, E0305). The check needs only the shape of the body, and the name
//! of the function, which is where E0305 stands, is known only here.

use crate::hir::{BlockId, ExprId, Program, StmtKind};

/// What a function body with a result type lacks, if anything.
pub(super) enum Missing {
    /// A path ends in this expression statement in a body with branches
    /// (E0303: add `return`).
    Implicit(ExprId),
    /// A path ends without a value (E0305).
    Return,
}

/// Checks the body of a function with a result type. Without branches the
/// last expression statement is the result (SYNTAX 4.10).
pub(super) fn missing_return(p: &Program, body: BlockId) -> Option<Missing> {
    let stmts = p.block(body).map_or(&[][..], |b| &b.stmts);
    let branches = stmts.iter().any(|&s| {
        matches!(
            p.stmt(s).map(|st| &st.kind),
            Some(StmtKind::If { .. } | StmtKind::IfLet { .. } | StmtKind::For { .. })
        )
    });
    let last = stmts.last().and_then(|&s| p.stmt(s)).map(|s| &s.kind);
    if (!branches && matches!(last, Some(StmtKind::Expr(_)))) || always_returns(p, body) {
        return None;
    }
    match falling_expr(p, body) {
        Some(e) if branches => Some(Missing::Implicit(e)),
        _ => Some(Missing::Return),
    }
}

/// True if every path through the block ends in `return`.
fn always_returns(p: &Program, block: BlockId) -> bool {
    let Some(block) = p.block(block) else {
        return true;
    };
    block
        .stmts
        .iter()
        .any(|&s| match p.stmt(s).map(|st| &st.kind) {
            Some(StmtKind::Return(_)) => true,
            Some(
                &StmtKind::If {
                    then_block,
                    else_block: Some(else_block),
                    ..
                }
                | &StmtKind::IfLet {
                    then_block,
                    else_block: Some(else_block),
                    ..
                },
            ) => always_returns(p, then_block) && always_returns(p, else_block),
            _ => false,
        })
}

/// The trailing expression statement of a path that falls off the end of
/// `block`, if that path ends in one.
fn falling_expr(p: &Program, block: BlockId) -> Option<ExprId> {
    let last = p.block(block)?.stmts.last().copied()?;
    match p.stmt(last)?.kind {
        StmtKind::Expr(e) => Some(e),
        StmtKind::If {
            then_block,
            else_block,
            ..
        }
        | StmtKind::IfLet {
            then_block,
            else_block,
            ..
        } => {
            if !always_returns(p, then_block)
                && let Some(e) = falling_expr(p, then_block)
            {
                return Some(e);
            }
            let else_block = else_block?;
            if always_returns(p, else_block) {
                None
            } else {
                falling_expr(p, else_block)
            }
        }
        _ => None,
    }
}
