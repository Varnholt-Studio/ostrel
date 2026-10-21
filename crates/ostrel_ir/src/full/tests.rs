// Test code may panic on a broken invariant.
#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use ostrel_core::{FileId, Span};
use ostrel_sema::hir;

use super::query::{CmpOp, Filter, Hop, Operand, Path, QueryPlan, See};
use super::*;

fn span(at: u32) -> Span {
    Span::new(FileId::from_raw(0), at, at + 1)
}

fn inst(at: u32, kind: InstKind) -> Inst {
    Inst {
        kind,
        span: span(at),
    }
}

fn block(insts: Vec<Inst>, term: Terminator) -> Block {
    Block {
        insts,
        term,
        term_span: span(0),
    }
}

fn function(name: &str, placement: Placement, kind: FnKind, locals: Vec<Ty>) -> Function {
    Function {
        name: name.to_string(),
        placement,
        kind,
        elevated: false,
        params: 0,
        locals,
        result: Ty::Unit,
        prologue: None,
        blocks: vec![block(Vec::new(), Terminator::Return(None))],
        span: span(0),
    }
}

const ROOM: ModelId = ModelId(0);
const USER: ModelId = ModelId(1);
const NAME: FieldId = FieldId(0);
const HANDLE: FieldId = FieldId(0);
const MEMBERS: FieldId = FieldId(1);
const NUMBER: FieldId = FieldId(2);

const SEND: FuncId = FuncId(0);
const CALL_RULE: FuncId = FuncId(1);
const ON_CLICK: FuncId = FuncId(2);
const CLOSE_ALL: FuncId = FuncId(3);
const ADMIN_RULE: FuncId = FuncId(4);
const EDIT_RULE: FuncId = FuncId(5);
const LABEL: FuncId = FuncId(6);

fn members_filter() -> Filter {
    Filter::InSet {
        elem: Box::new(Filter::Value(Operand::Me)),
        set: Path {
            hops: Vec::new(),
            field: FieldRef::Declared(MEMBERS),
        },
    }
}

fn plan(see: See) -> QueryPlan {
    QueryPlan {
        model: ROOM,
        filter: Some(Filter::Cmp(
            CmpOp::Eq,
            Box::new(Filter::Field(Path {
                hops: Vec::new(),
                field: FieldRef::Declared(NAME),
            })),
            Box::new(Filter::Value(Operand::Local(Local(0)))),
        )),
        see,
        sort: Some((FieldRef::Implicit(ImplicitField::Made), false)),
        take: None,
        take_span: None,
    }
}

