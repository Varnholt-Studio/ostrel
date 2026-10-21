//! The server side of the WebSocket session (ARCHITECTURE 5.4, 5.7, 5.9).
//!
//! This version carries the `Call` and `Return` path:
//!
//! * [`session`] reads the frames of a session after its accepted `Hello`, answers refused
//!   frames, hands sync messages on and keeps the open calls, which give the result type of a
//!   `return` ([`ostrel_sync::wire::Requests::call_function`]).
//! * [`call`] runs one call: a fn that is not locking without a lock on the read connection; a
//!   locking fn after a pre-check without the lock, then under the write lock from its entry, in
//!   one transaction on the writer connection (D159, D161, D162).
//! * [`fifo`] is the first in, first out lock of the process that guards the writer connection,
//!   the read connection and the audit store.
//!
//! * [`hello`] admits a session: protocol fields, token and device key status, replica,
//!   resume mark and sessions per principal, then supersedes an older session of the same
//!   replica; it also keeps the `Call` rate per principal (D156, D158, D159, D161).
//!
//! The socket itself (tokio, hyper and tokio-tungstenite of ARCHITECTURE 11) and the sync
//! messages follow in later versions; every part here is driven by plain futures, so tests
//! replay every interleaving.

pub mod call;
pub mod fifo;
pub mod hello;
pub mod ops;
pub mod session;

pub use call::{CallDb, CallServer, CommitFailed, FnFault, LOCKED_CALL_STEPS, ReadDb, ServerFns};
pub use fifo::{FifoGuard, FifoLock};
pub use hello::{
    Admitted, HelloInput, HelloRefused, Hellos, Identity, PrincipalKey, ReplicaStore, SessionEnd,
    SessionLimits, SessionTicket, StoreFailed, fatal_frame, read_hello, welcome_frame,
};
pub use session::{CallJob, Incoming, OpenCalls, Session};
