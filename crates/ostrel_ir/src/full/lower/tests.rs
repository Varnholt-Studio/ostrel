// Test code may panic on a broken invariant.
#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use ostrel_core::FileId;
use ostrel_sema::hir;

use super::super::query::{CmpOp, Filter, Hop, Operand, Path, See};
use super::super::*;

const FILE: FileId = FileId::from_raw(1);

/// Parses, resolves and checks `src`, which must give no diagnostic, and
/// lowers it.
fn lowered(src: &str) -> Result<Program, LowerError> {
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
    assert!(checked.is_empty(), "{checked:?}");
    lower(&program)
}

fn ok(src: &str) -> Program {
    let program = lowered(src).unwrap();
    assert_eq!(program.validate(), Vec::new());
    program
}

/// The text a lowering error points at.
fn error_at(src: &str) -> (&'static str, String) {
    let (kind, span) = match lowered(src).unwrap_err() {
        LowerError::Unchecked(s) => ("unchecked", s),
        LowerError::Unsupported(s) => ("unsupported", s),
        LowerError::Contract(v) => ("contract", v.span),
    };
    (
        kind,
        src[span.start as usize..span.end as usize].to_string(),
    )
}

fn func<'p>(p: &'p Program, name: &str) -> (FuncId, &'p Function) {
    let (i, f) = p
        .functions
        .iter()
        .enumerate()
        .find(|(_, f)| f.name == name)
        .unwrap_or_else(|| panic!("no function {name}"));
    (FuncId(i as u32), f)
}

fn insts(f: &Function) -> impl Iterator<Item = &InstKind> {
    f.blocks
        .iter()
        .flat_map(|b| b.insts.iter().map(|i| &i.kind))
}

fn model<'p>(p: &'p Program, name: &str) -> (ModelId, &'p Model) {
    let i = p.models.iter().position(|m| m.name == name).unwrap();
    (ModelId(i as u32), &p.models[i])
}

fn rule_fn(p: &Program, id: RuleId) -> &Function {
    p.function(p.rule(id).unwrap().pred).unwrap()
}

const CHAT: &str = "\
app Chat
  auth paseto open

data Room
  name: Text
  members: Set[User]
  check name.len in 1..40
  see if signed
  make if members == {me}
  edit members if added == {me} or removed == {me}

data Message
  room: Room
  text: Text
  check text.len in 1..2000
  see if me in room.members
  make if me in room.members
";

const TRACKER: &str = "\
app Tracker
  auth paseto invite

enum Role = guest | member | admin
enum Status = backlog | todo | doing | done | canceled

data Team
  name: Text
  prefix: Text fixed
  roles: Map[User, Role]
  see if me in roles
  make if roles == [me: admin]
  edit name if roles[me] == admin

data Issue
  team: Team fixed
  number: Int? serial per team
  title: Text
  status: Status = backlog
  assignee: User?
  check title.len in 1..200
  check assignee == none or assignee in team.roles
  see if (team.roles[me] ?? guest) >= member
  make if (team.roles[me] ?? guest) >= member
  edit status if status != done or team.roles[me] == admin
  drop if team.roles[me] == admin

server fn closeDone(t: Team)
  call if t.roles[me] == admin
  drop Issue where team == t and status == done

server fn purge(t: Team) as system
  call if t.roles[me] == admin
  drop Issue where team == t

server fn open(t: Team, title: Text) -> Issue
  call if (t.roles[me] ?? guest) >= member
  make Issue { team: t, title: title, assignee: none }

server fn rename(i: Issue, title: Text)
  call if i.team.roles[me] == admin
  i.title = title

server fn count(t: Team) -> Int
  let n = 0
  for i in Issue where team == t sort title limit 50
    n = n + 1
  return n

client fn shown(t: Team) -> Int
  return count(t) catch Denied ?? 0

client fn close(t: Team)
  closeDone(t)

client fn local(t: Team) -> Int
  let n = 0
  for i in Issue where team == t
    n = n + 1
  return n
";

#[test]
fn every_v0_1_example_lowers_and_keeps_the_contract() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/v0_1");
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "ostl") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        let program = ok(&src);
        let main = program
            .main
            .unwrap_or_else(|| panic!("{path:?} has no main"));
        let main = program.function(main).unwrap();
        assert_eq!((main.name.as_str(), main.params), ("main", 0), "{path:?}");
        // Script functions are shared, pure code without a prologue.
        assert!(
            program
                .functions
                .iter()
                .all(|f| f.placement == Placement::Shared
                    && f.kind == FnKind::Source
                    && f.prologue.is_none())
        );
        count += 1;
    }
    assert!(count >= 13, "{count}");
}