/// A chat sized program that keeps the whole contract:
/// * `send(r: Room, text: Text)`, a server fn with `call if me in r.members`,
///   renames the room as the caller;
/// * a client handler calls it remotely and settles with `?? ` on a fallback;
/// * `closeAll(name: Text) as system` with an admin `call` rule drops every
///   room with that name in one bulk operation;
/// * `edit name if me in members` reads the stored row in a predicate;
/// * a shared `label` function used by the handler.
fn sample() -> Program {
    let room = Ty::Ref(ROOM);
    let mut send = function(
        "send",
        Placement::Server,
        FnKind::Source,
        vec![room.clone(), Ty::Text],
    );
    send.params = 2;
    send.prologue = Some(Prologue {
        resolve: vec![Local(0)],
        call_rule: Some(RuleId(0)),
    });
    send.blocks = vec![block(
        vec![inst(
            10,
            InstKind::SetField {
                row: Local(0),
                model: ROOM,
                field: NAME,
                value: Local(1),
                auth: Auth::Caller,
            },
        )],
        Terminator::Return(None),
    )];

    let mut call_rule = function(
        "send.call",
        Placement::Shared,
        FnKind::Predicate,
        vec![
            room.clone(),
            Ty::Text,
            Ty::Set(Box::new(Ty::Ref(ROOM))),
            Ty::Optional(Box::new(room.clone())),
            Ty::Bool,
        ],
    );
    call_rule.params = 2;
    call_rule.result = Ty::Bool;
    call_rule.blocks = vec![block(
        vec![
            inst(
                20,
                InstKind::GetField {
                    dst: Local(2),
                    row: Local(0),
                    model: ROOM,
                    field: FieldRef::Declared(MEMBERS),
                    auth: Auth::Rule,
                },
            ),
            inst(21, InstKind::Me { dst: Local(3) }),
            inst(
                22,
                InstKind::Contains {
                    dst: Local(4),
                    elem: Local(3),
                    coll: Local(2),
                },
            ),
        ],
        Terminator::Return(Some(Local(4))),
    )];

    let mut on_click = function(
        "Main.button.click",
        Placement::Client,
        FnKind::Derived,
        vec![
            room.clone(),
            Ty::Text,
            Ty::Remote(Box::new(Ty::Unit)),
            Ty::Unit,
        ],
    );
    on_click.params = 1;
    on_click.blocks = vec![block(
        vec![
            inst(
                30,
                InstKind::Call {
                    dst: Some(Local(1)),
                    func: LABEL,
                    args: Vec::new(),
                },
            ),
            inst(
                31,
                InstKind::RemoteCall {
                    dst: Some(Local(2)),
                    func: SEND,
                    args: vec![Local(0), Local(1)],
                },
            ),
            inst(
                32,
                InstKind::Settle {
                    dst: Local(3),
                    remote: Local(2),
                    caught: Caught::new([CallErrorKind::Denied]),
                    fallback: Local(3),
                },
            ),
        ],
        Terminator::Return(None),
    )];

    let mut close_all = function(
        "closeAll",
        Placement::Server,
        FnKind::Source,
        vec![Ty::Text],
    );
    close_all.params = 1;
    close_all.elevated = true;
    close_all.prologue = Some(Prologue {
        resolve: Vec::new(),
        call_rule: Some(RuleId(1)),
    });
    close_all.blocks = vec![block(
        vec![inst(
            40,
            InstKind::BulkDrop {
                plan: plan(See::System),
                auth: Auth::System,
            },
        )],
        Terminator::Return(None),
    )];

    let mut admin_rule = function(
        "closeAll.call",
        Placement::Shared,
        FnKind::Predicate,
        vec![Ty::Text, Ty::Bool],
    );
    admin_rule.params = 1;
    admin_rule.result = Ty::Bool;
    admin_rule.blocks = vec![block(
        vec![inst(
            50,
            InstKind::Const {
                dst: Local(1),
                value: Const::Bool(false),
            },
        )],
        Terminator::Return(Some(Local(1))),
    )];

    let mut edit_rule = function(
        "Room.edit.name",
        Placement::Shared,
        FnKind::Predicate,
        vec![
            Ty::Set(Box::new(Ty::Ref(ROOM))),
            Ty::Optional(Box::new(room.clone())),
            Ty::Bool,
        ],
    );
    edit_rule.result = Ty::Bool;
    edit_rule.blocks = vec![block(
        vec![
            inst(
                60,
                InstKind::RuleField {
                    dst: Local(0),
                    state: RowState::Stored,
                    field: FieldRef::Declared(MEMBERS),
                },
            ),
            inst(61, InstKind::Me { dst: Local(1) }),
            inst(
                62,
                InstKind::Contains {
                    dst: Local(2),
                    elem: Local(1),
                    coll: Local(0),
                },
            ),
        ],
        Terminator::Return(Some(Local(2))),
    )];

    let mut label = function("label", Placement::Shared, FnKind::Source, vec![Ty::Text]);
    label.result = Ty::Text;
    label.blocks = vec![block(
        vec![inst(
            70,
            InstKind::Const {
                dst: Local(0),
                value: Const::Text("hello".to_string()),
            },
        )],
        Terminator::Return(Some(Local(0))),
    )];

    let field = |name: &str, ty: Ty, mode: FieldMode| Field {
        name: name.to_string(),
        ty,
        mode,
        merge: Merge::Default,
        default: None,
        span: span(1),
    };
    Program {
        app: Some(App {
            name: "Chat".to_string(),
            auth: Some(Registration::Open),
            home: None,
        }),
        enums: Vec::new(),
        models: vec![
            Model {
                name: "Room".to_string(),
                fields: vec![
                    field("name", Ty::Text, FieldMode::Plain),
                    field(
                        "members",
                        Ty::Set(Box::new(Ty::Ref(ROOM))),
                        FieldMode::Plain,
                    ),
                    field("number", Ty::Int, FieldMode::Serial(None)),
                ],
                see: Some(members_filter()),
                rules: vec![RuleId(2)],
                checks: Vec::new(),
                span: span(1),
            },
            // The std `User` of an app with `auth` (SYNTAX 4.1, D142).
            Model {
                name: "User".to_string(),
                fields: vec![
                    field("handle", Ty::Text, FieldMode::Fixed),
                    field("name", Ty::Text, FieldMode::Plain),
                ],
                see: None,
                rules: Vec::new(),
                checks: Vec::new(),
                span: span(5),
            },
        ],
        rules: vec![
            Rule {
                op: RuleOp::Call,
                field: None,
                pred: CALL_RULE,
                deltas: None,
                span: span(2),
            },
            Rule {
                op: RuleOp::Call,
                field: None,
                pred: ADMIN_RULE,
                deltas: None,
                span: span(3),
            },
            Rule {
                op: RuleOp::Edit,
                field: Some(NAME),
                pred: EDIT_RULE,
                deltas: None,
                span: span(4),
            },
        ],
        vars: Vec::new(),
        functions: vec![
            send, call_rule, on_click, close_all, admin_rule, edit_rule, label,
        ],
        views: Vec::new(),
        styles: Vec::new(),
        main: None,
    }
}

