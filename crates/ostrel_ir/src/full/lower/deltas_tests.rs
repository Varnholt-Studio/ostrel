// Test code may panic on a broken invariant.
#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

//! The directions of `Set` and `Map` field rules in the IR (D187).

use ostrel_core::FileId;
use ostrel_sema::hir;

use super::super::*;

const FILE: FileId = FileId::from_raw(1);

/// Parses, resolves, checks and lowers `src`, which must give no diagnostic
/// and a program that passes [`Program::validate`].
fn ok(src: &str) -> Program {
    let (tokens, lexed) = ostrel_syntax::lex::lex(src, FILE);
    let (module, parsed) = ostrel_syntax::parse::parse(src, FILE, &tokens, &lexed);
    assert!(
        lexed.is_empty() && parsed.is_empty(),
        "{lexed:?} {parsed:?}"
    );
    let (mut program, resolved) = hir::resolve(&[hir::Source {
        file: FILE,
        text: src,
        module: &module,
    }]);
    assert!(resolved.is_empty(), "{resolved:?}");
    let checked = hir::check(&mut program);
    assert!(
        checked
            .iter()
            .all(|d| d.severity != ostrel_core::Severity::Error),
        "{checked:?}"
    );
    let program = lower(&program).unwrap();
    assert_eq!(program.validate(), Vec::new());
    program
}

/// A chat room whose `members` field has the rule `edit members if <cond>`,
/// and a `Map` field `roles` with `edit roles if <roles>`.
fn room(cond: &str, roles: &str) -> String {
    format!(
        "\
app Chat
  auth paseto open

enum Role = guest | admin

data Room
  name: Text
  members: Set[User]
  roles: Map[User, Role]
  see if signed
  make if members == {{me}}
  edit name if signed
  edit members if {cond}
  edit roles if {roles}
"
    )
}

/// The rules of `Room` in source order.
fn rules(p: &Program) -> Vec<&Rule> {
    let room = p.models.iter().find(|m| m.name == "Room").unwrap();
    room.rules.iter().map(|&r| p.rule(r).unwrap()).collect()
}

fn deltas(added: bool, removed: bool) -> Option<Deltas> {
    Some(Deltas { added, removed })
}

#[test]
fn an_unmentioned_direction_is_never_admitted() {
    // The hijack of G12: `or signed` must not admit any removal.
    let p = ok(&room("added == {me} or signed", "removed == [me: guest]"));
    let r = rules(&p);
    assert_eq!(r[2].field, Some(FieldId(1)));
    assert_eq!(r[2].deltas, deltas(true, false));
    assert!(r[2].deltas.unwrap().admits(Delta::Added));
    assert!(!r[2].deltas.unwrap().admits(Delta::Removed));
    // `Map`: a value change needs both directions, this rule admits only
    // the removal step.
    assert_eq!(r[3].field, Some(FieldId(2)));
    assert_eq!(r[3].deltas, deltas(false, true));
}

#[test]
fn a_rule_without_a_direction_admits_no_op() {
    let p = ok(&room("signed", "signed"));
    let r = rules(&p);
    for rule in &r[2..] {
        assert_eq!(rule.deltas, deltas(false, false));
        assert!(!rule.deltas.unwrap().admits(Delta::Added));
        assert!(!rule.deltas.unwrap().admits(Delta::Removed));
    }
}

#[test]
fn a_direction_counts_under_not_and_in_operands() {
    let p = ok(&room(
        "not (me in removed) or added == {me}",
        "added == [me: guest] and not (removed == [me: admin])",
    ));
    let r = rules(&p);
    assert_eq!(r[2].deltas, deltas(true, true));
    assert_eq!(r[3].deltas, deltas(true, true));
}

#[test]
fn only_collection_field_rules_carry_deltas() {
    let p = ok(&room("added == {me}", "removed == [me: guest]"));
    let r = rules(&p);
    // `make` and `edit name` are no collection field rules.
    assert_eq!((r[0].op, r[0].deltas), (RuleOp::Make, None));
    assert_eq!((r[1].op, r[1].deltas), (RuleOp::Edit, None));
}

/// The first program with a `members` rule that reads both directions.
fn both() -> Program {
    ok(&room("added == {me} or removed == {me}", "signed"))
}

fn members_rule(p: &Program) -> usize {
    let room = p.models.iter().find(|m| m.name == "Room").unwrap();
    room.rules[2].index()
}

fn problems(p: &Program) -> Vec<Problem> {
    p.validate().into_iter().map(|v| v.problem).collect()
}

#[test]
fn validate_refuses_a_predicate_that_reads_an_unadmitted_direction() {
    let mut p = both();
    let i = members_rule(&p);
    p.rules[i].deltas = deltas(true, false);
    assert_eq!(problems(&p), vec![Problem::BadDeltas]);
    p.rules[i].deltas = deltas(false, false);
    assert_eq!(problems(&p), vec![Problem::BadDeltas]);
}

#[test]
fn validate_refuses_deltas_on_the_wrong_rules() {
    // Missing on a collection field rule.
    let mut p = both();
    let i = members_rule(&p);
    p.rules[i].deltas = None;
    assert!(problems(&p).contains(&Problem::BadDeltas));
    // Present on a rule of a plain field.
    let mut p = both();
    let room = p.models.iter().find(|m| m.name == "Room").unwrap();
    let name = room.rules[1].index();
    p.rules[name].deltas = deltas(false, false);
    assert_eq!(problems(&p), vec![Problem::BadDeltas]);
    // Present on a record rule.
    let mut p = both();
    let room = p.models.iter().find(|m| m.name == "Room").unwrap();
    let make = room.rules[0].index();
    p.rules[make].deltas = deltas(true, true);
    assert_eq!(problems(&p), vec![Problem::BadDeltas]);
}

#[test]
fn validate_refuses_a_direction_read_in_a_check() {
    let mut p = both();
    let i = members_rule(&p);
    let pred = p.rules[i].pred;
    let room = p.models.iter().position(|m| m.name == "Room").unwrap();
    p.models[room].checks.push(pred);
    assert!(problems(&p).contains(&Problem::BadDeltas));
}
