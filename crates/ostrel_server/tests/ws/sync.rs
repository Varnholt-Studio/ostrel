//! The op path of a session: one `Ops` batch under the write lock (ARCHITECTURE 5.4, 5.5, 5.9;
//! D106, D108, D110, D159, D182, D186).
//!
//! The rules of the compiled program are hand written here ([`Rules`]): a `make` inserts the
//! row, a `set` of `title` updates it, a title `deny` is `Denied` and a title `bad` is
//! `Invalid`. Every test runs on the memory driver, which never waits.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ostrel_core::value::Value;
use ostrel_db::api::{
    Connection, DbError, Driver, Expr, Hlc, OpId, Query, ReplicaId, RowId, ServerSeq, Transaction,
    Write,
};
use ostrel_db_memory::MemoryDriver;
use ostrel_server::ws::FifoLock;
use ostrel_server::ws::hello::PrincipalKey;
use ostrel_server::ws::ops::{
    BatchFault, BatchSink, FIELD_AUTHOR, FIELD_CHANGED, FIELD_MADE, OpRules, OpServer, OpSession,
};
use ostrel_sync::protocol::{
    Ack, ClientMsg, DecodedOp, FieldId, FnId, LIMITS, ModelId, Op, OpBody, OpFault, RejectReason,
    ServerMsg,
};
use ostrel_sync::wire::{WireSchema, WireTy, decode_client};
use ostrel_vm::host::BoxFuture;

use super::block_on;

// ---------------------------------------------------------------------------------------------
// Schema and rules
// ---------------------------------------------------------------------------------------------

const NOTE: ModelId = 0;
const TITLE: FieldId = 0;

struct Schema;

impl WireSchema for Schema {
    fn model_id(&self, name: &str) -> Option<ModelId> {
        (name == "Note").then_some(NOTE)
    }
    fn model_name(&self, model: ModelId) -> Option<&str> {
        (model == NOTE).then_some("Note")
    }
    fn field_id(&self, model: ModelId, name: &str) -> Option<FieldId> {
        (model == NOTE && name == "title").then_some(TITLE)
    }
    fn field_name(&self, model: ModelId, field: FieldId) -> Option<&str> {
        (model == NOTE && field == TITLE).then_some("title")
    }
    fn field_ty(&self, model: ModelId, field: FieldId) -> Option<&WireTy> {
        (model == NOTE && field == TITLE).then_some(&WireTy::Text)
    }
    fn function_id(&self, _: &str) -> Option<FnId> {
        None
    }
    fn function_name(&self, _: FnId) -> Option<&str> {
        None
    }
    fn function_params(&self, _: FnId) -> Option<&[WireTy]> {
        None
    }
    fn function_result(&self, _: FnId) -> Option<&WireTy> {
        None
    }
}

/// Switches of the hand written rules.
#[derive(Default)]
struct Switch {
    /// `apply` also writes the implicit field `made` itself.
    write_made: Cell<bool>,
    /// `apply` commits a write on a side connection first, so the batch commit fails.
    conflict: Cell<bool>,
    side: RefCell<Option<Box<dyn Connection>>>,
}

struct Rules(Rc<Switch>);

fn title_of(op: &Op) -> Option<&str> {
    match &op.body {
        OpBody::Make { fields } => fields.iter().find_map(|(f, v)| match (f, v) {
            (&TITLE, Value::Text(t)) => Some(t.as_str()),
            _ => None,
        }),
        OpBody::Set {
            field: TITLE,
            value: Value::Text(t),
        } => Some(t.as_str()),
        _ => None,
    }
}

async fn version_of<'c>(
    tx: &mut (dyn Transaction<'c> + 'c),
    row: RowId,
) -> Result<Option<u64>, DbError> {
    let q = Query {
        model: NOTE,
        filter: Some(Expr::Cmp(
            ostrel_db::api::CmpOp::Eq,
            Box::new(Expr::RowId(Vec::new())),
            Box::new(Expr::Const(Value::Ref(row))),
        )),
        order: Vec::new(),
        limit: None,
        after: None,
    };
    Ok(tx.query(&q).await?.0.first().map(|r| r.version))
}

