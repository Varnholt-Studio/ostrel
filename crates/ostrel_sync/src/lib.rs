//! Sync between client replicas and the server of the Ostrel programming language.
//!
//! Status: F1b draft (ARCHITECTURE 13). [`protocol`] holds the message types, op bodies, reject
//! reasons and limits table of ARCHITECTURE 5.4, 5.5 and 5.9. It is a draft until the RED
//! re-check of ARCHITECTURE 5.3 to 5.6; the sync engine is not written yet.

pub mod protocol;
