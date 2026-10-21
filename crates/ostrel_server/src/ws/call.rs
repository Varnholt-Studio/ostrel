//! Running one server fn call of a `Call` frame (ARCHITECTURE 5.4 "Server fn calls", 5.7, 5.9).
//!
//! [`CallServer`] holds what every call of the process shares: the program's server fns
//! ([`ServerFns`]), the writer connection behind the write lock, the read connection, the audit
//! store and the clock. [`CallServer::call`] runs one call in the order of D159 and D161:
//!
//! * A fn that is not locking takes no lock. It gets the read connection as
//!   [`CallDb::Read`] and reads each query in a read transaction of its own, so no read
//!   transaction is open while the VM runs, across an `extern js server` call or while it waits
//!   (D162). Every query in it is filtered by `see`.
//! * A locking fn (one that can write, is elevated or can call such a fn; the compiler marks
//!   it) is first pre-checked: its model arguments are resolved through `see` and its `call`
//!   rule is evaluated in one read transaction on the read connection, without the write lock.
//!   That transaction ends before the call waits for the lock. `NotFound` or `Denied` there is
//!   the final answer, so a refused call never occupies the lock. An admission is not kept:
//!   the call then takes the write lock, begins its transaction on the writer connection at
//!   entry and hands it to the fn as [`CallDb::Locked`]; the fn resolves its arguments and
//!   evaluates its `call` rule again in that transaction before the body. The transaction
//!   commits when the fn returns a value and rolls back otherwise; the lock is released after.
//! * Under the lock the VM step limit is [`LOCKED_CALL_STEPS`] (5.9), and every `extern js
//!   server` call fails at once with `ExternError` `Busy` and is not sent (D156).
//! * A `commit` that fails ends the process (D159): [`CallServer::call`] returns
//!   [`CommitFailed`] and the transport closes every session and exits with code 75. Nothing
//!   of that call is answered.
//!
//! The entry into the VM for a named server fn with arguments is the VM's (T2); until it
//! exists, [`ServerFns`] is the seam a test or the compiled program fills.
//!
//! ASSUMPTION: a database error before the body ran (the pre-check could not read, or the
//! transaction of a locking call could not begin) is `CallError` `Unavailable`: nothing of the
//! body happened, so a retry is safe, which is what `Unavailable` promises (D134).

use std::cell::RefCell;
use std::fmt;

use ostrel_core::ids::RowId;
use ostrel_core::value::{SafeInt, Value};
use ostrel_db::api::{Connection, DbError, Transaction};
use ostrel_sync::protocol::{CallError, FnId};
use ostrel_vm::host::{
    AuditEntry, AuditError, BoxFuture, ExternError, ExternErrorKind, ServerHost,
};
use ostrel_vm::{Host, Limits, RuntimeError};

use super::fifo::FifoLock;
use crate::audit::{AuditRecord, AuditStore};
use crate::host::{Clock, TaskHost};

/// VM steps of a call while it holds the write lock (ARCHITECTURE 5.9, D161); each row that a
/// query returns under the lock counts 100 steps, which the VM entry charges.
pub const LOCKED_CALL_STEPS: u64 = 200_000;

/// The database a server fn reads and writes in one call.
pub enum CallDb<'a, 'c> {
    /// A locking call: its transaction on the writer connection, begun at entry while the call
    /// holds the write lock. Every read and write of the call, also of the fns it calls, runs
    /// in it. The caller commits or rolls it back.
    Locked(&'a mut (dyn Transaction<'c> + 'c)),
    /// A call that is not locking: the read connection. Each query takes the connection, reads
    /// in a read transaction of its own and ends it before its rows reach the VM (D162).
    Read(&'a FifoLock<Box<dyn Connection>>),
}

impl fmt::Debug for CallDb<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CallDb::Locked(_) => "CallDb::Locked",
            CallDb::Read(_) => "CallDb::Read",
        })
    }
}

/// Why a server fn returned no value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FnFault {
    /// A typed refusal: `NotFound` (an unreadable or missing model argument, D99), `Denied`
    /// (the `call` rule or a write rule), `Limit` or `Invalid`.
    Refused(CallError),
    /// The VM stopped with a runtime error.
    Runtime(RuntimeError),
}

/// The server fns of the compiled program, as the call path needs them.
pub trait ServerFns {
    /// True when the compiler marked `function` as locking: it can write, is elevated (`as
    /// system`) or can call such a fn (D159, D161).
    fn is_locking(&self, function: FnId) -> bool;

