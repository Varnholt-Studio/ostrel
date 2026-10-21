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
//! The socket itself (tokio, hyper and tokio-tungstenite of ARCHITECTURE 11), `Hello` with the
//! device key status, replicas and session limits, and the sync messages follow in later
//! versions; every part here is driven by plain futures, so tests replay every interleaving.

pub mod call;
pub mod fifo;
pub mod session;

pub use call::{
    CallDb, CallServer, CommitFailed, FnFault, LOCKED_CALL_STEPS, ReadDb, ServerFns,
};
pub use fifo::{FifoGuard, FifoLock};
pub use session::{CallJob, Incoming, OpenCalls, Session};
