//! The [`ServerHost`] of one VM task (ARCHITECTURE 4.1, D117).

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use ostrel_core::ids::RowId;
use ostrel_core::value::{SafeInt, Value};
use ostrel_vm::Host;
use ostrel_vm::host::{
    AuditEntry, AuditError, BoxFuture, ExternError, ExternErrorKind, ServerHost,
};

use crate::audit::{AuditRecord, AuditStore};

/// The clock of the server. Tests use a controlled clock and never wait on the wall clock
/// (D74).
pub trait Clock {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&mut self) -> SafeInt;
}

/// The wall clock of the machine.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

/// Largest `Int` (ARCHITECTURE 7.4); the wall clock stays below it for the next 285 000
/// years, the bound only keeps the conversion total.
const INT_MAX: i64 = 9_007_199_254_740_991;

impl Clock for SystemClock {
    fn now_ms(&mut self) -> SafeInt {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(INT_MAX));
        let ms = ms.clamp(0, INT_MAX);
        SafeInt::new(ms).unwrap_or_else(|_| unreachable!("clamped into the Int range"))
    }
}

/// The host of one VM task: principal, clock, audit log and program output.
///
/// The principal is fixed when the task starts and stays the same for every nested call
/// (D98, D117).
pub struct TaskHost<'a> {
    principal: Option<RowId>,
    clock: &'a mut dyn Clock,
    audit: &'a mut dyn AuditStore,
    output: Vec<String>,
    /// The failed audit entry, kept only while it is the latest event this host saw.
    audit_failure: Option<AuditError>,
}

impl<'a> TaskHost<'a> {
    /// A host for one task of `principal`.
    pub fn new(
        principal: Option<RowId>,
        clock: &'a mut dyn Clock,
        audit: &'a mut dyn AuditStore,
    ) -> Self {
        TaskHost {
            principal,
            clock,
            audit,
            output: Vec::new(),
            audit_failure: None,
        }
    }

    /// The lines the program printed, in order.
    pub fn output(&self) -> &[String] {
        &self.output
    }

    /// The audit entry that could not be written, if that failure is the latest event this
    /// host saw (D134). Any later host event (a written audit entry, `print`, `now` or a bridge
    /// call) clears it, because the program then went on after the failed call.
    pub fn audit_failure(&self) -> Option<&AuditError> {
        self.audit_failure.as_ref()
    }

    /// Records a host event that is not a failed audit entry.
    fn went_on(&mut self) {
        self.audit_failure = None;
    }

    /// Ends the task and returns its printed lines.
    pub fn into_output(self) -> Vec<String> {
        self.output
    }

    /// Ends the task and returns its printed lines and the failed audit entry, if that failure
    /// was the latest host event of the task.
    pub fn finish(self) -> (Vec<String>, Option<AuditError>) {
        (self.output, self.audit_failure)
    }
}

impl fmt::Debug for TaskHost<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskHost")
            .field("principal", &self.principal.map(RowId::to_hex))
            .field("output_lines", &self.output.len())
            .field("audit_failed", &self.audit_failure.is_some())
            .finish_non_exhaustive()
    }
}

impl Host for TaskHost<'_> {
    fn print(&mut self, text: &str) {
        self.went_on();
        self.output.push(text.to_owned());
    }
}

