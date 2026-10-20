//! JSON codec for the frames of the sync protocol (ARCHITECTURE 5.4, D106).
//!
//! [`decode_client`] and [`decode_server`] turn one received WebSocket frame into a message of
//! [`crate::protocol`]; [`encode_client`] and [`encode_server`] write one. Both sides use the
//! same functions: the server decodes client frames and encodes server frames, the client the
//! other way round.
//!
//! # Frame form
//!
//! A frame is one JSON object: `"v"` is [`PROTOCOL_VERSION`], `"t"` the message name, and every
//! other key is a field of the message in snake case. Every key is required; an absent
//! optional value is written as `null`. Ids, [`Hlc`](crate::protocol::Hlc) and
//! [`ServerSeq`](crate::protocol::ServerSeq) are fixed width lowercase hex strings, counts and
//! small ids are JSON integers, reasons and error codes their variant names. A pair (for
//! example a re-stamped op in [`Ack`](crate::protocol::Ack)) is a two element array. Encoded
//! frames are canonical (ARCHITECTURE 5.3): keys sorted, no whitespace.
//!
//! # Checks on every received frame
//!
//! In this order, each with its own [`WireError`]:
//!
//! 1. size: more than [`LIMITS`](crate::protocol::LIMITS)`.frame_bytes` bytes is
//!    [`WireError::FrameTooLarge`], before any parsing (D121, both directions);
//! 2. JSON: well formed UTF-8 JSON, no repeated key, at most [`MAX_DEPTH`] levels deep, every
//!    integer in the safe range of D24 ([`json`]);
//! 3. envelope: an object with version 0 and a known message name;
//! 4. fields: every field present with the right form, no other key.
//!
//! The codec checks form only. Rules that need the session or the app, such as a
//! `replica_secret` in an app with `auth` (D122), belong to the server.
//!
//! # Not yet covered
//!
//! Messages that carry a [`Value`](crate::protocol::Value), a query or a row (`subscribe`,
//! `ops` in both directions, `call`, `snapshot`, `return`) give [`WireError::Unsupported`]. A
//! value's JSON form does not name its type (ARCHITECTURE 5.1: a string may be `Text`, an
//! enum, a reference, `Bytes` or `Rank`), so reading one needs the field types of the compiled
//! schema; that interface and the wire form of queries, rows and op bodies follow in their own
//! task.

pub mod json;

mod frame;

use std::fmt;

use ostrel_core::ids::InvalidId;

use crate::protocol::ErrorCode;

pub use frame::{decode_client, decode_server, encode_client, encode_server};
pub use json::MAX_DEPTH;

#[cfg(doc)]
use crate::protocol::PROTOCOL_VERSION;

/// Longest key or name a [`WireError`] repeats from a received frame, in bytes.
pub const MAX_ECHO_BYTES: usize = 64;

/// Why a frame could not be read or written. Every kind except
/// [`WireError::FrameTooLarge`] is a fatal [`ErrorCode::Protocol`] ([`WireError::code`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    /// The frame is larger than the frame limit (ARCHITECTURE 5.9, D121).
    FrameTooLarge { bytes: usize },
    /// Not one well formed JSON text in UTF-8. The text comes from the JSON parser and names a
    /// position, never frame content.
    Malformed(String),
    /// Arrays and objects nested deeper than [`MAX_DEPTH`].
    TooDeep,
    /// An object names this key twice (cut to [`MAX_ECHO_BYTES`]).
    DuplicateKey(String),
    /// An integer outside the safe range `-(2^53 - 1)..=2^53 - 1` (D24).
    NumberOutOfRange,
    /// The frame is not an object.
    NotAnObject,
    /// `"v"` is not the protocol version this codec speaks.
    UnsupportedVersion,
    /// `"t"` names no message of this direction (cut to [`MAX_ECHO_BYTES`]).
    UnknownMessage(String),
    /// A required key is absent.
    MissingKey(&'static str),
    /// A key that the message does not have (cut to [`MAX_ECHO_BYTES`]).
    UnknownKey(String),
    /// The value of this key has the wrong JSON kind or an unknown name.
    WrongType(&'static str),
    /// An integer of this key is outside the range of its field, for example a negative id.
    FieldRange(&'static str),
    /// An id, stamp or log position of this key is not in its strict hex form.
    InvalidId(&'static str, InvalidId),
    /// A known message this codec cannot read or write yet (module documentation).
    Unsupported(&'static str),
}

impl WireError {
    /// The protocol error the server answers with (ARCHITECTURE 5.9).
    pub fn code(&self) -> ErrorCode {
        match self {
            WireError::FrameTooLarge { .. } => ErrorCode::FrameTooLarge,
            _ => ErrorCode::Protocol,
        }
    }
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::FrameTooLarge { bytes } => {
                write!(f, "frame of {bytes} bytes is over the frame limit")
            }
            WireError::Malformed(why) => write!(f, "malformed JSON: {why}"),
            WireError::TooDeep => write!(f, "JSON nested deeper than {MAX_DEPTH} levels"),
            WireError::DuplicateKey(key) => write!(f, "object repeats the key {key:?}"),
            WireError::NumberOutOfRange => write!(f, "number outside the safe integer range"),
            WireError::NotAnObject => write!(f, "frame is not a JSON object"),
            WireError::UnsupportedVersion => write!(f, "unsupported protocol version"),
            WireError::UnknownMessage(name) => write!(f, "unknown message {name:?}"),
            WireError::MissingKey(key) => write!(f, "missing key {key:?}"),
            WireError::UnknownKey(key) => write!(f, "unknown key {key:?}"),
            WireError::WrongType(key) => write!(f, "key {key:?} has the wrong form"),
            WireError::FieldRange(key) => write!(f, "key {key:?} is out of range"),
            WireError::InvalidId(key, why) => write!(f, "key {key:?}: {why}"),
            WireError::Unsupported(name) => {
                write!(f, "message {name:?} is not supported by this codec yet")
            }
        }
    }
}

impl std::error::Error for WireError {}

/// `text` cut to at most [`MAX_ECHO_BYTES`] bytes on a character boundary, so an error never
/// carries a large part of a hostile frame.
fn clip(text: &str) -> String {
    let mut end = text.len().min(MAX_ECHO_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default().to_owned()
}
