//! Hostile tests for the VM step limit of server fn calls (ARCHITECTURE 5.9, risk 13 (a)).
//!
//! The server runs on one thread, so a call whose VM loops without an await blocks every other
//! session of the process, writers and the broadcast queue included, until the VM stops. A
//! call that takes no lock therefore gets the same step budget as a call under the write lock,
//! so it blocks the process no longer than the architecture already accepts for a writer.
//!
//! The VM has no entry for a named server fn yet, so the fns here run a compiled script whose
//! `main` does not finish in practice (`fib(60)`) with the limits the call path hands them.

use std::cell::RefCell;
use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use ostrel_core::FileId;
use ostrel_core::value::{SafeInt, Value};
use ostrel_db::api::{Driver, RowId, Transaction};
use ostrel_db_memory::MemoryDriver;
use ostrel_server::audit::MemoryAuditLog;
use ostrel_server::host::Clock;
use ostrel_server::ws::{CallDb, CallServer, FnFault, LOCKED_CALL_STEPS, ServerFns};
use ostrel_sync::protocol::{CallError, FnId};
use ostrel_vm::host::{BoxFuture, ServerHost};
use ostrel_vm::{Host, Image, Limits};

/// A fn that is not locking and loops in the VM.
const SPIN: FnId = 0;
/// A locking fn that loops in the VM under the write lock.
const SPIN_LOCKED: FnId = 1;
/// A fn that is not locking and returns at once.
const QUICK: FnId = 2;

const ENDLESS: &str = "\
fn fib(n: Int) -> Int
  if n < 2
    return n
  return fib(n - 1) + fib(n - 2)

fn main()
  print(fib(60))
";

fn block_on<F: Future>(f: F) -> F::Output {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("the future waited although nothing pauses"),
    }
}

fn image(src: &str) -> Image {
    let file = FileId::from_raw(0);
    let (tokens, lexed) = ostrel_syntax::lex::lex(src, file);
    assert!(lexed.is_empty(), "{lexed:?}");
    let (module, parsed) = ostrel_syntax::parse::parse(src, file, &tokens, &lexed);
    assert!(parsed.is_empty(), "{parsed:?}");
    let (checked, diags) = ostrel_sema::check(&module, file, src);
    assert!(diags.is_empty(), "{diags:?}");
    let program = ostrel_ir::lower(&module, &checked, file).unwrap_or_else(|e| panic!("{e:?}"));
    ostrel_vm::compile(&program).unwrap_or_else(|e| panic!("{e:?}"))
}

/// Drops program output; the endless program prints nothing before it stops.
struct Silent;

impl Host for Silent {
    fn print(&mut self, _: &str) {}
}

struct Fns {
    image: Image,
    /// The step limit every invoke got, per fn.
    steps: Rc<RefCell<Vec<(FnId, u64)>>>,
}

impl ServerFns for Fns {
    fn is_locking(&self, function: FnId) -> bool {
        function == SPIN_LOCKED
    }

    fn admit<'a, 'c>(
        &'a self,
        _: &'a mut (dyn Transaction<'c> + 'c),
        _: Option<RowId>,
        _: FnId,
        _: &'a [Value],
    ) -> BoxFuture<'a, Result<(), CallError>> {
        Box::pin(async { Ok(()) })
    }

    fn invoke<'a, 'c>(
        &'a self,
        _: CallDb<'a, 'c>,
        _: &'a mut dyn ServerHost,
        function: FnId,
        _: &'a [Value],
        limits: Limits,
    ) -> BoxFuture<'a, Result<Value, FnFault>> {
        Box::pin(async move {
            self.steps.borrow_mut().push((function, limits.max_steps));
            if function == QUICK {
                return Ok(Value::Bool(true));
            }
            ostrel_vm::run(&self.image, &mut Silent, limits).map_err(FnFault::Runtime)?;
            Ok(Value::Bool(true))
        })
    }
}

struct TestClock(i64);

impl Clock for TestClock {
    fn now_ms(&mut self) -> SafeInt {
        self.0 += 1;
        SafeInt::new(self.0).unwrap_or_else(|_| panic!("small clock"))
    }
}

fn server() -> (CallServer, Rc<RefCell<Vec<(FnId, u64)>>>) {
    let driver = MemoryDriver::new();
    let writer = block_on(driver.connect("memory:")).unwrap_or_else(|e| panic!("{e:?}"));
    let reader = block_on(driver.connect("memory:")).unwrap_or_else(|e| panic!("{e:?}"));
    let steps = Rc::new(RefCell::new(Vec::new()));
    let fns = Fns {
        image: image(ENDLESS),
        steps: Rc::clone(&steps),
    };
    let server = CallServer::new(
        Box::new(fns),
        writer,
        reader,
        Box::new(MemoryAuditLog::new()),
        Box::new(TestClock(0)),
        Limits::default(),
    );
    (server, steps)
}

fn step_limit() -> Result<Result<Value, CallError>, ostrel_server::ws::CommitFailed> {
    Ok(Err(CallError::Runtime {
        kind: "StepLimit".to_owned(),
    }))
}

/// The shortest of three runs of `function`, each of which must end with `StepLimit`.
fn shortest_spin(server: &CallServer, function: FnId) -> Duration {
    (0..3)
        .map(|_| {
            let start = Instant::now();
            assert_eq!(block_on(server.call(None, function, &[])), step_limit());
            start.elapsed()
        })
        .min()
        .unwrap_or_default()
}

#[test]
fn ac_34_a_call_without_the_lock_that_loops_ends_with_step_limit() {
    let (server, steps) = server();
    assert_eq!(block_on(server.call(None, SPIN, &[])), step_limit());
    assert_eq!(*steps.borrow(), [(SPIN, LOCKED_CALL_STEPS)]);
    // The looping call left nothing held: the next call of another session runs at once.
    assert!(!server.read_connection().is_held());
    assert!(!server.write_lock().is_held());
    assert_eq!(
        block_on(server.call(None, QUICK, &[])),
        Ok(Ok(Value::Bool(true)))
    );
}

#[test]
fn a_call_without_the_lock_gets_no_more_steps_than_a_call_under_it() {
    let (server, steps) = server();
    assert!(block_on(server.call(None, QUICK, &[])).is_ok());
    assert!(block_on(server.call(None, SPIN_LOCKED, &[])).is_ok());
    assert_eq!(
        *steps.borrow(),
        [(QUICK, LOCKED_CALL_STEPS), (SPIN_LOCKED, LOCKED_CALL_STEPS)]
    );
}

#[test]
fn a_looping_call_without_the_lock_blocks_the_process_no_longer_than_a_writer_may() {
    // Every other session waits while the VM runs. A writer at its step limit is the hold
    // time the KPI A setup budgets (ARCHITECTURE 5.4 "Cost", 5.9); a call that takes no lock
    // must not block the process longer than that. Compared within one run, so the bound
    // holds on a slow or loaded machine and in a debug build alike.
    let (server, _) = server();
    let locked = shortest_spin(&server, SPIN_LOCKED);
    let unlocked = shortest_spin(&server, SPIN);
    assert!(
        unlocked <= locked * 3 + Duration::from_millis(20),
        "a call without the lock ran {unlocked:?}, a writer at its limit {locked:?}"
    );
}
