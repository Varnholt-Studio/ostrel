//! The `Call` and `Return` path of the WebSocket session (ARCHITECTURE 5.4 "Server fn calls",
//! 5.7, 5.9; D155, D156, D159, D161, D162).
//!
//! The VM has no entry for a named server fn yet, so the server fns here are hand written
//! ([`Fns`]): each one reads and writes the database through the [`CallDb`] it gets and records
//! what it saw in a [`Probe`]. A fn can pause at its pre-check or in its body, so a test can
//! look at the locks while a call is in flight. Futures are polled by hand with a no-op waker;
//! the memory driver never waits, so only a pause makes a call pending.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::{Pin, pin};
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use ostrel_core::value::{SafeInt, Value};
use ostrel_db::api::{Connection, DbError, Driver, Query, RowId, Transaction};
use ostrel_db_memory::MemoryDriver;
use ostrel_server::audit::{AuditRecord, AuditStore};
use ostrel_server::host::Clock;
use ostrel_server::ws::{
    CallDb, CallServer, CommitFailed, FnFault, Incoming, LOCKED_CALL_STEPS, ServerFns, Session,
};
use ostrel_sync::protocol::{CallError, FieldId, FnId, ModelId};
use ostrel_sync::wire::{WireSchema, WireTy};
use ostrel_vm::host::{AuditEntry, AuditError, BoxFuture, ExternErrorKind, ServerHost};
use ostrel_vm::{Limits, RuntimeError, RuntimeErrorKind, Span};

// ---------------------------------------------------------------------------------------------
// Driving futures
// ---------------------------------------------------------------------------------------------

fn poll<F: Future>(f: Pin<&mut F>) -> Poll<F::Output> {
    f.poll(&mut Context::from_waker(Waker::noop()))
}

/// Runs a future that pauses nowhere.
fn block_on<F: Future>(f: F) -> F::Output {
    match poll(pin!(f)) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("the future waited although nothing pauses"),
    }
}

/// Pending while the flag is set.
struct Pause<'a>(&'a Cell<bool>);

