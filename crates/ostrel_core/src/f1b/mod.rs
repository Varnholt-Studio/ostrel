//! F1b contract (ARCHITECTURE 5.3, work package F1B-CORE): replica and row identities, the
//! hybrid logical clock, the persisted and wire [`value::Value`], and the canonical JSON
//! writer shared with the JavaScript runtime.
//!
//! Everything here is std only. Parsing of wire JSON lives in `ostrel_sync`; this module only
//! validates hex ids and writes canonical bytes.

pub mod canon;
pub mod ids;
pub mod value;

#[cfg(test)]
mod vectors;