impl ServerHost for TaskHost<'_> {
    fn principal(&self) -> Option<RowId> {
        self.principal
    }

    fn now(&mut self) -> SafeInt {
        self.went_on();
        self.clock.now_ms()
    }

    /// ASSUMPTION: the sidecar supervisor of ARCHITECTURE 7.2 is not attached in this crate
    /// version, so every bridge call fails at once with `Crashed` (the host is down), is not
    /// sent and is not queued, like a call while the host is down (SPEC 12.8, D91).
    fn extern_call<'b>(
        &'b mut self,
        _module: &'b str,
        _function: &'b str,
        _args: &'b [Value],
    ) -> BoxFuture<'b, Result<Value, ExternError>> {
        self.went_on();
        Box::pin(async {
            Err(ExternError {
                kind: ExternErrorKind::Crashed,
                message: "sidecar is not attached".to_owned(),
            })
        })
    }

    /// Hashes the arguments and stores only the [`AuditRecord`] (D117, D118). The entry
    /// itself, with its arguments, is not kept and not logged.
    ///
    /// A failure is returned to the VM, which then does not run the body, and is also kept
    /// in the host until the next host event, so that a task that stops right after it
    /// reports the call as `Unavailable` (D134) and not as a runtime error of the program.
    /// A written entry clears an earlier failure.
    fn audit<'b>(&'b mut self, entry: &'b AuditEntry) -> BoxFuture<'b, Result<(), AuditError>> {
        let record = AuditRecord::from_entry(entry);
        Box::pin(async move {
            let result = self.audit.append(record).await;
            self.audit_failure = result.as_ref().err().cloned();
            result
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::audit::{MemoryAuditLog, hash_arguments};
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// Drives a future that never waits on outside events.
    pub(crate) fn ready<F: Future>(fut: F) -> F::Output {
        let mut fut = pin!(fut);
        let mut cx = Context::from_waker(Waker::noop());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => out,
            Poll::Pending => unreachable!("nothing in these tests waits"),
        }
    }

    /// A clock that advances by one millisecond per read.
    #[derive(Debug, Default)]
    pub(crate) struct StepClock(pub(crate) i64);

    impl Clock for StepClock {
        fn now_ms(&mut self) -> SafeInt {
            self.0 += 1;
            SafeInt::new(self.0).unwrap_or_else(|_| unreachable!("small test clock"))
        }
    }

    /// An audit store that refuses every record.
    pub(crate) struct ClosedLog;

    impl AuditStore for ClosedLog {
        fn append<'a>(&'a mut self, _: AuditRecord) -> BoxFuture<'a, Result<(), AuditError>> {
            Box::pin(async { Err(AuditError("log closed".to_owned())) })
        }
    }

    fn user() -> Option<RowId> {
        RowId::from_hex("000000000000000000000000000000ff").ok()
    }

    #[test]
    fn audit_stores_only_the_hash_of_the_arguments() {
        let mut clock = StepClock::default();
        let mut log = MemoryAuditLog::new();
        let secret = "card-4111111111111111";
        let args = vec![Value::Text(secret.to_owned())];
        {
            let mut host = TaskHost::new(user(), &mut clock, &mut log);
            let entry = AuditEntry {
                function: "billing.charge".to_owned(),
                principal: host.principal(),
                arguments: args.clone(),
                time: host.now(),
            };
            assert_eq!(ready(host.audit(&entry)), Ok(()));
            let debug = format!("{host:?}");
            assert!(!debug.contains(secret), "{debug}");
        }
        let [record] = log.records() else {
            unreachable!("exactly one record was written")
        };
        assert_eq!(record.arguments_sha256, hash_arguments(&args));
        assert_eq!(record.principal, user());
        assert_eq!(record.time.get(), 1);
        let all = format!("{log:?} {record}");
        assert!(!all.contains(secret), "{all}");
    }

    #[test]
    fn failed_audit_is_reported_to_the_vm() {
        let mut clock = StepClock::default();
        let mut log = ClosedLog;
        let mut host = TaskHost::new(None, &mut clock, &mut log);
        let entry = AuditEntry {
            function: "f".to_owned(),
            principal: None,
            arguments: Vec::new(),
            time: host.now(),
        };
        assert_eq!(
            ready(host.audit(&entry)),
            Err(AuditError("log closed".to_owned()))
        );
        // D134: the host keeps the failure, so the task can report `Unavailable`.
        assert_eq!(
            host.audit_failure(),
            Some(&AuditError("log closed".to_owned()))
        );
        assert!(format!("{host:?}").contains("audit_failed: true"));
    }

    /// An audit store that refuses the first record and accepts every later one.
    #[derive(Default)]
    struct FlakyLog {
        refused: bool,
        stored: usize,
    }

    impl AuditStore for FlakyLog {
        fn append<'a>(&'a mut self, _: AuditRecord) -> BoxFuture<'a, Result<(), AuditError>> {
            Box::pin(async move {
                if self.refused {
                    self.stored += 1;
                    Ok(())
                } else {
                    self.refused = true;
                    Err(AuditError("disk full".to_owned()))
                }
            })
        }
    }

    fn entry() -> AuditEntry {
        AuditEntry {
            function: "f".to_owned(),
            principal: None,
            arguments: Vec::new(),
            time: SafeInt::new(0).unwrap_or_else(|_| unreachable!("zero is an Int")),
        }
    }

    #[test]
    fn written_audit_entry_clears_an_earlier_failure() {
        let mut clock = StepClock::default();
        let mut log = FlakyLog::default();
        let failure = {
            let mut host = TaskHost::new(None, &mut clock, &mut log);
            assert!(ready(host.audit(&entry())).is_err());
            assert_eq!(
                host.audit_failure(),
                Some(&AuditError("disk full".to_owned()))
            );
            assert_eq!(ready(host.audit(&entry())), Ok(()));
            host.finish().1
        };
        assert_eq!(failure, None);
        assert_eq!(log.stored, 1);
    }

    /// D134: after the program went on past a failed call, a later error is not `Unavailable`.
    #[test]
    fn every_later_host_event_clears_an_audit_failure() {
        let events: [fn(&mut TaskHost<'_>); 3] = [
            |h| h.print("went on"),
            |h| {
                h.now();
            },
            |h| {
                let _ = ready(h.extern_call("mail", "send", &[]));
            },
        ];
        for event in events {
            let mut clock = StepClock::default();
            let mut log = ClosedLog;
            let mut host = TaskHost::new(None, &mut clock, &mut log);
            assert!(ready(host.audit(&entry())).is_err());
            event(&mut host);
            assert_eq!(host.audit_failure(), None);
            assert_eq!(host.finish().1, None);
        }
    }

    #[test]
    fn principal_and_clock_come_from_the_task() {
        let mut clock = StepClock(41);
        let mut log = MemoryAuditLog::new();
        let mut host = TaskHost::new(user(), &mut clock, &mut log);
        assert_eq!(host.principal(), user());
        assert_eq!(host.now().get(), 42);
        assert_eq!(host.now().get(), 43);
        host.print("line");
        assert_eq!(host.output(), ["line"]);
        assert_eq!(host.into_output(), ["line"]);
    }

    #[test]
    fn bridge_calls_fail_as_crashed_without_a_sidecar() {
        let mut clock = StepClock::default();
        let mut log = MemoryAuditLog::new();
        let mut host = TaskHost::new(None, &mut clock, &mut log);
        let result = ready(host.extern_call("mail", "send", &[]));
        assert_eq!(result.map_err(|e| e.kind), Err(ExternErrorKind::Crashed));
    }

    #[test]
    fn system_clock_is_after_2020() {
        // 2020-01-01T00:00:00Z in milliseconds.
        assert!(SystemClock.now_ms().get() > 1_577_836_800_000);
    }
}
