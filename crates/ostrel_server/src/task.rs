//! One VM task per request (ARCHITECTURE 4.1).
//!
//! A request enters the VM here: the principal is resolved once from the credentials of the
//! request ([`crate::identity`]), a [`TaskHost`] is built for it, and the program runs with
//! the host and, as a separate borrow, the database connection in a [`ServerContext`]
//! (D117). An invalid token stops the request before the VM starts.
//!
//! The VM of this version runs `fn main()` of a script program (`ostrel_vm::run`); the entry
//! of a named server function with arguments follows as an addition of `ostrel_vm` and is
//! wired here when it exists.

use std::fmt;

use ostrel_core::ids::RowId;
use ostrel_db::api::Connection;
use ostrel_vm::host::ServerContext;
use ostrel_vm::{Image, Limits, RuntimeError};

use crate::audit::AuditStore;
use crate::host::{Clock, TaskHost};
use crate::identity::{Credentials, IdentityError, TokenVerifier, resolve_principal};

/// The result of one task that ran to its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The principal the task ran as; `None` for an anonymous request.
    pub principal: Option<RowId>,
    /// The lines the program printed.
    pub output: Vec<String>,
}

/// Why a task did not run to its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskError {
    /// The request has no principal; the VM did not start.
    Identity(IdentityError),
    /// The VM stopped with a runtime error. `output` holds what was printed before it.
    Runtime {
        /// The error the VM reported.
        error: RuntimeError,
        /// Lines printed before the error.
        output: Vec<String>,
    },
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskError::Identity(e) => e.fmt(f),
            TaskError::Runtime { error, .. } => {
                write!(f, "runtime error[{}]: {}", error.kind, error.kind.message())
            }
        }
    }
}

impl std::error::Error for TaskError {}

/// A server for one program image: everything a VM task needs that outlives the task.
pub struct Server {
    image: Image,
    limits: Limits,
    db: Box<dyn Connection>,
    audit: Box<dyn AuditStore>,
    clock: Box<dyn Clock>,
    verifier: Box<dyn TokenVerifier>,
}

impl Server {
    /// A server that runs `image` within `limits` against `db`.
    pub fn new(
        image: Image,
        limits: Limits,
        db: Box<dyn Connection>,
        audit: Box<dyn AuditStore>,
        clock: Box<dyn Clock>,
        verifier: Box<dyn TokenVerifier>,
    ) -> Self {
        Server {
            image,
            limits,
            db,
            audit,
            clock,
            verifier,
        }
    }

