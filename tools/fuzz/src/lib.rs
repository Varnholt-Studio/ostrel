//! Seeded, std only fuzzer for the Ostrel compiler front end (SPEC AC-36, decision #46).
//!
//! The fuzzer derives every input from `(seed, iteration, corpus)`, runs the `ostrel`
//! CLI on it in a separate process with a time and a memory limit, and keeps every
//! input that ends in anything other than exit code 0 or 1. See `README.md` for the
//! commands, the committed seeds and how a finding becomes a corpus case.

pub mod campaign;
pub mod cli;
pub mod exec;
pub mod generate;
pub mod mutate;
pub mod rng;