impl Future for Pause<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        if self.0.get() {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Schema and server fns
// ---------------------------------------------------------------------------------------------

/// A non locking fn that reads, calls an extern and can pause in its body.
const COUNT: FnId = 0;
/// A locking fn that writes the sequence `bumps` in its transaction.
const BUMP: FnId = 1;
/// A locking fn that writes and then stops with `DivisionByZero`.
const BOOM: FnId = 2;
/// A locking elevated fn: audit entry, then body.
const AUDITED: FnId = 3;
/// A non locking fn whose result does not fit one frame.
const BIG: FnId = 4;
/// A locking fn whose write meets a commit of another connection.
const CONFLICT: FnId = 5;

fn functions() -> &'static [(&'static str, Vec<WireTy>, WireTy)] {
    use std::sync::OnceLock;
    static FUNCTIONS: OnceLock<Vec<(&'static str, Vec<WireTy>, WireTy)>> = OnceLock::new();
    FUNCTIONS.get_or_init(|| {
        vec![
            ("count", vec![], WireTy::Int),
            ("bump", vec![WireTy::Text], WireTy::Int),
            ("boom", vec![], WireTy::Int),
            ("audited", vec![WireTy::Text], WireTy::Bool),
            ("big", vec![], WireTy::List(Box::new(WireTy::Text))),
            ("conflict", vec![], WireTy::Int),
        ]
    })
}

struct Schema;

impl WireSchema for Schema {
    fn model_id(&self, _: &str) -> Option<ModelId> {
        None
    }
    fn model_name(&self, _: ModelId) -> Option<&str> {
        None
    }
    fn field_id(&self, _: ModelId, _: &str) -> Option<FieldId> {
        None
    }
    fn field_name(&self, _: ModelId, _: FieldId) -> Option<&str> {
        None
    }
    fn field_ty(&self, _: ModelId, _: FieldId) -> Option<&WireTy> {
        None
    }
    fn function_id(&self, name: &str) -> Option<FnId> {
        let at = functions().iter().position(|(n, ..)| *n == name)?;
        FnId::try_from(at).ok()
    }
    fn function_name(&self, function: FnId) -> Option<&str> {
        functions().get(function as usize).map(|(name, ..)| *name)
    }
    fn function_params(&self, function: FnId) -> Option<&[WireTy]> {
        functions()
            .get(function as usize)
            .map(|(_, params, _)| params.as_slice())
    }
    fn function_result(&self, function: FnId) -> Option<&WireTy> {
        functions()
            .get(function as usize)
            .map(|(.., result)| result)
    }
}

/// What the fns saw, and switches the tests set.
#[derive(Default)]
struct Probe {
    /// Steps in order: `admit <fn>`, `invoke <fn> locked|read`.
    log: RefCell<Vec<String>>,
    /// The kind of every extern call, per fn.
    externs: RefCell<Vec<(FnId, ExternErrorKind)>>,
    /// The step limit every invoke got, per fn.
    steps: RefCell<Vec<(FnId, u64)>>,
    pause_admit: Cell<bool>,
    pause_invoke: Cell<bool>,
    /// Pauses `count` after its query.
    pause_after_read: Cell<bool>,
    /// The pre-check refuses with `Denied`.
    deny_pre_check: Cell<bool>,
    /// The repeated admission under the lock refuses with `Denied`.
    deny_under_lock: Cell<bool>,
    /// A connection to the same database beside the server's two.
    side: RefCell<Option<Box<dyn Connection>>>,
}

struct Fns(Rc<Probe>);

fn int(v: u64) -> Value {
    let v = i64::try_from(v).unwrap_or(i64::MAX);
    Value::Int(SafeInt::new(v).unwrap_or_else(|_| panic!("{v} is not an Int")))
}

fn runtime(kind: RuntimeErrorKind) -> FnFault {
    FnFault::Runtime(RuntimeError {
        kind,
        span: Span::default(),
    })
}

fn db(e: DbError) -> FnFault {
    panic!("database error in a test fn: {e:?}")
}

impl ServerFns for Fns {
    fn is_locking(&self, function: FnId) -> bool {
        matches!(function, BUMP | BOOM | AUDITED | CONFLICT)
    }

    fn admit<'a, 'c>(
        &'a self,
        _tx: &'a mut (dyn Transaction<'c> + 'c),
        _principal: Option<RowId>,
        function: FnId,
        _args: &'a [Value],
    ) -> BoxFuture<'a, Result<(), CallError>> {
        let probe = &self.0;
        Box::pin(async move {
            probe.log.borrow_mut().push(format!("admit {function}"));
            Pause(&probe.pause_admit).await;
            if probe.deny_pre_check.get() {
                return Err(CallError::Denied);
            }
            Ok(())
        })
    }

    fn invoke<'a, 'c>(
        &'a self,
        db_: CallDb<'a, 'c>,
        host: &'a mut dyn ServerHost,
        function: FnId,
        args: &'a [Value],
        limits: Limits,
    ) -> BoxFuture<'a, Result<Value, FnFault>> {
        let probe = &self.0;
        Box::pin(async move {
            let mode = match &db_ {
                CallDb::Locked(_) => "locked",
                CallDb::Read(_) => "read",
            };
            probe
                .log
                .borrow_mut()
                .push(format!("invoke {function} {mode}"));
            probe.steps.borrow_mut().push((function, limits.max_steps));
            match (function, db_) {
                (COUNT, CallDb::Read(reader)) => {
                    Pause(&probe.pause_invoke).await;
                    // One read transaction for the one query, ended before the body goes on.
                    let rows = reader.query(&Query::all(0)).await.map_err(db)?;
                    probe.log.borrow_mut().push(format!("read {}", rows.0.len()));
                    Pause(&probe.pause_after_read).await;
                    let kind = host
                        .extern_call("mail", "send", &[])
                        .await
                        .err()
                        .map(|e| e.kind);
                    if let Some(kind) = kind {
                        probe.externs.borrow_mut().push((function, kind));
                    }
                    Ok(int(7))
                }
                (BIG, CallDb::Read(_)) => {
                    // 8 texts of 200 KiB: each within the text limit, together above 1 MiB.
                    Ok(Value::List(vec![Value::Text("x".repeat(200 * 1024)); 8]))
                }
                (_, CallDb::Locked(tx)) => {
                    if probe.deny_under_lock.get() {
                        return Err(FnFault::Refused(CallError::Denied));
                    }
                    Pause(&probe.pause_invoke).await;
                    let n = tx.next_in_sequence("bumps").await.map_err(db)?;
                    match function {
                        BUMP => {
                            let kind = host.extern_call("mail", "send", args).await.err();
                            if let Some(e) = kind {
                                probe.externs.borrow_mut().push((function, e.kind));
                            }
                            Ok(int(n))
                        }
                        BOOM => Err(runtime(RuntimeErrorKind::DivisionByZero)),
                        AUDITED => {
                            let entry = AuditEntry {
                                function: "audited".to_owned(),
                                principal: host.principal(),
                                arguments: args.to_vec(),
                                time: host.now(),
                            };
                            // The VM does not run the body when the entry fails (D117).
                            if host.audit(&entry).await.is_err() {
                                return Err(runtime(RuntimeErrorKind::DivisionByZero));
                            }
                            Ok(Value::Bool(true))
                        }
                        CONFLICT => {
                            let taken = probe.side.borrow_mut().take();
                            let Some(mut side) = taken else {
                                panic!("no side connection")
                            };
                            {
                                let mut other = side.begin().await.map_err(db)?;
                                other.next_in_sequence("other").await.map_err(db)?;
                                other.commit().await.map_err(db)?;
                            }
                            *probe.side.borrow_mut() = Some(side);
                            Ok(int(n))
                        }
                        _ => panic!("fn {function} is not locking"),
                    }
                }
                (f, other) => panic!("fn {f} got {other:?}"),
            }
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------------------------

/// Audit records in a list the test can read while the server owns the store.
#[derive(Clone, Default)]
struct SharedLog(Rc<RefCell<Vec<AuditRecord>>>);

impl AuditStore for SharedLog {
    fn append<'a>(&'a mut self, record: AuditRecord) -> BoxFuture<'a, Result<(), AuditError>> {
        Box::pin(async move {
            self.0.borrow_mut().push(record);
            Ok(())
        })
    }
}

struct ClosedLog;

impl AuditStore for ClosedLog {
    fn append<'a>(&'a mut self, _: AuditRecord) -> BoxFuture<'a, Result<(), AuditError>> {
        Box::pin(async { Err(AuditError("log closed".to_owned())) })
    }
}