#[test]
fn control_flow_becomes_blocks() {
    let p = ok("\
fn pick(a: Int, b: Bool) -> Int
  if b and a > 0
    return a
  else
    if a == 0 or not b
      return 1
  let c = a
  return c

fn main()
  let x = pick(2, true)
  print(\"x = {x}\")
");
    let (_, pick) = func(&p, "pick");
    // `and` and `or` branch to their right side; every block ends.
    let branches = pick
        .blocks
        .iter()
        .filter(|b| matches!(b.term, Terminator::Branch { .. }))
        .count();
    assert_eq!(branches, 4);
    let returns = pick
        .blocks
        .iter()
        .filter(|b| matches!(b.term, Terminator::Return(Some(_))))
        .count();
    assert_eq!(returns, 3);
    let (_, main) = func(&p, "main");
    assert!(insts(main).any(|k| matches!(k, InstKind::Concat { .. })));
    assert!(insts(main).any(|k| matches!(k, InstKind::ToText { .. })));
    assert!(insts(main).any(|k| matches!(k, InstKind::Print { .. })));
}

#[test]
fn code_after_return_is_not_lowered_and_a_missing_return_is_an_error() {
    let p = ok("\
fn f(a: Int) -> Int
  return a
  print(\"never\")

fn main()
  print(\"{f(1)}\")
");
    let (_, f) = func(&p, "f");
    assert!(!insts(f).any(|k| matches!(k, InstKind::Print { .. })));
    assert_eq!(
        error_at(
            "\
fn f(a: Int) -> Int
  if a > 0
    return a

fn main()
  print(\"{f(1)}\")
"
        ),
        (
            "unchecked",
            "fn f(a: Int) -> Int\n  if a > 0\n    return a".to_string()
        )
    );
}

#[test]
fn see_rules_become_filters_of_the_pushdown_subset() {
    let p = ok(CHAT);
    let (room, r) = model(&p, "Room");
    assert_eq!(r.see, Some(Filter::Value(Operand::Signed)));
    let (_, m) = model(&p, "Message");
    assert_eq!(
        m.see,
        Some(Filter::InSet {
            elem: Box::new(Filter::Value(Operand::Me)),
            set: Path {
                hops: vec![Hop {
                    field: FieldRef::Declared(FieldId(0)),
                    model: room,
                }],
                field: FieldRef::Declared(FieldId(1)),
            },
        })
    );
    // The std `User` keeps its own rules.
    let (_, user) = model(&p, "User");
    assert_eq!(user.see, Some(Filter::Value(Operand::Signed)));
    assert_eq!(user.rules.len(), 1);

    let p = ok(TRACKER);
    let (team, _) = model(&p, "Team");
    let (_, issue) = model(&p, "Issue");
    let roles = Path {
        hops: vec![Hop {
            field: FieldRef::Declared(FieldId(0)),
            model: team,
        }],
        field: FieldRef::Declared(FieldId(2)),
    };
    assert_eq!(
        issue.see,
        Some(Filter::Cmp(
            CmpOp::Ge,
            Box::new(Filter::Coalesce(
                Box::new(Filter::MapGet {
                    map: roles,
                    key: Box::new(Filter::Value(Operand::Me)),
                }),
                Operand::Const(Const::Variant(EnumId(0), 0)),
            )),
            Box::new(Filter::Value(Operand::Const(Const::Variant(EnumId(0), 1)))),
        ))
    );
    let (_, t) = model(&p, "Team");
    assert!(matches!(t.see, Some(Filter::InMap { .. })));
}

#[test]
fn a_see_rule_outside_the_subset_is_an_error() {
    let src = CHAT.replace("see if signed", "see if name.len > 3");
    assert_eq!(error_at(&src), ("unchecked", "name.len".to_string()));
    let src = CHAT.replace("see if signed", "see if signed\n  see if name == \"a\"");
    assert_eq!(
        error_at(&src),
        ("unsupported", "see if name == \"a\"".to_string())
    );
}

#[test]
fn write_rules_and_checks_are_predicates_under_rule_auth() {
    let p = ok(CHAT);
    let (_, room) = model(&p, "Room");
    let ops: Vec<(RuleOp, Option<FieldId>)> = room
        .rules
        .iter()
        .map(|&r| (p.rule(r).unwrap().op, p.rule(r).unwrap().field))
        .collect();
    assert_eq!(
        ops,
        vec![(RuleOp::Make, None), (RuleOp::Edit, Some(FieldId(1)))]
    );
    for &r in &room.rules {
        let f = rule_fn(&p, r);
        assert_eq!(
            (f.kind, f.placement, &f.result, f.params),
            (FnKind::Predicate, Placement::Shared, &Ty::Bool, 0)
        );
    }
    // `make` reads the proposed row; `edit members` reads `added` and
    // `removed` as sets.
    let make = rule_fn(&p, room.rules[0]);
    assert!(insts(make).any(|k| matches!(
        k,
        InstKind::RuleField {
            state: RowState::Proposed,
            ..
        }
    )));
    let edit = rule_fn(&p, room.rules[1]);
    let deltas: Vec<Delta> = insts(edit)
        .filter_map(|k| match k {
            InstKind::RuleDelta { dir, .. } => Some(*dir),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec![Delta::Added, Delta::Removed]);
    // A missing principal makes the rule false: `me` is unwrapped only
    // behind `IsSome`, the other branch returns `false`.
    assert!(insts(edit).any(|k| matches!(k, InstKind::IsSome { .. })));
    let denies = edit.blocks.iter().any(|b| {
        matches!(
            b.insts.as_slice(),
            [Inst { kind: InstKind::Const { dst, value: Const::Bool(false) }, .. }]
                if b.term == Terminator::Return(Some(*dst))
        )
    });
    assert!(denies);
    assert_eq!(room.checks.len(), 1);
    let check = p.function(room.checks[0]).unwrap();
    assert_eq!(check.kind, FnKind::Predicate);
    assert!(insts(check).any(|k| matches!(
        k,
        InstKind::Std {
            func: StdFn::Len,
            ..
        }
    )));

    let p = ok(TRACKER);
    let (_, issue) = model(&p, "Issue");
    let ops: Vec<RuleOp> = issue.rules.iter().map(|&r| p.rule(r).unwrap().op).collect();
    assert_eq!(ops, vec![RuleOp::Make, RuleOp::Edit, RuleOp::Drop]);
    // Rows behind references are read as rules read them.
    for &r in &issue.rules {
        let f = rule_fn(&p, r);
        assert!(
            insts(f).all(|k| !matches!(k, InstKind::GetField { auth, .. } if *auth != Auth::Rule))
        );
        assert!(insts(f).any(|k| matches!(
            k,
            InstKind::GetField {
                auth: Auth::Rule,
                ..
            }
        )));
    }
    // `edit status` reads the stored state.
    let edit = rule_fn(&p, issue.rules[1]);
    assert!(insts(edit).any(|k| matches!(
        k,
        InstKind::RuleField {
            state: RowState::Stored,
            ..
        }
    )));
    assert_eq!(issue.checks.len(), 2);
}

#[test]
fn new_in_an_edit_rule_reads_the_proposed_row() {
    let src = TRACKER.replace(
        "  edit name if roles[me] == admin\n",
        "  edit name if roles[me] == admin and new.name != \"\"\n",
    );
    let p = ok(&src);
    let (_, team) = model(&p, "Team");
    let edit = rule_fn(&p, team.rules[1]);
    let states: Vec<RowState> = insts(edit)
        .filter_map(|k| match k {
            InstKind::RuleField { state, .. } => Some(*state),
            _ => None,
        })
        .collect();
    assert_eq!(states, vec![RowState::Stored, RowState::Proposed]);
}

#[test]
fn the_prologue_resolves_model_arguments_then_checks_the_call_rule() {
    let p = ok(TRACKER);
    let (_, rename) = func(&p, "rename");
    let prologue = rename.prologue.as_ref().unwrap();
    // Step 1: only the model reference parameters, in order.
    assert_eq!(prologue.resolve, vec![Local(0)]);
    // Step 2: the `call` rule, a predicate over the same parameters.
    let rule = p.rule(prologue.call_rule.unwrap()).unwrap();
    assert_eq!((rule.op, rule.field), (RuleOp::Call, None));
    let pred = p.function(rule.pred).unwrap();
    assert_eq!(
        (pred.kind, pred.params, &pred.locals[..2]),
        (FnKind::Predicate, 2, &[Ty::Ref(ModelId(2)), Ty::Text][..])
    );
    // Two hops, both read under `Auth::Rule`.
    let reads = insts(pred)
        .filter(|k| {
            matches!(
                k,
                InstKind::GetField {
                    auth: Auth::Rule,
                    ..
                }
            )
        })
        .count();
    assert_eq!(reads, 2);
    // Step 3, the audit entry, belongs to elevated functions only.
    assert!(!rename.elevated);
    assert!(func(&p, "purge").1.elevated);
    // A server function without `call` rule denies every client call.
    let (_, count) = func(&p, "count");
    assert_eq!(
        count.prologue,
        Some(Prologue {
            resolve: vec![Local(0)],
            call_rule: None
        })
    );
    // Client functions have no prologue.
    assert_eq!(func(&p, "shown").1.prologue, None);
}

#[test]
fn writes_run_as_the_caller_and_as_system_only_when_elevated() {
    let p = ok(TRACKER);
    let (_, close) = func(&p, "closeDone");
    let bulk = insts(close)
        .find_map(|k| match k {
            InstKind::BulkDrop { plan, auth } => Some((plan, *auth)),
            _ => None,
        })
        .unwrap();
    assert_eq!(bulk.1, Auth::Caller);
    let (_, issue) = model(&p, "Issue");
    assert_eq!(bulk.0.see, See::Rule(issue.see.clone().unwrap()));
    assert!(matches!(bulk.0.filter, Some(Filter::And(_))));

    let (_, purge) = func(&p, "purge");
    let bulk = insts(purge)
        .find_map(|k| match k {
            InstKind::BulkDrop { plan, auth } => Some((plan, *auth)),
            _ => None,
        })
        .unwrap();
    assert_eq!((bulk.0.see.clone(), bulk.1), (See::System, Auth::System));

    let (_, rename) = func(&p, "rename");
    assert!(insts(rename).any(|k| matches!(
        k,
        InstKind::SetField {
            auth: Auth::Caller,
            field: FieldId(2),
            ..
        }
    )));
}

#[test]
fn a_make_names_every_field_and_fills_defaults() {
    let p = ok(TRACKER);
    let (_, open) = func(&p, "open");
    let make = insts(open)
        .find_map(|k| match k {
            InstKind::Make { fields, auth, .. } => Some((fields, *auth)),
            _ => None,
        })
        .unwrap();
    // `number` is serial and left out; `status` takes its default.
    let named: Vec<u32> = make.0.iter().map(|(f, _)| f.0).collect();
    assert_eq!(named, vec![0, 2, 3, 4]);
    assert_eq!(make.1, Auth::Caller);
    assert!(insts(open).any(|k| matches!(
        k,
        InstKind::Const {
            value: Const::Variant(EnumId(1), 0),
            ..
        }
    )));
    assert!(insts(open).any(|k| matches!(
        k,
        InstKind::Const {
            value: Const::None,
            ..
        }
    )));
    // The default is also a client function without parameters (D116).
    let (_, issue) = model(&p, "Issue");
    let default = p.function(issue.fields[3].default.unwrap()).unwrap();
    assert_eq!(
        (default.placement, default.kind, default.params),
        (Placement::Client, FnKind::Derived, 0)
    );
}

#[test]
fn queries_carry_the_see_filter_of_their_side() {
    let p = ok(TRACKER);
    let (_, issue) = model(&p, "Issue");
    let (_, count) = func(&p, "count");
    let plan = insts(count)
        .find_map(|k| match k {
            InstKind::Query { plan, .. } => Some(plan),
            _ => None,
        })
        .unwrap();
    assert_eq!(plan.see, See::Rule(issue.see.clone().unwrap()));
    assert_eq!(plan.sort, Some((FieldRef::Declared(FieldId(2)), false)));
    assert!(matches!(plan.take, Some((query::Take::First, _))));
    assert!(plan.take_span.is_some());
    assert!(
        count
            .blocks
            .iter()
            .any(|b| matches!(b.term, Terminator::Next { .. }))
    );

    let (_, local) = func(&p, "local");
    let plan = insts(local)
        .find_map(|k| match k {
            InstKind::Query { plan, .. } => Some(plan),
            _ => None,
        })
        .unwrap();
    assert_eq!(plan.see, See::Replica);

    // Without a `see` rule a server query reads nothing (D8).
    let src = TRACKER.replace("  see if (team.roles[me] ?? guest) >= member\n", "");
    let p = ok(&src);
    let (_, count) = func(&p, "count");
    assert!(
        insts(count).any(|k| matches!(k, InstKind::Query { plan, .. } if plan.see == See::DenyAll))
    );
}

#[test]
fn a_client_calls_a_server_function_remotely() {
    let p = ok(TRACKER);
    let (count_id, _) = func(&p, "count");
    let (_, shown) = func(&p, "shown");
    assert!(insts(shown).any(|k| matches!(
        k,
        InstKind::RemoteCall { dst: Some(_), func, .. } if *func == count_id
    )));
    let caught = insts(shown)
        .find_map(|k| match k {
            InstKind::Settle { caught, .. } => Some(caught.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        caught.kinds(),
        &[CallErrorKind::Denied, CallErrorKind::Offline]
    );
    let (_, close) = func(&p, "close");
    assert!(insts(close).any(|k| matches!(k, InstKind::RemoteCall { dst: None, .. })));
    assert!(!insts(close).any(|k| matches!(k, InstKind::Call { .. })));
}

#[test]
fn what_is_not_lowered_yet_is_an_error_never_a_silent_gap() {
    // Views.
    let src = format!("{CHAT}\nview Main\n  text \"hi\"\n");
    assert_eq!(error_at(&src).0, "unsupported");
    // `me` as a row outside a predicate.
    let src = format!(
        "{CHAT}\nserver fn mine() -> Room\n  make Room {{ name: \"a\", members: {{me}} }}\n"
    );
    assert_eq!(error_at(&src), ("unsupported", "me".to_string()));
    // `sort id desc` (SPEC 12.10 does not say whether `sort id` is source).
    let src = TRACKER.replace("sort title limit 50", "sort id desc");
    assert_eq!(error_at(&src).0, "unsupported");
}

#[test]
fn sort_id_ascending_is_the_order_without_sort() {
    let src = TRACKER.replace("sort title limit 50", "sort id");
    let p = ok(&src);
    let (_, count) = func(&p, "count");
    assert!(insts(count).any(|k| matches!(k, InstKind::Query { plan, .. } if plan.sort.is_none())));
}

#[test]
fn an_unplaced_function_that_reads_rows_breaks_the_contract() {
    // ASSUMPTION until the checker infers placement (T218): unmarked is
    // shared, and shared code reads no rows.
    let src = TRACKER.replace("client fn local", "fn local");
    let err = lowered(&src).unwrap_err();
    assert!(
        matches!(err, LowerError::Contract(v) if v.problem == Problem::NotPure),
        "{err:?}"
    );
}

#[test]
fn a_call_rule_on_a_function_off_the_server_is_an_error() {
    let src = TRACKER.replace(
        "client fn close(t: Team)\n",
        "client fn close(t: Team)\n  call if signed\n",
    );
    assert_eq!(error_at(&src), ("unchecked", "call if signed".to_string()));
}

#[test]
fn vars_are_client_state_with_an_initialiser() {
    let p = ok("\
app Notes

var count: Int = 1

client fn bump()
  count = count + 1
");
    assert_eq!(p.vars.len(), 1);
    let init = p.function(p.vars[0].init.unwrap()).unwrap();
    assert_eq!(
        (init.placement, init.kind, init.params, &init.result),
        (Placement::Client, FnKind::Derived, 0, &Ty::Int)
    );
    let (_, bump) = func(&p, "bump");
    assert!(insts(bump).any(|k| matches!(k, InstKind::LoadVar { .. })));
    assert!(insts(bump).any(|k| matches!(k, InstKind::StoreVar { .. })));
}

#[test]
fn and_and_or_run_their_right_side_only_when_it_decides() {
    let p = ok("\
fn both(a: Bool, b: Bool) -> Bool
  a and b

fn either(a: Bool, b: Bool) -> Bool
  a or b

fn main()
  print(\"{both(true, false)} {either(true, false)}\")
");
    for (name, right_on_true) in [("both", true), ("either", false)] {
        let (_, f) = func(&p, name);
        let Terminator::Branch {
            cond,
            then,
            otherwise,
        } = f.blocks[0].term
        else {
            panic!("{name}: {:?}", f.blocks[0].term);
        };
        assert_eq!(cond, Local(0), "{name}");
        let right = if right_on_true { then } else { otherwise };
        let join = if right_on_true { otherwise } else { then };
        let right = f.block(right).unwrap();
        assert!(
            matches!(
                right.insts[..],
                [Inst {
                    kind: InstKind::Copy { src: Local(1), .. },
                    ..
                }]
            ),
            "{name}"
        );
        assert_eq!(right.term, Terminator::Jump(join), "{name}");
    }
}
