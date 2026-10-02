//! PASETO tokens and identity primitives of the Ostrel programming language.
//!
//! The server signs session tokens as PASETO `v4.public` (Ed25519) and verifies them on
//! every connection and request (ARCHITECTURE 8). This crate holds the token layer:
//! pre authentication encoding, signing and verification. Claims such as `sub`, `kid`
//! and `exp` live in the signed message and are checked by the caller.
//!
//! Every token handed to [`verify`] is treated as hostile input: its length is bounded,
//! its encoding must be canonical, and only `v4.public` is accepted.

// This crate handles untrusted input: no unwrap, expect, panic or unchecked indexing.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod paseto;

pub use paseto::{
    Error, MAX_TOKEN_LEN, PublicKey, SecretKey, V4_PUBLIC_HEADER, Verified, pae, sign, verify,
};