/// The problems `validate` reports after `change`, in report order.
fn problems_after(change: impl FnOnce(&mut Program)) -> Vec<Problem> {
    let mut program = sample();
    change(&mut program);
    program.validate().into_iter().map(|v| v.problem).collect()
}

fn first_inst(program: &mut Program, func: FuncId) -> &mut InstKind {
    &mut program.functions[func.index()].blocks[0].insts[0].kind
}

#[test]
fn the_sample_keeps_the_contract() {
    assert_eq!(sample().validate(), Vec::new());
}

#[test]
fn targets_split_by_placement_and_share_shared_code() {
    let program = sample();
    let ids = |target| program.target(target).map(|(id, _)| id).collect::<Vec<_>>();
    assert_eq!(
        ids(Target::Server),
        vec![SEND, CALL_RULE, CLOSE_ALL, ADMIN_RULE, EDIT_RULE, LABEL]
    );
    assert_eq!(
        ids(Target::Client),
        vec![CALL_RULE, ON_CLICK, ADMIN_RULE, EDIT_RULE, LABEL]
    );
}

#[test]
fn a_direct_call_never_crosses_the_split() {
    // Client calls a server function directly instead of remotely.
    let client_to_server = problems_after(|p| {
        p.functions[ON_CLICK.index()].blocks[0].insts[0].kind = InstKind::Call {
            dst: None,
            func: SEND,
            args: Vec::new(),
        };
    });
    assert_eq!(client_to_server, vec![Problem::CrossPlacementCall]);
    // Server calls client code.
    let server_to_client = problems_after(|p| {
        p.functions[SEND.index()].blocks[0].insts.push(inst(
            11,
            InstKind::Call {
                dst: None,
                func: ON_CLICK,
                args: Vec::new(),
            },
        ));
    });
    assert_eq!(server_to_client, vec![Problem::CrossPlacementCall]);
    // Shared code calls a placed function.
    let shared_to_server = problems_after(|p| {
        p.functions[LABEL.index()].blocks[0].insts.push(inst(
            71,
            InstKind::Call {
                dst: None,
                func: SEND,
                args: Vec::new(),
            },
        ));
    });
    assert_eq!(shared_to_server, vec![Problem::CrossPlacementCall]);
}

#[test]
fn a_remote_call_goes_from_client_to_a_server_source_function() {
    let from_server = problems_after(|p| {
        let call = p.functions[ON_CLICK.index()].blocks[0].insts[1].clone();
        p.functions[SEND.index()].blocks[0].insts.push(call);
    });
    assert_eq!(from_server, vec![Problem::ClientOnly]);
    let to_client = problems_after(|p| {
        if let InstKind::RemoteCall { func, .. } =
            &mut p.functions[ON_CLICK.index()].blocks[0].insts[1].kind
        {
            *func = LABEL;
        }
    });
    assert_eq!(to_client, vec![Problem::BadRemoteCall]);
}

#[test]
fn system_power_stays_inside_elevated_code() {
    let write = problems_after(|p| {
        if let InstKind::SetField { auth, .. } = first_inst(p, SEND) {
            *auth = Auth::System;
        }
    });
    assert_eq!(write, vec![Problem::SystemOutsideElevated]);
    let query = problems_after(|p| {
        p.functions[SEND.index()].blocks[0].insts.push(inst(
            12,
            InstKind::Query {
                dst: Local(1),
                plan: plan(See::System),
            },
        ));
    });
    assert_eq!(query, vec![Problem::SystemOutsideElevated]);
    // A bulk operation as the caller must run under the see rule.
    let bulk = problems_after(|p| {
        if let InstKind::BulkDrop { auth, .. } = first_inst(p, CLOSE_ALL) {
            *auth = Auth::Caller;
        }
    });
    assert_eq!(bulk, vec![Problem::BadBulk]);
}