    /// The pre-check of a locking call: resolves the model arguments through the `see` rules of
    /// `principal` and evaluates the `call` rule, in `tx`, a read transaction on the read
    /// connection. `Err` is `NotFound` or `Denied`; a fn without a `call` rule is `Denied`
    /// (D147). Nothing is written and no audit entry is made.
    fn admit<'a, 'c>(
        &'a self,
        tx: &'a mut (dyn Transaction<'c> + 'c),
        principal: Option<RowId>,
        function: FnId,
        args: &'a [Value],
    ) -> BoxFuture<'a, Result<(), CallError>>;

    /// Runs `function` with `args` as the principal of `host`, within `limits`: model
    /// arguments through `see`, then the `call` rule, then the audit entry of an elevated fn,
    /// then the body (D99, D117).
    fn invoke<'a, 'c>(
        &'a self,
        db: CallDb<'a, 'c>,
        host: &'a mut dyn ServerHost,
        function: FnId,
        args: &'a [Value],
        limits: Limits,
    ) -> BoxFuture<'a, Result<Value, FnFault>>;
}

/// A `commit` failed: the process must close every session and exit with code 75 (D159).
#[derive(Clone, Debug, PartialEq)]
pub struct CommitFailed(pub DbError);

impl fmt::Display for CommitFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "commit failed, the process ends: {:?}", self.0)
    }
}

impl std::error::Error for CommitFailed {}

/// What every call of one server process shares.
pub struct CallServer {
    fns: Box<dyn ServerFns>,
    /// The write lock of the process owns the writer connection, so the writer connection is
    /// reachable only while the lock is held (ARCHITECTURE 5.4 "Connections").
    writer: FifoLock<Box<dyn Connection>>,
    reader: FifoLock<Box<dyn Connection>>,
    audit: FifoLock<Box<dyn AuditStore>>,
    clock: RefCell<Box<dyn Clock>>,
    limits: Limits,
}

impl CallServer {
    /// A call server. `writer` and `reader` are two connections to the same app database; the
    /// reader must be opened in the adapter's reader form (D161). `limits` are the VM limits
    /// of a call; under the write lock the step limit is at most [`LOCKED_CALL_STEPS`].
    pub fn new(
        fns: Box<dyn ServerFns>,
        writer: Box<dyn Connection>,
        reader: Box<dyn Connection>,
        audit: Box<dyn AuditStore>,
        clock: Box<dyn Clock>,
        limits: Limits,
    ) -> Self {
        CallServer {
            fns,
            writer: FifoLock::new(writer),
            reader: FifoLock::new(reader),
            audit: FifoLock::new(audit),
            clock: RefCell::new(clock),
            limits,
        }
    }

    /// The write lock with the writer connection. Every other write transaction of the
    /// process (op batches, closures, registration) takes the same lock.
    pub fn write_lock(&self) -> &FifoLock<Box<dyn Connection>> {
        &self.writer
    }

    /// The read connection.
    pub fn read_connection(&self) -> &FifoLock<Box<dyn Connection>> {
        &self.reader
    }

    /// Runs one call of `function` with `args` for `principal` (module documentation).
    pub async fn call(
        &self,
        principal: Option<RowId>,
        function: FnId,
        args: &[Value],
    ) -> Result<Result<Value, CallError>, CommitFailed> {
        let mut clock = SharedClock(&self.clock);
        let mut audit = SharedAudit(&self.audit);
        let mut host = TaskHost::new(principal, &mut clock, &mut audit);

        if !self.fns.is_locking(function) {
            let result = self
                .fns
                .invoke(
                    CallDb::Read(&self.reader),
                    &mut host,
                    function,
                    args,
                    self.limits,
                )
                .await;
            let (_, audit_failure) = host.finish();
            return Ok(settle(result, audit_failure));
        }

        if let Err(refused) = self.pre_check(principal, function, args).await {
            return Ok(Err(refused));
        }

        let mut writer = self.writer.lock().await;
        let Ok(mut tx) = writer.begin().await else {
            return Ok(Err(CallError::Unavailable));
        };
        let limits = Limits {
            max_steps: self.limits.max_steps.min(LOCKED_CALL_STEPS),
            ..self.limits
        };
        let result = {
            let mut locked = LockedHost(&mut host);
            self.fns
                .invoke(
                    CallDb::Locked(tx.as_mut()),
                    &mut locked,
                    function,
                    args,
                    limits,
                )
                .await
        };
        if result.is_ok() {
            tx.commit().await.map_err(CommitFailed)?;
        } else {
            // A failed rollback leaves nothing visible: the transaction never commits.
            let _ = tx.rollback().await;
        }
        drop(writer);
        let (_, audit_failure) = host.finish();
        Ok(settle(result, audit_failure))
    }

