//! Sync between client replicas and the server of the Ostrel programming language.
//!
//! Status: F1b draft (ARCHITECTURE 13). [`protocol`] holds the message types, op bodies, reject
//! reasons and limits table of ARCHITECTURE 5.4, 5.5 and 5.9. It is a draft until the RED
//! re-check of ARCHITECTURE 5.3 to 5.6; the sync engine is not written yet. [`wire`] reads and
//! writes those messages as JSON frames. [`broadcast_fence`] is the ordered broadcast queue of
//! a server process (ARCHITECTURE 5.6, D126).

pub mod broadcast_fence;
pub mod protocol;
pub mod wire;
