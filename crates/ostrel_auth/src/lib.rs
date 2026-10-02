//! PASETO tokens and identity primitives of the Ostrel programming language.
//!
//! The server signs session tokens as PASETO `v4.public` (Ed25519) and verifies them on
//! every connection and request (ARCHITECTURE 8). This crate holds the token layer:
//! pre authentication encoding, signing and verification, plus the checks of the session
//! claims `sub`, `kid`, `exp` and `nbf` once their values are read from the signed message.
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

mod claims;
mod paseto;

pub use claims::{
    Claim, ClaimInput, Claims, ClaimsError, MAX_KID_LEN, Policy, SUB_HEX_LEN, UnixTime,
    check_claims,
};

pub use paseto::{
    Error, MAX_TOKEN_LEN, PublicKey, SecretKey, V4_PUBLIC_HEADER, Verified, pae, sign, verify,
};
