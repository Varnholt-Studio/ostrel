//! The host contract of the VM inside `ostrel_server` (freeze wave F3, ARCHITECTURE 4.1
//! and 13).
//!
//! The CLI needs only [`Host::print`]. The server adds by addition what a server function
//! reaches outside the VM: the principal of the request, the clock, calls into the Node
//! sidecar (ARCHITECTURE 7.2, D60), the audit log of elevated code (ARCHITECTURE 5.7,
//! AC-32) and the database through the abstract interface `ostrel_db::api` (ARCHITECTURE 6).
//!
//! Concurrency follows ARCHITECTURE 4.1: tokio `current_thread` with one `LocalSet`, one VM
//! task per request or incoming batch, values `Rc` based. Every database call, sidecar call
//! and audit write is an await point, so every such method returns a [`BoxFuture`], which
//! is not `Send`, like the futures of `ostrel_db::api` (ARCHITECTURE 6.2).
//!
//! The database connection is not reached through a method of [`ServerHost`] but sits next
//! to it in [`ServerContext`]; the server sets its type parameter to
//! `dyn ostrel_db::api::Connection`. A `Transaction<'c>` borrows its connection for `'c`
//! (ARCHITECTURE 6.1); if the connection were behind `&mut self` of the host, an open
//! transaction would lock the whole host and the VM could not read the clock, call the
//! sidecar or write the audit entry inside it. Two disjoint borrows keep both usable.
//!
//! The incoming call of a server function (the RPC from a client, or a batch of ops) is not
//! a host method: it is the entry into the VM. Its principal reaches the VM through
//! [`ServerHost::principal`], and every rule the VM evaluates uses it (AC-11, AC-32).
//!
//! Values cross this boundary as the persisted and wire [`Value`] of `ostrel_core`; the VM
//! converts its own `Rc` based values at the boundary (ARCHITECTURE 5.3).

use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub use ostrel_core::ids::RowId;
pub use ostrel_core::value::{SafeInt, Value};

use crate::Host;

/// A boxed future that may borrow for `'a`. Not `Send`. The same type as
/// `ostrel_db::api::BoxFuture`, so futures of both contracts mix freely.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// What a server function reaches outside the VM, apart from the database (see
/// [`ServerContext`]). `ostrel_server` implements it once per VM task.
pub trait ServerHost: Host {
    /// The user row of the principal the request is evaluated against (`me`), or `None`
    /// for a request without a verified principal. From v0.3 it comes from a verified
    /// PASETO v4.public token (D18); in debug builds the test harness may supply a
    /// development identity (AC-12). It does not change during one VM task.
    fn principal(&self) -> Option<RowId>;

    /// The current time in milliseconds since the Unix epoch, the value of a `Time`.
    /// The server reads its clock here; tests implement it with a controllable clock and
    /// never wait on the wall clock (D74).
    fn now(&mut self) -> SafeInt;

    /// Calls the export `function` of the extern module `module` (the `as` name of an
    /// `extern js server` block) in the Node sidecar with `args` and awaits its result.
    ///
    /// The supervisor of ARCHITECTURE 7.2 sits behind this method: it encodes the request,
    /// applies the timeout, the 64 call slots and the restart policy, and checks the result
    /// against the declared type before it returns. A failure is always one of the kinds
    /// of [`ExternErrorKind`] (SPEC 12.8); a value is never changed on the way. Nothing is
    /// retried automatically.
    fn extern_call<'a>(
        &'a mut self,
        module: &'a str,
        function: &'a str,
        args: &'a [Value],
    ) -> BoxFuture<'a, Result<Value, ExternError>>;

    /// Writes the audit entry of one execution of elevated (`as system`) code before that
    /// code runs (ARCHITECTURE 5.7, AC-32). On `Err` the VM does not run the elevated
    /// function and the request fails, so no elevated execution happens without its entry.
    fn audit<'a>(&'a mut self, entry: &'a AuditEntry) -> BoxFuture<'a, Result<(), AuditError>>;
}

/// Everything one VM task on the server works with: the host and, as a separate borrow,
/// the database connection of the task.
///
/// `D` is `dyn ostrel_db::api::Connection` in the server. The VM opens transactions on
/// `db` (`Connection::begin`) and may use `host` while one is open, because the two fields
/// are borrowed independently.
pub struct ServerContext<'h, D: ?Sized> {
    /// The connection of this VM task to the database of the app.
    pub db: &'h mut D,
    /// Principal, clock, sidecar, audit log and program output.
    pub host: &'h mut dyn ServerHost,
}