#[test]
fn an_elevated_function_needs_its_call_rule() {
    let problems = problems_after(|p| {
        p.functions[CLOSE_ALL.index()].prologue = Some(Prologue {
            resolve: Vec::new(),
            call_rule: None,
        });
    });
    assert_eq!(problems, vec![Problem::BadElevation]);
    let on_client = problems_after(|p| p.functions[LABEL.index()].elevated = true);
    assert_eq!(on_client, vec![Problem::BadElevation]);
}

#[test]
fn every_server_source_function_has_a_prologue_and_only_those() {
    assert_eq!(
        problems_after(|p| p.functions[SEND.index()].prologue = None),
        vec![Problem::PrologueMismatch]
    );
    assert_eq!(
        problems_after(|p| {
            p.functions[LABEL.index()].prologue = Some(Prologue {
                resolve: Vec::new(),
                call_rule: None,
            });
        }),
        vec![Problem::PrologueMismatch]
    );
    // Only model reference parameters are resolved, and only a call rule
    // guards a call.
    assert_eq!(
        problems_after(|p| {
            p.functions[SEND.index()].prologue = Some(Prologue {
                resolve: vec![Local(1)],
                call_rule: Some(RuleId(2)),
            });
        }),
        vec![Problem::BadPrologue]
    );
}

#[test]
fn predicates_are_pure_and_read_rows_as_rules() {
    let write = problems_after(|p| {
        p.functions[EDIT_RULE.index()].blocks[0].insts.push(inst(
            63,
            InstKind::Drop {
                row: Local(1),
                model: ROOM,
                auth: Auth::Caller,
            },
        ));
    });
    assert_eq!(
        write,
        vec![Problem::WriteInPredicate, Problem::RuleOutsidePredicate]
    );
    let as_caller = problems_after(|p| {
        if let InstKind::GetField { auth, .. } = first_inst(p, CALL_RULE) {
            *auth = Auth::Caller;
        }
    });
    assert_eq!(
        as_caller,
        vec![Problem::NotPure, Problem::RuleOutsidePredicate]
    );
    let rule_input_elsewhere = problems_after(|p| {
        let read = p.functions[EDIT_RULE.index()].blocks[0].insts[0].clone();
        p.functions[SEND.index()].blocks[0].insts.push(read);
    });
    assert_eq!(rule_input_elsewhere, vec![Problem::RuleOutsidePredicate]);
}

#[test]
fn shared_code_reads_no_rows() {
    let problems = problems_after(|p| {
        p.functions[LABEL.index()].blocks[0].insts.push(inst(
            72,
            InstKind::Query {
                dst: Local(0),
                plan: plan(See::Replica),
            },
        ));
    });
    assert_eq!(problems, vec![Problem::NotPure, Problem::ClientOnly]);
}

#[test]
fn queries_carry_the_see_filter_of_their_side() {
    let replica_on_server = problems_after(|p| {
        p.functions[SEND.index()].blocks[0].insts.push(inst(
            13,
            InstKind::Query {
                dst: Local(1),
                plan: plan(See::Replica),
            },
        ));
    });
    assert_eq!(replica_on_server, vec![Problem::ClientOnly]);
    let rule_on_client = problems_after(|p| {
        p.functions[ON_CLICK.index()].blocks[0].insts.push(inst(
            33,
            InstKind::Query {
                dst: Local(1),
                plan: plan(See::Rule(members_filter())),
            },
        ));
    });
    assert_eq!(rule_on_client, vec![Problem::ServerOnly]);
}

#[test]
fn pending_and_rejected_are_never_read_on_the_server() {
    let problems = problems_after(|p| {
        if let InstKind::RuleField { field, .. } = first_inst(p, EDIT_RULE) {
            *field = FieldRef::Implicit(ImplicitField::Pending);
        }
    });
    assert_eq!(problems, vec![Problem::ClientFieldOnServer]);
    // On the client they are ordinary fields.
    let on_client = problems_after(|p| {
        p.functions[ON_CLICK.index()].blocks[0].insts.push(inst(
            34,
            InstKind::GetField {
                dst: Local(3),
                row: Local(0),
                model: ROOM,
                field: FieldRef::Implicit(ImplicitField::Rejected),
                auth: Auth::Caller,
            },
        ));
    });
    assert_eq!(on_client, Vec::new());
}

/// `field == me` on the queried row.
fn is_me(field: FieldRef, hops: Vec<Hop>) -> Filter {
    Filter::Cmp(
        CmpOp::Eq,
        Box::new(Filter::Field(Path { hops, field })),
        Box::new(Filter::Value(Operand::Me)),
    )
}