    /// The pre-check of a locking call in one read transaction, ended before the call waits
    /// for the write lock (D161, D162).
    async fn pre_check(
        &self,
        principal: Option<RowId>,
        function: FnId,
        args: &[Value],
    ) -> Result<(), CallError> {
        let mut reader = self.reader.lock().await;
        let Ok(mut tx) = reader.begin().await else {
            return Err(CallError::Unavailable);
        };
        let admitted = self.fns.admit(tx.as_mut(), principal, function, args).await;
        // A read transaction has nothing to keep; rolling it back only ends it.
        let _ = tx.rollback().await;
        admitted
    }
}

impl fmt::Debug for CallServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallServer")
            .field("write_lock", &self.writer)
            .field("read_connection", &self.reader)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// The call error the client gets for a fn that returned no value.
///
/// A runtime error right after an audit entry that could not be written, with no host event in
/// between, is `Unavailable`: the failed entry stopped the elevated call before its body (D134).
/// The host decides this exactly as it does for a task ([`TaskHost::audit_failure`]).
fn settle(
    result: Result<Value, FnFault>,
    audit_failure: Option<AuditError>,
) -> Result<Value, CallError> {
    match result {
        Ok(value) => Ok(value),
        Err(FnFault::Refused(error)) => Err(error),
        Err(FnFault::Runtime(_)) if audit_failure.is_some() => Err(CallError::Unavailable),
        Err(FnFault::Runtime(error)) => Err(CallError::Runtime {
            kind: error.kind.name().to_owned(),
        }),
    }
}

/// The clock of the process, read by one call at a time.
struct SharedClock<'a>(&'a RefCell<Box<dyn Clock>>);

impl Clock for SharedClock<'_> {
    fn now_ms(&mut self) -> SafeInt {
        self.0.borrow_mut().now_ms()
    }
}

/// The audit store of the process; one append at a time, in the order the calls ask.
struct SharedAudit<'a>(&'a FifoLock<Box<dyn AuditStore>>);

impl AuditStore for SharedAudit<'_> {
    fn append<'b>(&'b mut self, record: AuditRecord) -> BoxFuture<'b, Result<(), AuditError>> {
        Box::pin(async move {
            let mut store = self.0.lock().await;
            store.append(record).await
        })
    }
}

/// The host of a call that holds the write lock: a bridge call fails at once with `Busy` and
/// is not sent (ARCHITECTURE 5.4, 5.9, D156); everything else is the task host.
struct LockedHost<'h, 'a>(&'h mut TaskHost<'a>);

impl Host for LockedHost<'_, '_> {
    fn print(&mut self, text: &str) {
        self.0.print(text);
    }
}

impl ServerHost for LockedHost<'_, '_> {
    fn principal(&self) -> Option<RowId> {
        self.0.principal()
    }

    fn now(&mut self) -> SafeInt {
        self.0.now()
    }

    fn extern_call<'b>(
        &'b mut self,
        module: &'b str,
        function: &'b str,
        _args: &'b [Value],
    ) -> BoxFuture<'b, Result<Value, ExternError>> {
        let message = format!("{module}.{function} called while the call holds the write lock");
        Box::pin(async move {
            Err(ExternError {
                kind: ExternErrorKind::Busy,
                message,
            })
        })
    }

    fn audit<'b>(&'b mut self, entry: &'b AuditEntry) -> BoxFuture<'b, Result<(), AuditError>> {
        self.0.audit(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ostrel_vm::RuntimeErrorKind;
    use ostrel_vm::Span;

    fn runtime(kind: RuntimeErrorKind) -> FnFault {
        FnFault::Runtime(RuntimeError {
            kind,
            span: Span::default(),
        })
    }

    #[test]
    fn a_value_and_a_refusal_pass_unchanged() {
        assert_eq!(settle(Ok(Value::Bool(true)), None), Ok(Value::Bool(true)));
        assert_eq!(
            settle(Err(FnFault::Refused(CallError::Denied)), None),
            Err(CallError::Denied)
        );
        // A refusal is the answer also after a failed audit entry of a nested call.
        assert_eq!(
            settle(
                Err(FnFault::Refused(CallError::NotFound)),
                Some(AuditError("disk full".to_owned()))
            ),
            Err(CallError::NotFound)
        );
    }

    #[test]
    fn a_runtime_error_travels_by_its_kind_name() {
        assert_eq!(
            settle(Err(runtime(RuntimeErrorKind::StepLimit)), None),
            Err(CallError::Runtime {
                kind: "StepLimit".to_owned()
            })
        );
    }

    #[test]
    fn a_runtime_error_right_after_a_failed_audit_entry_is_unavailable() {
        assert_eq!(
            settle(
                Err(runtime(RuntimeErrorKind::DivisionByZero)),
                Some(AuditError("disk full".to_owned()))
            ),
            Err(CallError::Unavailable)
        );
    }
}