struct TestClock(i64);

impl Clock for TestClock {
    fn now_ms(&mut self) -> SafeInt {
        self.0 += 1;
        SafeInt::new(self.0).unwrap_or_else(|_| panic!("small clock"))
    }
}

struct Setup {
    server: CallServer,
    probe: Rc<Probe>,
}

fn setup_with(audit: Box<dyn AuditStore>) -> Setup {
    let driver = MemoryDriver::new();
    let writer = block_on(driver.connect("memory:")).unwrap_or_else(|e| panic!("{e:?}"));
    let reader = block_on(driver.connect("memory:")).unwrap_or_else(|e| panic!("{e:?}"));
    let side = block_on(driver.connect("memory:")).unwrap_or_else(|e| panic!("{e:?}"));
    let probe = Rc::new(Probe::default());
    *probe.side.borrow_mut() = Some(side);
    let server = CallServer::new(
        Box::new(Fns(Rc::clone(&probe))),
        writer,
        reader,
        audit,
        Box::new(TestClock(0)),
        Limits::default(),
    );
    Setup { server, probe }
}

fn setup() -> Setup {
    setup_with(Box::new(SharedLog::default()))
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

/// The value the sequence `bumps` would take next, read without changing it.
fn next_bump(s: &Setup) -> u64 {
    let mut side = s.probe.side.borrow_mut();
    let Some(side) = side.as_mut() else {
        panic!("no side connection")
    };
    block_on(async {
        let mut tx = side.begin().await.unwrap_or_else(|e| panic!("{e:?}"));
        let n = tx
            .next_in_sequence("bumps")
            .await
            .unwrap_or_else(|e| panic!("{e:?}"));
        tx.rollback().await.unwrap_or_else(|e| panic!("{e:?}"));
        n
    })
}

fn log(s: &Setup) -> Vec<String> {
    s.probe.log.borrow().clone()
}

// ---------------------------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------------------------

fn call_frame(call: u32, function: &str, args: &str) -> Vec<u8> {
    format!(r#"{{"v":0,"t":"call","call":{call},"function":"{function}","args":{args}}}"#)
        .into_bytes()
}

#[test]
fn a_call_frame_gets_its_return_and_the_call_closes() {
    let s = setup();
    let mut session = Session::new(None);
    let job = match session.receive(&call_frame(1, "count", "[]"), &Schema) {
        Incoming::Call(job) => job,
        other => panic!("expected a call, got {other:?}"),
    };
    assert_eq!((job.call, job.function, job.principal), (1, COUNT, None));
    assert_eq!(session.open_calls().len(), 1);
    let result = block_on(s.server.call(job.principal, job.function, &job.args));
    assert_eq!(result, Ok(Ok(int(7))));
    let frame = session
        .finish(job.call, Ok(int(7)), &Schema)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(frame, r#"{"call":1,"result":{"ok":7},"t":"return","v":0}"#);
    assert!(session.open_calls().is_empty());
}

#[test]
fn the_return_is_typed_by_the_function_of_its_open_call() {
    let mut session = Session::new(None);
    assert!(matches!(
        session.receive(&call_frame(4, "bump", r#"["a"]"#), &Schema),
        Incoming::Call(_)
    ));
    // `bump` returns an Int: a Bool result does not fit, which is a server defect.
    assert!(session.finish(4, Ok(Value::Bool(true)), &Schema).is_err());
    // The call is closed all the same; a second return for it names no open call.
    assert!(session.open_calls().is_empty());
    assert!(session.finish(4, Ok(int(1)), &Schema).is_err());
}

#[test]
fn a_refused_call_frame_gets_a_return_and_the_connection_stays() {
    let mut session = Session::new(None);
    assert_eq!(
        session.receive(&call_frame(2, "nope", "[]"), &Schema),
        Incoming::Reply(r#"{"call":2,"result":{"err":"NotFound"},"t":"return","v":0}"#.to_owned())
    );
    assert_eq!(
        session.receive(&call_frame(3, "bump", "[1]"), &Schema),
        Incoming::Reply(r#"{"call":3,"result":{"err":"Invalid"},"t":"return","v":0}"#.to_owned())
    );
    assert!(session.open_calls().is_empty());
}

#[test]
fn a_malformed_frame_and_a_second_hello_are_fatal() {
    let fatal = r#"{"code":"Protocol","sub":null,"t":"error","v":0}"#.to_owned();
    let mut session = Session::new(None);
    assert_eq!(
        session.receive(b"{\"v\":0,\"t\":\"call\"}", &Schema),
        Incoming::Close(fatal.clone())
    );
    let hello = br#"{"v":0,"t":"hello","token":null,"replica":null,"replica_secret":null,"last_server_seq":null}"#;
    assert_eq!(session.receive(hello, &Schema), Incoming::Close(fatal));
}

#[test]
fn an_open_call_id_used_again_is_fatal() {
    let mut session = Session::new(None);
    assert!(matches!(
        session.receive(&call_frame(5, "count", "[]"), &Schema),
        Incoming::Call(_)
    ));
    assert_eq!(
        session.receive(&call_frame(5, "count", "[]"), &Schema),
        Incoming::Close(r#"{"code":"Protocol","sub":null,"t":"error","v":0}"#.to_owned())
    );
}

#[test]
fn an_open_call_id_with_an_unknown_function_is_fatal() {
    let mut session = Session::new(None);
    assert!(matches!(
        session.receive(&call_frame(5, "count", "[]"), &Schema),
        Incoming::Call(_)
    ));
    // Not `return` `NotFound` for call 5: the client would take it as the result of the call
    // that is still running.
    assert_eq!(
        session.receive(&call_frame(5, "nope", "[]"), &Schema),
        Incoming::Close(r#"{"code":"Protocol","sub":null,"t":"error","v":0}"#.to_owned())
    );
}

#[test]
fn an_open_call_id_with_arguments_that_do_not_fit_is_fatal() {
    let mut session = Session::new(None);
    assert!(matches!(
        session.receive(&call_frame(6, "bump", r#"["a"]"#), &Schema),
        Incoming::Call(_)
    ));
    // Not `return` `Invalid` for call 6, which is still running.
    assert_eq!(
        session.receive(&call_frame(6, "bump", "[1]"), &Schema),
        Incoming::Close(r#"{"code":"Protocol","sub":null,"t":"error","v":0}"#.to_owned())
    );
}

#[test]
fn a_refused_call_beside_an_open_call_gets_its_return_and_the_open_call_stays() {
    let mut session = Session::new(None);
    assert!(matches!(
        session.receive(&call_frame(7, "count", "[]"), &Schema),
        Incoming::Call(_)
    ));
    assert_eq!(
        session.receive(&call_frame(8, "nope", "[]"), &Schema),
        Incoming::Reply(r#"{"call":8,"result":{"err":"NotFound"},"t":"return","v":0}"#.to_owned())
    );
    assert_eq!(session.open_calls().len(), 1);
    let frame = session
        .finish(7, Ok(int(7)), &Schema)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(frame, r#"{"call":7,"result":{"ok":7},"t":"return","v":0}"#);
}

#[test]
fn sync_messages_go_on_to_the_sync_engine() {
    let mut session = Session::new(None);
    match session.receive(br#"{"v":0,"t":"unsubscribe","sub":3}"#, &Schema) {
        Incoming::Sync(msg) => assert_eq!(msg.name(), "unsubscribe"),
        other => panic!("expected a sync message, got {other:?}"),
    }
}

#[test]
fn a_return_too_large_for_one_frame_is_limit() {
    let s = setup();
    let mut session = Session::new(None);
    let Incoming::Call(job) = session.receive(&call_frame(9, "big", "[]"), &Schema) else {
        panic!("expected a call")
    };
    let result =
        block_on(s.server.call(None, job.function, &job.args)).unwrap_or_else(|e| panic!("{e}"));
    let frame = session
        .finish(job.call, result, &Schema)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(
        frame,
        r#"{"call":9,"result":{"err":"Limit"},"t":"return","v":0}"#
    );
}

// ---------------------------------------------------------------------------------------------
// Locks
// ---------------------------------------------------------------------------------------------

#[test]
fn a_locking_call_holds_the_write_lock_from_its_entry_and_commits() {
    let s = setup();
    s.probe.pause_invoke.set(true);
    let args = [text("a")];
    let mut call = pin!(s.server.call(None, BUMP, &args));
    assert!(poll(call.as_mut()).is_pending());
    // Paused in the body: the write lock is held, the read connection is free, and the
    // admission ran before, on the read connection.
    assert!(s.server.write_lock().is_held());
    assert!(!s.server.read_connection().is_held());
    assert_eq!(log(&s), ["admit 1", "invoke 1 locked"]);
    s.probe.pause_invoke.set(false);
    assert_eq!(poll(call.as_mut()), Poll::Ready(Ok(Ok(int(1)))));
    assert!(!s.server.write_lock().is_held());
    // Committed: the sequence goes on from 1.
    assert_eq!(next_bump(&s), 2);
}

#[test]
fn the_pre_check_ends_its_read_transaction_before_it_waits_for_the_lock() {
    let s = setup();
    let args = [text("a")];
    let held = block_on(s.server.write_lock().lock());
    let mut call = pin!(s.server.call(None, BUMP, &args));
    assert!(poll(call.as_mut()).is_pending());
    // The call waits for the write lock with its pre-check done and the read connection free.
    assert_eq!(log(&s), ["admit 1"]);
    assert_eq!(s.server.write_lock().waiting(), 1);
    assert!(!s.server.read_connection().is_held());
    drop(held);
    assert_eq!(poll(call.as_mut()), Poll::Ready(Ok(Ok(int(1)))));
}

#[test]
fn the_pre_check_runs_without_the_write_lock() {
    let s = setup();
    s.probe.pause_admit.set(true);
    let args = [text("a")];
    let mut call = pin!(s.server.call(None, BUMP, &args));
    assert!(poll(call.as_mut()).is_pending());
    assert!(s.server.read_connection().is_held());
    assert!(!s.server.write_lock().is_held());
    s.probe.pause_admit.set(false);
    assert!(poll(call.as_mut()).is_ready());
}

#[test]
fn a_refused_pre_check_is_final_and_never_takes_the_write_lock() {
    let s = setup();
    s.probe.deny_pre_check.set(true);
    let result = block_on(s.server.call(None, BUMP, &[text("a")]));
    assert_eq!(result, Ok(Err(CallError::Denied)));
    assert_eq!(s.server.write_lock().grants(), 0);
    assert_eq!(log(&s), ["admit 1"]);
}

#[test]
fn the_admission_is_repeated_under_the_lock_and_a_refusal_there_rolls_back() {
    let s = setup();
    s.probe.deny_under_lock.set(true);
    let result = block_on(s.server.call(None, BUMP, &[text("a")]));
    assert_eq!(result, Ok(Err(CallError::Denied)));
    assert_eq!(log(&s), ["admit 1", "invoke 1 locked"]);
    assert_eq!(s.server.write_lock().grants(), 1);
    assert_eq!(next_bump(&s), 1);
}

#[test]
fn a_call_that_is_not_locking_takes_no_lock_and_runs_beside_a_writer() {
    let s = setup();
    let held = block_on(s.server.write_lock().lock());
    s.probe.pause_invoke.set(true);
    let mut call = pin!(s.server.call(None, COUNT, &[]));
    assert!(poll(call.as_mut()).is_pending());
    // Paused in the VM: no read transaction is open (D162).
    assert!(!s.server.read_connection().is_held());
    s.probe.pause_invoke.set(false);
    assert_eq!(poll(call.as_mut()), Poll::Ready(Ok(Ok(int(7)))));
    drop(held);
    assert_eq!(s.server.write_lock().grants(), 1);
    assert_eq!(log(&s), ["invoke 0 read", "read 0"]);
}

#[test]
fn a_query_of_a_call_that_is_not_locking_ends_its_read_transaction_before_it_returns() {
    let s = setup();
    s.probe.pause_after_read.set(true);
    let mut call = pin!(s.server.call(None, COUNT, &[]));
    assert!(poll(call.as_mut()).is_pending());
    // The query ran and the fn goes on in the VM: the read connection is free again (D162).
    assert_eq!(log(&s), ["invoke 0 read", "read 0"]);
    assert!(!s.server.read_connection().is_held());
    s.probe.pause_after_read.set(false);
    assert_eq!(poll(call.as_mut()), Poll::Ready(Ok(Ok(int(7)))));
}

#[test]
fn a_bridge_call_under_the_write_lock_is_busy_and_not_sent() {
    let s = setup();
    assert!(block_on(s.server.call(None, BUMP, &[text("a")])).is_ok());
    assert!(block_on(s.server.call(None, COUNT, &[])).is_ok());
    // Under the lock `Busy`; without it the call reaches the host, whose sidecar is not
    // attached in this crate version (`Crashed`).
    assert_eq!(
        *s.probe.externs.borrow(),
        [
            (BUMP, ExternErrorKind::Busy),
            (COUNT, ExternErrorKind::Crashed)
        ]
    );
}

#[test]
fn the_step_limit_under_the_write_lock_is_200_000() {
    let s = setup();
    assert!(block_on(s.server.call(None, BUMP, &[text("a")])).is_ok());
    assert!(block_on(s.server.call(None, COUNT, &[])).is_ok());
    assert_eq!(LOCKED_CALL_STEPS, 200_000);
    assert_eq!(
        *s.probe.steps.borrow(),
        [(BUMP, 200_000), (COUNT, Limits::default().max_steps)]
    );
}

#[test]
fn writers_get_the_lock_in_the_order_they_asked() {
    let s = setup();
    s.probe.pause_invoke.set(true);
    let a = [text("a")];
    let b = [text("b")];
    let mut first = pin!(s.server.call(None, BUMP, &a));
    let mut second = pin!(s.server.call(None, BUMP, &b));
    assert!(poll(first.as_mut()).is_pending());
    assert!(poll(second.as_mut()).is_pending());
    assert_eq!(s.server.write_lock().waiting(), 1);
    s.probe.pause_invoke.set(false);
    // The second call is polled first, but the first holds the lock.
    assert!(poll(second.as_mut()).is_pending());
    assert_eq!(poll(first.as_mut()), Poll::Ready(Ok(Ok(int(1)))));
    assert_eq!(poll(second.as_mut()), Poll::Ready(Ok(Ok(int(2)))));
}

// ---------------------------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------------------------

#[test]
fn a_runtime_error_rolls_the_call_back_and_travels_by_kind() {
    let s = setup();
    let result = block_on(s.server.call(None, BOOM, &[]));
    assert_eq!(
        result,
        Ok(Err(CallError::Runtime {
            kind: "DivisionByZero".to_owned()
        }))
    );
    assert_eq!(next_bump(&s), 1);
    assert!(!s.server.write_lock().is_held());
}

#[test]
fn an_elevated_call_writes_its_audit_entry_and_commits() {
    let audit = SharedLog::default();
    let s = setup_with(Box::new(audit.clone()));
    let user = RowId::from_hex("000000000000000000000000000000aa").ok();
    let result = block_on(s.server.call(user, AUDITED, &[text("secret")]));
    assert_eq!(result, Ok(Ok(Value::Bool(true))));
    let records = audit.0.borrow();
    let [record] = records.as_slice() else {
        panic!("one audit record, got {}", records.len())
    };
    assert_eq!(record.function, "audited");
    assert_eq!(record.principal, user);
    assert!(!format!("{record:?}").contains("secret"));
    assert_eq!(next_bump(&s), 2);
}

#[test]
fn a_failed_audit_entry_makes_the_call_unavailable_and_rolls_it_back() {
    let s = setup_with(Box::new(ClosedLog));
    let result = block_on(s.server.call(None, AUDITED, &[text("x")]));
    assert_eq!(result, Ok(Err(CallError::Unavailable)));
    assert_eq!(next_bump(&s), 1);
}

#[test]
fn a_failed_commit_ends_the_process() {
    let s = setup();
    let result = block_on(s.server.call(None, CONFLICT, &[]));
    assert!(
        matches!(result, Err(CommitFailed(_))),
        "expected CommitFailed, got {result:?}"
    );
    // Nothing of the call is visible, and the lock was released.
    assert_eq!(next_bump(&s), 1);
    assert!(!s.server.write_lock().is_held());
}