impl OpRules for Rules {
    fn check<'a, 'c>(
        &'a self,
        tx: &'a mut (dyn Transaction<'c> + 'c),
        _: Option<RowId>,
        op: &'a Op,
    ) -> BoxFuture<'a, Result<Option<RejectReason>, DbError>> {
        Box::pin(async move {
            if title_of(op) == Some("bad") {
                return Ok(Some(RejectReason::Invalid));
            }
            let exists = version_of(tx, op.row).await?.is_some();
            Ok(match op.body {
                OpBody::Make { .. } if exists => Some(RejectReason::Conflict),
                OpBody::Make { .. } => None,
                _ if !exists => Some(RejectReason::Invalid),
                _ => None,
            })
        })
    }

    fn apply<'a, 'c>(
        &'a self,
        tx: &'a mut (dyn Transaction<'c> + 'c),
        _: Option<RowId>,
        op: &'a Op,
    ) -> BoxFuture<'a, Result<Result<Vec<Write>, RejectReason>, DbError>> {
        Box::pin(async move {
            if title_of(op) == Some("deny") {
                return Ok(Err(RejectReason::Denied));
            }
            if self.0.conflict.get() {
                let taken = self.0.side.borrow_mut().take();
                if let Some(mut side) = taken {
                    let mut other = side.begin().await?;
                    other.next_in_sequence("other").await?;
                    other.commit().await?;
                    *self.0.side.borrow_mut() = Some(side);
                }
            }
            let mut fields = match &op.body {
                OpBody::Make { fields } => fields.clone(),
                OpBody::Set { field, value } => vec![(*field, value.clone())],
                other => panic!("the test schema has no body {other:?}"),
            };
            if self.0.write_made.get() {
                fields.push((FIELD_MADE, Value::Text("forged".to_owned())));
            }
            let write = match &op.body {
                OpBody::Make { .. } => Write::Insert {
                    model: NOTE,
                    row: op.row,
                    fields,
                    collections: Vec::new(),
                },
                _ => Write::Update {
                    model: NOTE,
                    row: op.row,
                    expect_version: version_of(tx, op.row).await?.unwrap_or(0),
                    fields,
                    collections: Vec::new(),
                },
            };
            Ok(Ok(vec![write]))
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Server, sink and ops
// ---------------------------------------------------------------------------------------------

/// What the batch handed to the sink, and whether it held the write lock at that moment.
#[derive(Default)]
struct Sink {
    events: Vec<(&'static str, bool)>,
    committed: Vec<ServerSeq>,
    frames: Vec<ServerMsg>,
    held: Option<Rc<FifoLock<Box<dyn Connection>>>>,
}

impl Sink {
    fn held(&self) -> bool {
        self.held.as_ref().is_some_and(|l| l.is_held())
    }
}

impl BatchSink for Sink {
    fn committed(&mut self, last: ServerSeq) {
        let held = self.held();
        self.events.push(("committed", held));
        self.committed.push(last);
    }

    fn outcomes(&mut self, frames: Vec<ServerMsg>) {
        let held = self.held();
        self.events.push(("outcomes", held));
        self.frames.extend(frames);
    }
}

struct Setup {
    server: OpServer,
    writer: Rc<FifoLock<Box<dyn Connection>>>,
    reader: Box<dyn Connection>,
    switch: Rc<Switch>,
}

const ME: u64 = 7;
const ISSUED_MS: u64 = 1_000;
const NOW_MS: u64 = 100_000;

fn setup() -> Setup {
    let driver = MemoryDriver::new();
    let connect = || block_on(driver.connect("memory:")).unwrap_or_else(|e| panic!("{e:?}"));
    let switch = Rc::new(Switch::default());
    *switch.side.borrow_mut() = Some(connect());
    Setup {
        server: OpServer::new(Box::new(Rules(Rc::clone(&switch)))),
        writer: Rc::new(FifoLock::new(connect())),
        reader: connect(),
        switch,
    }
}

fn user() -> RowId {
    RowId::from_hex("000000000000000000000000000000aa").unwrap_or_else(|e| panic!("{e:?}"))
}

fn session(principal: Option<RowId>, auth: bool) -> OpSession {
    OpSession {
        replica: ReplicaId(ME),
        principal,
        auth,
        key: PrincipalKey::Ip(std::net::IpAddr::from([127, 0, 0, 1])),
        floor: hlc(ISSUED_MS, 0, ME),
    }
}

fn hlc(ms: u64, counter: u16, replica: u64) -> Hlc {
    Hlc::new(ms, counter, ReplicaId(replica)).unwrap_or_else(|e| panic!("{e:?}"))
}

fn id(seq: u32) -> OpId {
    OpId {
        replica: ReplicaId(ME),
        seq,
    }
}

fn row(n: u16) -> RowId {
    RowId::new(hlc(2_000, n, ME))
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

fn make(seq: u32, row_no: u16, title: &str) -> DecodedOp {
    DecodedOp::Valid(Op {
        id: id(seq),
        hlc: hlc(50_000, u16::try_from(seq).unwrap_or(0), ME),
        model: NOTE,
        row: row(row_no),
        body: OpBody::Make {
            fields: vec![(TITLE, text(title))],
        },
    })
}

fn set(seq: u32, row_no: u16, title: &str) -> DecodedOp {
    DecodedOp::Valid(Op {
        id: id(seq),
        hlc: hlc(60_000, u16::try_from(seq).unwrap_or(0), ME),
        model: NOTE,
        row: row(row_no),
        body: OpBody::Set {
            field: TITLE,
            value: text(title),
        },
    })
}

fn invalid(seq: u32) -> DecodedOp {
    DecodedOp::Invalid {
        id: id(seq),
        hlc: hlc(50_000, 0, ME),
        row: row(999),
        fault: OpFault::UnknownField("nope".to_owned()),
    }
}

fn run(s: &Setup, who: &OpSession, ops: &[DecodedOp], now: u64) -> (Sink, Result<(), BatchFault>) {
    let mut sink = Sink {
        held: Some(Rc::clone(&s.writer)),
        ..Sink::default()
    };
    let end = block_on(s.server.batch(&s.writer, &Schema, who, ops, now, &mut sink));
    assert!(!s.writer.is_held(), "the batch released the write lock");
    (sink, end.map(|_| ()))
}

fn reject(seq: u32, reason: RejectReason) -> ServerMsg {
    ServerMsg::Reject {
        op: id(seq),
        reason,
    }
}

/// The kinds of `frames` in order, with the op each names.
fn kinds(frames: &[ServerMsg]) -> Vec<(&'static str, u32)> {
    frames
        .iter()
        .map(|f| match f {
            ServerMsg::Reject { op, .. } => ("reject", op.seq),
            ServerMsg::Defer { op, .. } => ("defer", op.seq),
            ServerMsg::Ack(ack) => ("ack", ack.op.seq),
            other => panic!("not an outcome: {other:?}"),
        })
        .collect()
}

fn rows(s: &mut Setup) -> Vec<ostrel_db::api::Row> {
    block_on(async {
        let mut tx = s.reader.begin().await.unwrap_or_else(|e| panic!("{e:?}"));
        let q = Query {
            model: NOTE,
            filter: None,
            order: Vec::new(),
            limit: None,
            after: None,
        };
        let rows = tx.query(&q).await.unwrap_or_else(|e| panic!("{e:?}"));
        let _ = tx.rollback().await;
        rows.0
    })
}

// ---------------------------------------------------------------------------------------------
// Order of the outcomes (D182)
// ---------------------------------------------------------------------------------------------

/// SPEC 12.14 `ac_57_outcome_order`, ARCHITECTURE 5.4 "Outcomes in the stream": a batch with a
/// replay, a `Reject` in the middle and a `Defer` near its end gives every `Reject`, then the
/// `Defer`, then the replay `Ack` and the cumulative `Ack` last.
#[test]
fn ac_57_outcome_order() {
    let s = setup();
    let me = session(None, false);
    // 199 ops leave one token of the burst of 200 at this instant.
    let first: Vec<DecodedOp> = (1..=199)
        .map(|seq| make(seq, u16::try_from(seq).unwrap_or(0), "a"))
        .collect();
    let (sink, end) = run(&s, &me, &first, NOW_MS);
    assert_eq!(end, Ok(()));
    assert_eq!(sink.frames.len(), 1, "only the cumulative ack");

    let batch = [
        make(5, 5, "a"),  // replay of an accepted op
        set(200, 1, "b"), // takes the last token: accepted
        invalid(201),     // rejected before the rate
        set(202, 1, "c"), // over the rate: deferred
        set(203, 1, "d"), // after the defer: undecided
    ];
    let (sink, end) = run(&s, &me, &batch, NOW_MS);
    assert_eq!(end, Ok(()));
    assert_eq!(
        kinds(&sink.frames),
        vec![("reject", 201), ("defer", 202), ("ack", 5), ("ack", 200)]
    );
    let [
        _,
        ServerMsg::Defer { retry_after_ms, .. },
        _,
        ServerMsg::Ack(last),
    ] = sink.frames.as_slice()
    else {
        panic!("{:?}", sink.frames)
    };
    assert_eq!(*retry_after_ms, 20, "50 ops per s: one token every 20 ms");
    assert_eq!(last.seq, ServerSeq(200));

    // The deferred ops were not decided: resent after the delay they are accepted.
    let (sink, end) = run(
        &s,
        &me,
        &[set(202, 1, "c"), set(203, 1, "d")],
        NOW_MS + 1_000,
    );
    assert_eq!(end, Ok(()));
    assert_eq!(kinds(&sink.frames), vec![("ack", 203)]);
}

#[test]
fn every_reject_stands_before_the_ack_that_covers_later_ops() {
    let s = setup();
    let me = session(None, false);
    let batch = [
        make(1, 1, "a"),
        make(2, 2, "deny"),
        make(3, 3, "a"),
        make(4, 1, "a"), // conflict: the row exists
        make(5, 5, "bad"),
        make(6, 6, "a"),
    ];
    let (sink, end) = run(&s, &me, &batch, NOW_MS);
    assert_eq!(end, Ok(()));
    assert_eq!(
        sink.frames[..3],
        [
            reject(2, RejectReason::Denied),
            reject(4, RejectReason::Conflict),
            reject(5, RejectReason::Invalid),
        ]
    );
    assert_eq!(kinds(&sink.frames[3..]), vec![("ack", 6)]);
}

#[test]
fn the_fence_push_and_the_outcomes_happen_under_the_write_lock() {
    let s = setup();
    let (sink, end) = run(
        &s,
        &session(None, false),
        &[make(1, 1, "a"), make(2, 2, "a")],
        NOW_MS,
    );
    assert_eq!(end, Ok(()));
    assert_eq!(sink.events, vec![("committed", true), ("outcomes", true)]);
    assert_eq!(sink.committed, vec![ServerSeq(2)]);
}

// ---------------------------------------------------------------------------------------------
// Order and identity (5.5 steps 1 and 2)
// ---------------------------------------------------------------------------------------------

#[test]
fn a_replay_gets_its_original_outcome_and_writes_nothing() {
    let mut s = setup();
    let me = session(None, false);
    let batch = [make(1, 1, "a"), make(2, 2, "deny")];
    let (first, _) = run(&s, &me, &batch, NOW_MS);
    let (again, end) = run(&s, &me, &batch, NOW_MS + 5);
    assert_eq!(end, Ok(()));
    // Nothing committed, so nothing is pushed to the fence.
    assert!(again.committed.is_empty());
    assert_eq!(again.frames[0], reject(2, RejectReason::Denied));
    let ServerMsg::Ack(original) = &first.frames[1] else {
        panic!("{:?}", first.frames)
    };
    let ServerMsg::Ack(replayed) = &again.frames[1] else {
        panic!("{:?}", again.frames)
    };
    assert_eq!((replayed.op, replayed.seq), (original.op, original.seq));
    // A replay always names its accepted stamp.
    assert_eq!(replayed.restamped, vec![(id(1), hlc(50_000, 1, ME))]);
    assert_eq!(rows(&mut s).len(), 1);
}

#[test]
fn an_op_repeated_inside_one_batch_is_a_replay_of_the_first() {
    let s = setup();
    let (sink, end) = run(
        &s,
        &session(None, false),
        &[make(1, 1, "a"), make(1, 1, "a")],
        NOW_MS,
    );
    assert_eq!(end, Ok(()));
    assert_eq!(kinds(&sink.frames), vec![("ack", 1), ("ack", 1)]);
}

#[test]
fn a_gap_another_replica_and_a_foreign_row_are_forged() {
    let s = setup();
    let me = session(None, false);
    let mut foreign = make(1, 1, "a");
    if let DecodedOp::Valid(op) = &mut foreign {
        op.row = RowId::new(hlc(2_000, 1, 99));
    }
    let mut other = make(2, 2, "a");
    if let DecodedOp::Valid(op) = &mut other {
        op.id.replica = ReplicaId(99);
    }
    let batch = [foreign, other, make(2, 2, "a"), make(9, 9, "a")];
    let (sink, end) = run(&s, &me, &batch, NOW_MS);
    assert_eq!(end, Ok(()));
    assert_eq!(
        kinds(&sink.frames),
        vec![("reject", 1), ("reject", 2), ("reject", 9), ("ack", 2)]
    );
    assert!(sink.frames[..3].iter().all(|f| matches!(
        f,
        ServerMsg::Reject {
            reason: RejectReason::Forged,
            ..
        }
    )));
    // The foreign row was recorded: its replay is Forged again and seq 2 stays accepted.
    let (sink, _) = run(&s, &me, &[make(1, 1, "a")], NOW_MS);
    assert_eq!(sink.frames, vec![reject(1, RejectReason::Forged)]);
}

#[test]
fn a_session_without_principal_in_an_auth_app_is_denied() {
    let s = setup();
    let (sink, end) = run(&s, &session(None, true), &[make(1, 1, "a")], NOW_MS);
    assert_eq!(end, Ok(()));
    assert_eq!(sink.frames, vec![reject(1, RejectReason::Denied)]);
}

#[test]
fn a_batch_over_the_limit_gets_one_reject_limit_per_op_in_batch_order() {
    let mut s = setup();
    let n = LIMITS.ops_per_batch + 1;
    let batch: Vec<DecodedOp> = (1..=n).map(|seq| make(seq, 1, "a")).collect();
    let (sink, end) = run(&s, &session(None, false), &batch, NOW_MS);
    assert_eq!(end, Ok(()));
    let expected: Vec<ServerMsg> = (1..=n)
        .map(|seq| reject(seq, RejectReason::Limit))
        .collect();
    assert_eq!(sink.frames, expected);
    assert!(rows(&mut s).is_empty());
}

// ---------------------------------------------------------------------------------------------
// Stamps (5.5 step 3, D186)
// ---------------------------------------------------------------------------------------------

#[test]
fn a_future_stamp_is_moved_and_the_row_carries_the_accepted_stamps() {
    let mut s = setup();
    let me = session(Some(user()), true);
    let mut future = make(1, 1, "a");
    if let DecodedOp::Valid(op) = &mut future {
        op.hlc = hlc(NOW_MS + 60_000, 0, ME);
    }
    let (sink, end) = run(&s, &me, &[future], NOW_MS);
    assert_eq!(end, Ok(()));
    let upper = NOW_MS + u64::from(LIMITS.clock_tolerance_ms);
    let [ServerMsg::Ack(Ack { restamped, .. })] = sink.frames.as_slice() else {
        panic!("{:?}", sink.frames)
    };
    assert_eq!(restamped, &vec![(id(1), hlc(upper, 0, ME))]);
    let time = Value::Time(
        ostrel_core::value::SafeInt::new(i64::try_from(upper).unwrap_or(0))
            .unwrap_or_else(|e| panic!("{e:?}")),
    );
    let rows = rows(&mut s);
    let fields = &rows[0].fields;
    assert!(fields.contains(&(FIELD_MADE, time.clone())), "{fields:?}");
    assert!(fields.contains(&(FIELD_CHANGED, time)), "{fields:?}");
    assert!(
        fields.contains(&(FIELD_AUTHOR, Value::Ref(user()))),
        "{fields:?}"
    );

    // A later edit with an old stamp moves just above the replica's last accepted stamp and
    // changes `changed` only.
    let (sink, _) = run(&s, &me, &[set(2, 1, "b")], NOW_MS);
    let [ServerMsg::Ack(Ack { restamped, .. })] = sink.frames.as_slice() else {
        panic!("{:?}", sink.frames)
    };
    assert_eq!(restamped, &vec![(id(2), hlc(upper, 1, ME))]);
}

#[test]
fn without_auth_the_row_has_no_author() {
    let mut s = setup();
    let (_, end) = run(&s, &session(None, false), &[make(1, 1, "a")], NOW_MS);
    assert_eq!(end, Ok(()));
    let rows = rows(&mut s);
    assert!(rows[0].fields.iter().all(|(f, _)| *f != FIELD_AUTHOR));
    assert!(rows[0].fields.iter().any(|(f, _)| *f == FIELD_MADE));
}

/// D186: the op decoder refuses implicit field names, so a client can never write a stamp or
/// the author; the op is `Reject Invalid` and the connection stays.
#[test]
fn an_op_that_names_an_implicit_field_is_rejected_invalid() {
    let s = setup();
    let head = |seq: u32| {
        format!(
            r#""hlc":"{}","id":"{}","model":"Note","row":"{}""#,
            hlc(50_000, 0, ME).to_hex(),
            id(seq).to_hex(),
            row(1).to_hex()
        )
    };
    let frame = format!(
        r#"{{"ops":[{{"body":{{"make":{{"made":5,"title":"a"}}}},{}}},{{"body":{{"set":{{"field":"changed","value":5}}}},{}}},{{"body":{{"set":{{"field":"author","value":"{}"}}}},{}}}],"t":"ops","v":0}}"#,
        head(1),
        head(2),
        user().to_hex(),
        head(3)
    );
    let Ok(ClientMsg::Ops(ops)) = decode_client(frame.as_bytes(), &Schema) else {
        panic!("not an ops frame: {frame}")
    };
    assert!(
        ops.iter().all(|op| matches!(op, DecodedOp::Invalid { .. })),
        "{ops:?}"
    );
    let (sink, end) = run(&s, &session(None, false), &ops, NOW_MS);
    assert_eq!(end, Ok(()));
    assert_eq!(
        sink.frames,
        vec![
            reject(1, RejectReason::Invalid),
            reject(2, RejectReason::Invalid),
            reject(3, RejectReason::Invalid),
        ]
    );
}

#[test]
fn rules_that_write_an_implicit_field_are_a_defect_and_nothing_is_answered() {
    let mut s = setup();
    s.switch.write_made.set(true);
    let (sink, end) = run(&s, &session(None, false), &[make(1, 1, "a")], NOW_MS);
    assert!(matches!(end, Err(BatchFault::Defect(_))), "{end:?}");
    assert!(sink.events.is_empty());
    assert!(rows(&mut s).is_empty());
}

// ---------------------------------------------------------------------------------------------
// Commit (D159)
// ---------------------------------------------------------------------------------------------

#[test]
fn a_failed_commit_ends_the_process_and_answers_nothing() {
    let mut s = setup();
    s.switch.conflict.set(true);
    let (sink, end) = run(&s, &session(None, false), &[make(1, 1, "a")], NOW_MS);
    assert!(matches!(end, Err(BatchFault::Commit(_))), "{end:?}");
    assert!(sink.events.is_empty(), "nothing pushed or acknowledged");
    assert!(rows(&mut s).is_empty());
}
