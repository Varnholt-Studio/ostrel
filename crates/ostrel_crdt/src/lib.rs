//! Row granular CRDTs (ARCHITECTURE 5.1, 5.2, 6.2), the Rust side of `runtime/js/crdt/`.
//!
//! * [`LwwRegister`]: last writer wins register, the default merge of every scalar field.
//! * [`AddWinsSet`]: `Set[T]`, observed remove set with add tags, add wins (D49).
//! * [`LwwMap`]: `Map[K, V]`, last writer wins per key with tombstones (G6).
//!
//! All three are built on the `ostrel_core` types: [`Hlc`](ostrel_core::ids::Hlc) and
//! [`OpId`](ostrel_core::ids::OpId) identify writes, [`Value`](ostrel_core::value::Value) carries what is written, and
//! the replica state is compared by its canonical encoding (ARCHITECTURE 5.3), which
//! `encode_state` and `encode_value` produce byte identical to the JavaScript runtime.
//! The shared vectors in `tests/crdt-vectors/` check both sides.
//!
//! Delivery (D62): the CRDTs assume exactly once delivery in log order, which the sync layer
//! guarantees. Replays are still harmless where the merge rule allows it: a register or map
//! write with an `Hlc` already applied changes nothing, and an add whose seq is not above the
//! replica's live tag is ignored. A set add delivered again after the remove that named its
//! tag would bring the element back; that order is the sync layer's to prevent.
//!
//! Element and key order is the wire order of D61,
//! [`compare_key`](ostrel_core::value::compare_key), never the canonical bytes.
//!
//! Every method validates its input before it changes anything: on `Err` the replica is
//! unchanged. Every [`CrdtError`] maps to the protocol reason `Invalid`.

// Ops arrive from untrusted replicas: no unwrap, expect, panic or unchecked indexing outside
// of tests.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod error;
mod keyed;
mod lww;
mod map;
mod set;

pub use error::CrdtError;
pub use lww::LwwRegister;
pub use map::{LwwMap, MapEntry};
pub use set::{AddWinsSet, MAX_REMOVE_TAGS};