const AUTHOR: FieldRef = FieldRef::Implicit(ImplicitField::Author);

#[test]
fn filters_reach_the_implicit_fields() {
    // `see if author == me`, the rule of
    // examples/v0_2/rules_fail_closed/e_limit_after_see.ostl.
    let see_author = problems_after(|p| p.models[0].see = Some(is_me(AUTHOR, Vec::new())));
    assert_eq!(see_author, Vec::new());
    // `made` and `changed` in the `where` filter of a server query, and a hop
    // through `author`.
    let server_query = problems_after(|p| {
        let mut plan = plan(See::Rule(is_me(AUTHOR, Vec::new())));
        plan.filter = Some(Filter::And(vec![
            Filter::Cmp(
                CmpOp::Lt,
                Box::new(Filter::Field(Path {
                    hops: Vec::new(),
                    field: FieldRef::Implicit(ImplicitField::Made),
                })),
                Box::new(Filter::Field(Path {
                    hops: Vec::new(),
                    field: FieldRef::Implicit(ImplicitField::Changed),
                })),
            ),
            is_me(
                FieldRef::Declared(HANDLE),
                vec![Hop {
                    field: AUTHOR,
                    model: USER,
                }],
            ),
        ]));
        p.functions[SEND.index()].blocks[0].insts.push(inst(
            14,
            InstKind::Query {
                dst: Local(1),
                plan,
            },
        ));
    });
    assert_eq!(server_query, Vec::new());
}

#[test]
fn filter_paths_keep_the_field_mapping() {
    // The row id is `RowId`, never a path field.
    let id_path = problems_after(|p| {
        p.models[0].see = Some(is_me(FieldRef::Implicit(ImplicitField::Id), Vec::new()));
    });
    assert_eq!(id_path, vec![Problem::BadPath]);
    // Only a declared field or `author` is a reference to hop through.
    let made_hop = problems_after(|p| {
        p.models[0].see = Some(Filter::RowId(vec![Hop {
            field: FieldRef::Implicit(ImplicitField::Made),
            model: ROOM,
        }]));
    });
    assert_eq!(made_hop, vec![Problem::BadPath]);
    // `pending` exists only in the replica: not in a `see` rule, not in a
    // server query, but in a client query.
    let pending = || is_me(FieldRef::Implicit(ImplicitField::Pending), Vec::new());
    let in_see = problems_after(|p| p.models[0].see = Some(pending()));
    assert_eq!(in_see, vec![Problem::ClientFieldOnServer]);
    let in_server_query = problems_after(|p| {
        if let InstKind::BulkDrop { plan, .. } = first_inst(p, CLOSE_ALL) {
            plan.filter = Some(pending());
        }
    });
    assert_eq!(in_server_query, vec![Problem::ClientFieldOnServer]);
    let in_client_query = problems_after(|p| {
        let mut plan = plan(See::Replica);
        plan.filter = Some(pending());
        p.functions[ON_CLICK.index()].blocks[0].insts.push(inst(
            35,
            InstKind::Query {
                dst: Local(1),
                plan,
            },
        ));
    });
    assert_eq!(in_client_query, Vec::new());
}

#[test]
fn a_hop_reaches_the_model_of_its_reference() {
    let hop = |field: FieldRef, model: ModelId, read: FieldId| {
        problems_after(move |p| {
            p.models[0].fields.push(Field {
                name: "owner".to_string(),
                ty: Ty::Optional(Box::new(Ty::Ref(USER))),
                mode: FieldMode::Plain,
                merge: Merge::Default,
                default: None,
                span: span(6),
            });
            p.models[0].see = Some(is_me(FieldRef::Declared(read), vec![Hop { field, model }]));
        })
    };
    let owner = FieldRef::Declared(FieldId(3));
    // `owner: User?` and `author` point to the std `User`.
    assert_eq!(hop(owner, USER, HANDLE), Vec::new());
    assert_eq!(hop(AUTHOR, USER, HANDLE), Vec::new());
    // The hop names another model than the field points to.
    assert_eq!(hop(owner, ROOM, NAME), vec![Problem::BadPath]);
    assert_eq!(hop(AUTHOR, ROOM, NAME), vec![Problem::BadPath]);
    // `name: Text` is no reference at all.
    assert_eq!(
        hop(FieldRef::Declared(NAME), USER, HANDLE),
        vec![Problem::BadPath]
    );
    // A field the model does not have, before or after the hop.
    assert_eq!(
        hop(FieldRef::Declared(FieldId(9)), USER, HANDLE),
        vec![Problem::UnknownId]
    );
    assert_eq!(hop(owner, USER, FieldId(9)), vec![Problem::UnknownId]);
    // Without `auth` there is no `author` (G8) and no std `User`.
    let without_auth = problems_after(|p| {
        p.app.as_mut().unwrap().auth = None;
        p.models[0].see = Some(Filter::And(vec![
            is_me(AUTHOR, Vec::new()),
            Filter::RowId(vec![Hop {
                field: AUTHOR,
                model: USER,
            }]),
        ]));
    });
    assert_eq!(without_auth, vec![Problem::BadPath, Problem::BadPath]);
}