impl<D: ?Sized> fmt::Debug for ServerContext<'_, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerContext").finish_non_exhaustive()
    }
}

/// The failure kinds of a bridge call on the server (ARCHITECTURE 7.1, 7.2, SPEC 12.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExternErrorKind {
    /// The result does not have the declared type, or is not a plain JSON value.
    Type,
    /// The function threw or its promise rejected.
    Threw,
    /// The function returned `undefined`.
    Undefined,
    /// The request or the result is above the size or value budget of ARCHITECTURE 7.2.
    TooLarge,
    /// 64 calls are already open; the call was not sent.
    Busy,
    /// The call did not finish within its timeout.
    Timeout,
    /// The host process exited, was killed, is restarting or is down for good, or it
    /// answered with a protocol error.
    Crashed,
}

impl ExternErrorKind {
    /// The name a program sees for the kind.
    pub fn name(self) -> &'static str {
        match self {
            ExternErrorKind::Type => "Type",
            ExternErrorKind::Threw => "Threw",
            ExternErrorKind::Undefined => "Undefined",
            ExternErrorKind::TooLarge => "TooLarge",
            ExternErrorKind::Busy => "Busy",
            ExternErrorKind::Timeout => "Timeout",
            ExternErrorKind::Crashed => "Crashed",
        }
    }
}

impl fmt::Display for ExternErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A failed bridge call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternError {
    /// The kind the program sees.
    pub kind: ExternErrorKind,
    /// Details for the server log; for `Threw` the cut message of ARCHITECTURE 7.2. Never
    /// carries a stack.
    pub message: String,
}

impl fmt::Display for ExternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            write!(f, "extern call failed: {}", self.kind)
        } else {
            write!(f, "extern call failed: {}: {}", self.kind, self.message)
        }
    }
}

impl std::error::Error for ExternError {}

/// One audit log entry of an elevated execution (ARCHITECTURE 5.7: function, caller,
/// arguments hash, time; AC-32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// Qualified name of the elevated function.
    pub function: String,
    /// The principal of the original request, as [`ServerHost::principal`] gives it.
    pub principal: Option<RowId>,
    /// SHA-256 of the canonical encoding (ARCHITECTURE 5.3) of the arguments as a list.
    pub arguments_hash: [u8; 32],
    /// Time of the entry, from [`ServerHost::now`].
    pub time: SafeInt,
}

/// The audit entry could not be written. The text is for the server log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditError(pub String);

impl fmt::Display for AuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "audit entry not written: {}", self.0)
    }
}

