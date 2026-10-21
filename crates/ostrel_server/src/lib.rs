//! API server of the Ostrel programming language (ARCHITECTURE 4.1, owner T4).
//!
//! This crate embeds the bytecode VM through `ostrel_vm::run` and implements the server
//! side of the host contract, [`ostrel_vm::host::ServerHost`] (F3, D117), for one VM task:
//!
//! * [`identity`] resolves the principal (`me`) of a request: a valid token always wins, a
//!   request without a token is anonymous, and an invalid token is an error that never falls
//!   back to anything else. The development identity of AC-12 exists only behind the Cargo
//!   feature `dev-identity`, which cannot be built without debug assertions (D127).
//! * [`audit`] stores the audit entry of every elevated execution with the SHA-256 of the
//!   canonical encoding of its argument list, never the arguments themselves (D117, D118).
//! * [`key_status`] checks the status of the device key behind a token on every `Hello` and
//!   HTTP request and closes the open sessions of a revoked key (D102).
//! * [`secret`] holds a `replica_secret` (D111) and keeps it out of every `Debug` output.
//! * [`host`] is the [`ostrel_vm::host::ServerHost`] of one VM task, and [`task`] runs a
//!   program image with it next to the database connection of the task.
//!
//! Secrets never reach a log: tokens, replica secrets and audit arguments have no `Debug`
//! or `Display` output that shows them (olga #1655, georg #1572 F9, D122).
//!
//! The WebSocket transport (`Call`/`Return` frames of ARCHITECTURE 5.4) is not part of this
//! crate version; it follows once the wire codec of `ostrel_sync` is merged.

// This crate handles requests from untrusted clients: no unwrap, expect, panic or
// unchecked indexing.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

// D127: the development identity must be impossible in a release build. A release profile
// may still enable debug assertions, but `ostrel build` enables this feature only with
// `--debug`, and this guard stops every build that combines it with a release profile
// without debug assertions.
#[cfg(all(feature = "dev-identity", not(debug_assertions)))]
compile_error!(
    "the `dev-identity` feature of ostrel_server is only allowed in debug builds (AC-12, D127)"
);

pub mod audit;
pub mod host;
pub mod identity;
pub mod key_status;
pub mod secret;
pub mod task;

/// Hex digits of a SHA-256 digest, lowercase, as audit records and logs show it.
pub fn sha256_hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(64);
    for byte in digest {
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_hex_is_lowercase_and_complete() {
        let mut digest = [0u8; 32];
        digest[0] = 0xab;
        digest[31] = 0x01;
        let hex = sha256_hex(&digest);
        assert_eq!(hex.len(), 64);
        assert!(hex.starts_with("ab00"));
        assert!(hex.ends_with("0001"));
    }
}