    /// Runs one task for a request with `credentials`.
    pub fn run(&mut self, credentials: &Credentials) -> Result<Outcome, TaskError> {
        let principal =
            resolve_principal(credentials, self.verifier.as_ref()).map_err(TaskError::Identity)?;
        let mut host = TaskHost::new(principal, self.clock.as_mut(), self.audit.as_mut());
        let ctx: ServerContext<'_, dyn Connection> = ServerContext {
            db: self.db.as_mut(),
            host: &mut host,
        };
        let result = run_in(&self.image, ctx, self.limits);
        let output = host.into_output();
        match result {
            Ok(()) => Ok(Outcome { principal, output }),
            Err(error) => Err(TaskError::Runtime { error, output }),
        }
    }
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// Runs `image` in the context of one task. The VM reaches the host through `ctx.host`;
/// `ctx.db` is the connection of the task, open for the VM entries that use it.
pub fn run_in(
    image: &Image,
    ctx: ServerContext<'_, dyn Connection>,
    limits: Limits,
) -> Result<(), RuntimeError> {
    ostrel_vm::run(image, ctx.host, limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::MemoryAuditLog;
    use crate::host::tests::{StepClock, ready};
    use crate::identity::{NoTokens, TokenRejected};
    use ostrel_core::FileId;
    use ostrel_db::api::Driver;
    use ostrel_db_memory::MemoryDriver;
    use ostrel_vm::RuntimeErrorKind;

    fn image(src: &str) -> Image {
        let file = FileId::from_raw(0);
        let (tokens, lexed) = ostrel_syntax::lex::lex(src, file);
        assert!(lexed.is_empty(), "{lexed:?}");
        let (module, parsed) = ostrel_syntax::parse::parse(src, file, &tokens, &lexed);
        assert!(parsed.is_empty(), "{parsed:?}");
        let (checked, diags) = ostrel_sema::check(&module, file, src);
        assert!(diags.is_empty(), "{diags:?}");
        let program = ostrel_ir::lower(&module, &checked, file)
            .unwrap_or_else(|e| unreachable!("lowering failed: {e:?}"));
        ostrel_vm::compile(&program).unwrap_or_else(|e| unreachable!("compile failed: {e:?}"))
    }

    fn server(src: &str, verifier: Box<dyn TokenVerifier>) -> Server {
        let db = ready(MemoryDriver::new().connect("memory:"))
            .unwrap_or_else(|e| unreachable!("memory driver: {e:?}"));
        Server::new(
            image(src),
            Limits::default(),
            db,
            Box::new(MemoryAuditLog::new()),
            Box::new(StepClock::default()),
            verifier,
        )
    }

    fn user(n: u8) -> RowId {
        RowId::from_hex(&format!("{n:032x}")).unwrap_or_else(|_| unreachable!("valid hex"))
    }

    struct OneToken;

    impl TokenVerifier for OneToken {
        fn verify(&self, token: &str) -> Result<RowId, TokenRejected> {
            match token {
                "good" => Ok(user(9)),
                _ => Err(TokenRejected("bad signature".to_owned())),
            }
        }
    }

    const HELLO: &str = "fn main()\n  print(\"Hello, world!\")\n  print(6 * 7)\n";

    #[test]
    fn program_runs_inside_the_server() {
        let mut server = server(HELLO, Box::new(NoTokens));
        let outcome = server.run(&Credentials::anonymous());
        assert_eq!(
            outcome,
            Ok(Outcome {
                principal: None,
                output: vec!["Hello, world!".to_owned(), "42".to_owned()],
            })
        );
        // The server serves the next request with the same image and connection.
        assert!(server.run(&Credentials::anonymous()).is_ok());
    }

    #[test]
    fn runtime_error_keeps_earlier_output() {
        let src = "fn main()\n  print(\"before\")\n  print(1 / 0)\n";
        let mut server = server(src, Box::new(NoTokens));
        match server.run(&Credentials::anonymous()) {
            Err(TaskError::Runtime { error, output }) => {
                assert_eq!(error.kind, RuntimeErrorKind::DivisionByZero);
                assert_eq!(output, ["before"]);
            }
            other => unreachable!("expected a runtime error, got {other:?}"),
        }
    }

    #[test]
    fn task_runs_as_the_token_user() {
        let mut server = server(HELLO, Box::new(OneToken));
        let creds = Credentials::anonymous()
            .with_dev_identity(user(1))
            .with_token("good".to_owned());
        assert_eq!(server.run(&creds).map(|o| o.principal), Ok(Some(user(9))));
    }

    #[test]
    fn invalid_token_stops_the_request_before_the_vm() {
        let mut server = server(HELLO, Box::new(OneToken));
        let creds = Credentials::anonymous()
            .with_dev_identity(user(1))
            .with_token("forged".to_owned());
        assert_eq!(
            server.run(&creds),
            Err(TaskError::Identity(IdentityError::InvalidToken(
                TokenRejected("bad signature".to_owned())
            )))
        );
    }

    /// Groundwork for AC-12 (the release proof is T133): without the feature `dev-identity` the
    /// server runs a request that carries only a development identity as anonymous.
    #[cfg(not(feature = "dev-identity"))]
    #[test]
    fn server_without_feature_runs_dev_identity_as_anonymous() {
        let mut server = server(HELLO, Box::new(OneToken));
        let creds = Credentials::anonymous().with_dev_identity(user(1));
        assert_eq!(server.run(&creds).map(|o| o.principal), Ok(None));
    }

    /// Groundwork for AC-12 (the release proof is T133): in a debug build with the feature the
    /// development identity is the principal of a request without a token.
    #[cfg(feature = "dev-identity")]
    #[test]
    fn server_with_feature_runs_as_dev_identity() {
        let mut server = server(HELLO, Box::new(OneToken));
        let creds = Credentials::anonymous().with_dev_identity(user(1));
        assert_eq!(server.run(&creds).map(|o| o.principal), Ok(Some(user(1))));
    }
}