#[test]
fn a_sort_names_a_field_of_the_queried_model_never_the_id() {
    let sort = |field: FieldRef| {
        problems_after(move |p| {
            if let InstKind::BulkDrop { plan, .. } = first_inst(p, CLOSE_ALL) {
                plan.sort = Some((field, true));
            }
        })
    };
    assert_eq!(sort(FieldRef::Declared(NUMBER)), Vec::new());
    assert_eq!(
        sort(FieldRef::Implicit(ImplicitField::Id)),
        vec![Problem::BadPath]
    );
    assert_eq!(
        sort(FieldRef::Declared(FieldId(9))),
        vec![Problem::UnknownId]
    );
    let unknown_model = problems_after(|p| {
        if let InstKind::BulkDrop { plan, .. } = first_inst(p, CLOSE_ALL) {
            plan.model = ModelId(9);
        }
    });
    assert_eq!(unknown_model, vec![Problem::UnknownId]);
}

#[test]
fn a_check_is_a_predicate() {
    let checks = |make: fn(&mut Program) -> FuncId| {
        problems_after(move |p| {
            let check = make(p);
            p.models[0].checks.push(check);
        })
    };
    assert_eq!(checks(|_| EDIT_RULE), Vec::new());
    // An elevated derived function would run its body as system on every
    // write, without call rule and audit (D140 A3).
    assert_eq!(
        checks(add_elevated_derived),
        vec![Problem::RuleWithoutPredicate]
    );
    assert_eq!(checks(|_| LABEL), vec![Problem::RuleWithoutPredicate]);
    assert_eq!(checks(|_| FuncId(9999)), vec![Problem::UnknownId]);
}

#[test]
fn main_is_a_source_function_without_parameters_of_a_script() {
    let main = |app: bool, func: FuncId| {
        problems_after(move |p| {
            if !app {
                p.app = None;
            }
            p.main = Some(func);
        })
    };
    assert_eq!(main(false, LABEL), Vec::new());
    assert_eq!(main(true, LABEL), vec![Problem::BadMain]);
    assert_eq!(main(false, SEND), vec![Problem::BadMain]);
    assert_eq!(main(false, ON_CLICK), vec![Problem::BadMain]);
    assert_eq!(main(false, FuncId(99)), vec![Problem::UnknownId]);
}

/// Adds `closeAll.rows`, a function the lowering derives from the elevated
/// body of `closeAll`, and returns its id.
fn add_elevated_derived(p: &mut Program) -> FuncId {
    let mut rows = function(
        "closeAll.rows",
        Placement::Server,
        FnKind::Derived,
        vec![Ty::Text],
    );
    rows.params = 1;
    rows.elevated = true;
    rows.blocks = vec![block(
        vec![inst(
            80,
            InstKind::BulkDrop {
                plan: plan(See::System),
                auth: Auth::System,
            },
        )],
        Terminator::Return(None),
    )];
    p.functions.push(rows);
    FuncId(p.functions.len() as u32 - 1)
}

fn call(at: u32, func: FuncId) -> Inst {
    inst(
        at,
        InstKind::Call {
            dst: None,
            func,
            args: vec![Local(1)],
        },
    )
}

#[test]
fn elevation_is_entered_only_through_a_prologue() {
    // A server fn that is not elevated calls a derived elevated function: its
    // body would run as system without call rule and audit (D140 A3).
    let from_plain = problems_after(|p| {
        let rows = add_elevated_derived(p);
        p.functions[SEND.index()].blocks[0]
            .insts
            .push(call(15, rows));
    });
    assert_eq!(from_plain, vec![Problem::ElevatedWithoutEntry]);
    // From inside the elevated body it is part of that body.
    let from_elevated = problems_after(|p| {
        let rows = add_elevated_derived(p);
        p.functions[CLOSE_ALL.index()].blocks[0]
            .insts
            .push(call(41, rows));
    });
    assert_eq!(from_elevated, Vec::new());
    // An elevated source function has its prologue, which runs the call rule
    // and the audit on every entry (D98).
    let to_source = problems_after(|p| {
        p.functions[SEND.index()].blocks[0]
            .insts
            .push(call(16, CLOSE_ALL));
    });
    assert_eq!(to_source, Vec::new());
}