impl std::error::Error for AuditError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// Drives a future that never waits on outside events, as the in memory driver and
    /// the fake host here do. Fails the test instead of spinning if it would block.
    fn ready<F: Future>(fut: F) -> F::Output {
        let mut fut = pin!(fut);
        let mut cx = Context::from_waker(Waker::noop());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => out,
            Poll::Pending => unreachable_pending(),
        }
    }

    #[allow(clippy::panic)]
    fn unreachable_pending() -> ! {
        panic!("future is pending although nothing in this test waits")
    }

    #[derive(Default)]
    struct FakeHost {
        clock: i64,
        lines: Vec<String>,
        calls: Vec<(String, String, Vec<Value>)>,
        audit: Vec<AuditEntry>,
        fail_audit: bool,
        down: bool,
    }

    impl Host for FakeHost {
        fn print(&mut self, text: &str) {
            self.lines.push(text.to_owned());
        }
    }

    impl ServerHost for FakeHost {
        fn principal(&self) -> Option<RowId> {
            None
        }

        fn now(&mut self) -> SafeInt {
            self.clock += 1;
            SafeInt::new(self.clock).unwrap_or_else(|_| unreachable_pending())
        }

        fn extern_call<'a>(
            &'a mut self,
            module: &'a str,
            function: &'a str,
            args: &'a [Value],
        ) -> BoxFuture<'a, Result<Value, ExternError>> {
            Box::pin(async move {
                if self.down {
                    return Err(ExternError {
                        kind: ExternErrorKind::Crashed,
                        message: String::new(),
                    });
                }
                self.calls
                    .push((module.to_owned(), function.to_owned(), args.to_vec()));
                Ok(Value::Int(
                    SafeInt::new(args.len() as i64).unwrap_or_else(|_| unreachable_pending()),
                ))
            })
        }

        fn audit<'a>(&'a mut self, entry: &'a AuditEntry) -> BoxFuture<'a, Result<(), AuditError>> {
            Box::pin(async move {
                if self.fail_audit {
                    return Err(AuditError("log closed".to_owned()));
                }
                self.audit.push(entry.clone());
                Ok(())
            })
        }
    }

    /// A connection shaped like `ostrel_db::api::Connection`: its transaction borrows the
    /// connection for its whole life, exactly like `Transaction<'c>` (ARCHITECTURE 6.1).
    #[derive(Default)]
    struct FakeConnection {
        sequence: u64,
    }

    struct FakeTransaction<'c> {
        conn: &'c mut FakeConnection,
        sequence: u64,
    }

    impl FakeConnection {
        fn begin<'c>(&'c mut self) -> BoxFuture<'c, FakeTransaction<'c>> {
            Box::pin(async move {
                let sequence = self.sequence;
                FakeTransaction {
                    conn: self,
                    sequence,
                }
            })
        }
    }

    impl FakeTransaction<'_> {
        fn next_in_sequence(&mut self) -> u64 {
            self.sequence += 1;
            self.sequence
        }

        fn commit(self) {
            self.conn.sequence = self.sequence;
        }
    }

    #[test]
    fn host_is_usable_while_a_transaction_is_open() {
        let mut conn = FakeConnection::default();
        let mut host = FakeHost::default();
        let ctx = ServerContext {
            db: &mut conn,
            host: &mut host,
        };

        let (n, value, time, audited) = ready(async {
            let mut tx = ctx.db.begin().await;
            let n = tx.next_in_sequence();
            // Clock, sidecar, audit log and output while `tx` still borrows the connection.
            let time = ctx.host.now();
            let args = [Value::Text("a".to_owned()), Value::Bool(true)];
            let value = ctx.host.extern_call("mail", "send", &args).await;
            let entry = AuditEntry {
                function: "admin.reset".to_owned(),
                principal: ctx.host.principal(),
                arguments_hash: [0; 32],
                time,
            };
            let audited = ctx.host.audit(&entry).await;
            ctx.host.print("inside");
            tx.commit();
            (n, value, time, audited)
        });

        assert_eq!(n, 1);
        assert_eq!(time.get(), 1);
        assert_eq!(audited, Ok(()));
        let two = SafeInt::new(2).unwrap_or_else(|_| unreachable_pending());
        assert_eq!(value, Ok(Value::Int(two)));
        // The committed step is visible to the next transaction of the task.
        let next = ready(async { ctx.db.begin().await.next_in_sequence() });
        assert_eq!(next, 2);
        assert_eq!(host.lines, ["inside"]);
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.audit.len(), 1);
    }

    #[test]
    fn host_failures_are_typed() {
        let mut host = FakeHost {
            fail_audit: true,
            down: true,
            ..FakeHost::default()
        };
        let call = ready(host.extern_call("mail", "send", &[]));
        assert_eq!(
            call.as_ref().map_err(|e| e.kind),
            Err(ExternErrorKind::Crashed)
        );
        assert_eq!(
            call.map_err(|e| e.to_string()),
            Err("extern call failed: Crashed".to_owned())
        );
        let entry = AuditEntry {
            function: "f".to_owned(),
            principal: None,
            arguments_hash: [0; 32],
            time: host.now(),
        };
        let audited = ready(host.audit(&entry));
        assert_eq!(
            audited.map_err(|e| e.to_string()),
            Err("audit entry not written: log closed".to_owned())
        );
        assert!(host.audit.is_empty());
    }

    #[test]
    fn kind_names_follow_spec_12_8() {
        let kinds = [
            ExternErrorKind::Type,
            ExternErrorKind::Threw,
            ExternErrorKind::Undefined,
            ExternErrorKind::TooLarge,
            ExternErrorKind::Busy,
            ExternErrorKind::Timeout,
            ExternErrorKind::Crashed,
        ];
        let names: Vec<_> = kinds.iter().map(|k| k.to_string()).collect();
        assert_eq!(
            names,
            [
                "Type",
                "Threw",
                "Undefined",
                "TooLarge",
                "Busy",
                "Timeout",
                "Crashed"
            ]
        );
    }

    #[test]
    fn server_host_is_object_safe_and_a_host() {
        fn print_through(host: &mut dyn ServerHost) {
            host.print("x");
        }
        let mut host = FakeHost::default();
        print_through(&mut host);
        crate::run(
            &crate::Image::default(),
            &mut host,
            crate::Limits::default(),
        )
        .unwrap_or_else(|_| unreachable_pending());
        assert_eq!(host.lines, ["x"]);
    }
}