#[test]
fn a_make_names_every_declared_field_except_serial_ones_in_order() {
    let make = |fields: Vec<(FieldId, Local)>| {
        problems_after(move |p| {
            p.functions[SEND.index()].blocks[0].insts.push(inst(
                14,
                InstKind::Make {
                    dst: None,
                    model: ROOM,
                    fields,
                    auth: Auth::Caller,
                },
            ));
        })
    };
    assert_eq!(
        make(vec![(NAME, Local(1)), (MEMBERS, Local(1))]),
        Vec::new()
    );
    assert_eq!(make(vec![(NAME, Local(1))]), vec![Problem::IncompleteMake]);
    assert_eq!(
        make(vec![(MEMBERS, Local(1)), (NAME, Local(1))]),
        vec![Problem::IncompleteMake]
    );
    assert_eq!(
        make(vec![
            (NAME, Local(1)),
            (MEMBERS, Local(1)),
            (NUMBER, Local(1))
        ]),
        vec![Problem::IncompleteMake]
    );
}

#[test]
fn unknown_ids_are_reported_not_followed() {
    let problems = problems_after(|p| {
        p.rules[0].pred = FuncId(99);
        if let InstKind::SetField { model, .. } = first_inst(p, SEND) {
            *model = ModelId(9);
        }
    });
    assert_eq!(problems, vec![Problem::UnknownId, Problem::UnknownId]);
}

#[test]
fn settle_catches_offline_and_the_named_kinds_only() {
    let caught = Caught::new([CallErrorKind::Denied, CallErrorKind::Denied]);
    assert_eq!(
        caught.kinds(),
        &[CallErrorKind::Denied, CallErrorKind::Offline]
    );
    assert!(!caught.contains(CallErrorKind::Unavailable));
    assert!(!caught.contains(CallErrorKind::OutcomeUnknown));
    assert_eq!(Caught::new([]).kinds(), &[CallErrorKind::Offline]);
    for kind in CallErrorKind::ALL {
        assert_eq!(CallErrorKind::from_name(kind.name()), Some(kind));
    }
    assert_eq!(CallErrorKind::from_name("IntOverflow"), None);
}

#[test]
fn step_costs_follow_the_weighting_contract() {
    let l = Local(0);
    let cost = |kind: InstKind| kind.cost();
    // The calibration instruction has no allocation surcharge (D114).
    assert_eq!(
        cost(InstKind::Call {
            dst: None,
            func: SEND,
            args: Vec::new()
        }),
        Cost {
            work: Work::CalleeRegisters,
            allocs: 0
        }
    );
    // ToText and Concat keep A = 3 (D114).
    assert_eq!(cost(InstKind::ToText { dst: l, src: l }).allocs, 3);
    assert_eq!(
        cost(InstKind::Concat {
            dst: l,
            parts: Vec::new()
        }),
        Cost {
            work: Work::ConcatParts,
            allocs: 3
        }
    );
    // Equality reads bytes, arithmetic does not (D115).
    let binary = |op| InstKind::Binary {
        dst: l,
        op,
        lhs: l,
        rhs: l,
    };
    assert_eq!(cost(binary(BinOp::Eq)).work, Work::Compare);
    assert_eq!(cost(binary(BinOp::Add)).work, Work::None);
    // Every instruction whose work grows with its operands names its B.
    assert_eq!(
        cost(InstKind::Collect {
            dst: l,
            elems: Vec::new()
        })
        .work,
        Work::OperandBytes
    );
    assert_eq!(
        cost(InstKind::BulkDrop {
            plan: plan(See::System),
            auth: Auth::System
        })
        .work,
        Work::PerRow
    );
    // Rows are charged as they enter the VM.
    let next = Terminator::Next {
        iter: l,
        bind: l,
        body: BlockId(1),
        done: BlockId(2),
    };
    assert_eq!(next.cost().work, Work::Result);
    assert_eq!(Terminator::Return(None).cost().work, Work::None);
}

#[test]
fn filters_count_nodes_like_the_database_interface() {
    assert_eq!(members_filter().nodes(), 2);
    let filter = Filter::And(vec![
        members_filter(),
        Filter::Not(Box::new(members_filter())),
    ]);
    assert_eq!(filter.nodes(), 6);
}

#[test]
fn field_types_and_wire_types() {
    assert!(Ty::Map(Box::new(Ty::Text), Box::new(Ty::Ref(ROOM))).is_field_type());
    assert!(!Ty::Optional(Box::new(Ty::Optional(Box::new(Ty::Int)))).is_field_type());
    assert!(!Ty::Rows(ROOM).is_field_type());
    assert!(Ty::Unit.is_wire_type());
    assert!(!Ty::Remote(Box::new(Ty::Int)).is_wire_type());
    assert!(!Ty::Iter(Box::new(Ty::Int)).is_wire_type());
}

fn hir_program(field_ty: hir::Ty) -> hir::Program {
    let at = span(5);
    let field = |name: &str, ty: hir::Ty, mode: hir::FieldMode| hir::FieldDef {
        name: name.into(),
        span: at,
        ty,
        default: None,
        mode,
        merge: hir::Merge::Default,
    };
    hir::Program {
        defs: vec![
            hir::Def {
                name: "Issues".into(),
                span: at,
                kind: hir::DefKind::App(hir::AppDef {
                    auth: Some(hir::Registration::Invite),
                    home: None,
                }),
            },
            hir::Def {
                name: "Status".into(),
                span: at,
                kind: hir::DefKind::Enum(vec!["open".into(), "done".into()].into()),
            },
            hir::Def {
                name: "Issue".into(),
                span: at,
                kind: hir::DefKind::Model(hir::ModelDef {
                    fields: vec![
                        field("team", hir::Ty::Text, hir::FieldMode::Fixed),
                        field(
                            "status",
                            hir::Ty::Enum(hir::DefId(1)),
                            hir::FieldMode::Plain,
                        ),
                        field(
                            "number",
                            hir::Ty::Int,
                            hir::FieldMode::Serial(Some(hir::FieldRef::Declared(hir::FieldId(0)))),
                        ),
                        field("extra", field_ty, hir::FieldMode::Server),
                    ],
                    rules: Vec::new(),
                    checks: Vec::new(),
                }),
            },
        ],
        app: Some(hir::DefId(0)),
        ..hir::Program::default()
    }
}

#[test]
fn lowering_builds_the_schema_and_fails_closed() {
    let links = hir::Ty::Map(
        Box::new(hir::Ty::Text),
        Box::new(hir::Ty::Optional(Box::new(hir::Ty::Ref(hir::DefId(2))))),
    );
    let program = lower(&hir_program(links)).unwrap();
    assert_eq!(
        program.app,
        Some(App {
            name: "Issues".to_string(),
            auth: Some(Registration::Invite),
            home: None,
        })
    );
    assert_eq!(program.enums[0].variants, vec!["open", "done"]);
    let issue = &program.models[0];
    assert_eq!(issue.name, "Issue");
    let types: Vec<&Ty> = issue.fields.iter().map(|f| &f.ty).collect();
    assert_eq!(
        types,
        vec![
            &Ty::Text,
            &Ty::Enum(EnumId(0)),
            &Ty::Int,
            &Ty::Map(
                Box::new(Ty::Text),
                Box::new(Ty::Optional(Box::new(Ty::Ref(ModelId(0)))))
            ),
        ]
    );
    assert_eq!(
        issue.fields[2].mode,
        FieldMode::Serial(Some(FieldRef::Declared(FieldId(0))))
    );
    assert_eq!(issue.fields[3].mode, FieldMode::Server);
    // No rule lowered yet: nothing is readable or writable (D8).
    assert_eq!(issue.see, None);
    assert!(issue.rules.is_empty());
    assert!(program.functions.is_empty());
    assert_eq!(program.validate(), Vec::new());
}

#[test]
fn lowering_an_unchecked_program_is_an_error() {
    let at = span(5);
    assert_eq!(
        lower(&hir_program(hir::Ty::Error)),
        Err(LowerError::Unchecked(at))
    );
    // A reference to a definition that is not a model.
    assert_eq!(
        lower(&hir_program(hir::Ty::Ref(hir::DefId(1)))),
        Err(LowerError::Unchecked(at))
    );
    // A type that no field may have.
    assert_eq!(
        lower(&hir_program(hir::Ty::Rows(hir::DefId(2)))),
        Err(LowerError::Unchecked(at))
    );
    assert_eq!(lower(&hir::Program::default()), Ok(Program::default()));
}
