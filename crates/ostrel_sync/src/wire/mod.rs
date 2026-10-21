//! JSON codec for the frames of the sync protocol (ARCHITECTURE 5.4, D106).
//!
//! [`decode_client`] and [`decode_server`] turn one received WebSocket frame into a message of
//! [`crate::protocol`]; [`encode_client`] and [`encode_server`] write one. Both sides use the
//! same functions: the server decodes client frames and encodes server frames, the client the
//! other way round. All four take the same [`WireSchema`], because models and fields travel by
//! name while the messages in memory hold [`ModelId`] and [`FieldId`] (D135).
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
//! 2. JSON: well formed UTF-8 JSON, no repeated key, at most [`MAX_DEPTH`] levels deep
//!    ([`json`]);
//! 3. envelope: an object with version 0 and a known message name;
//! 4. fields: every field present with the right form, no other key; every integer field in
//!    the safe range of D24.
//!
//! The codec checks form only. Rules that need the session or the app, such as a
//! `replica_secret` in an app with `auth` (D122), belong to the server.
//!
//! # Typed values and ops (D132)
//!
//! The JSON form of a value does not name its type, so values are read against the
//! [`WireTy`] of their place, which a [`WireSchema`] supplies; after the codec only
//! [`Value`](crate::protocol::Value) exists. Models and fields travel by name. [`decode_op`] and
//! [`encode_op`] read and write one op in the form of ARCHITECTURE 5.4, and [`op_bytes`] measures
//! it for the limit of D109.
//!
//! A client `ops` frame is `{"ops": [op, ...]}`. An op that is well formed but does not fit the
//! schema becomes a [`DecodedOp::Invalid`] of [`crate::protocol`] and leaves the rest of the
//! batch alone; `evict` names its model. The fault types live in [`crate::protocol`], because
//! the server reads them in step 6 of ARCHITECTURE 5.5; this module only builds them (D135).
//!
//! # Queries, rows, server ops and calls
//!
//! `subscribe` carries a query ([`query`]), `snapshot` carries rows ([`row`]), the server's
//! `ops` carry each op with its log position, and `call` and `return` carry values typed by the
//! signature of the function ([`WireSchema::function_params`], [`WireSchema::function_result`]).
//!
//! Two of these messages name no model or function on the wire: the rows of a `snapshot`
//! belong to the model of its subscription, and the value of a `return` has the result type of
//! the function of its call. The side that encodes or decodes them supplies what it knows about
//! its own requests through [`Requests`].
//!
//! # What a fault closes
//!
//! Most faults of a received frame are fatal ([`WireError::closes_connection`]). Four are not,
//! because they concern one request: a `subscribe` whose query does not fit
//! ([`WireError::InvalidQuery`], answered by `error` with the subscription id), a `call` whose
//! function or arguments do not fit ([`WireError::InvalidCall`], answered by `return` with the
//! call error), and on the client a `snapshot` or `return` for a subscription or call it no
//! longer has ([`WireError::UnknownSubscription`], [`WireError::UnknownCall`], dropped).
//! [`WireError::reply`] gives the server's answer to a client frame that failed.
//!
//! # Limits
//!
//! [`encode_client`] refuses an op over the op limit (D109), so no client sends one.
//! [`encode_server`] answers a `return` that would not fit one frame with `CallError::Limit`
//! instead of the result (D121). [`snapshot_pages`] and [`ops_pages`] split rows and server ops
//! into frames that fit (D121), and [`row_bytes`] measures a row for the row limit.

pub mod json;

pub mod query;
pub mod row;

mod frame;
mod op;
mod paging;
mod value;

use std::fmt;

use ostrel_core::ids::InvalidId;

use crate::protocol::{
    CallError, CallId, ErrorCode, FieldId, FnId, ModelId, OpFault, OpId, ProtocolError, RowId,
    ServerMsg, SubId, ValueFault,
};

pub use frame::{decode_client, decode_server, encode_client, encode_server};
pub use json::MAX_DEPTH;
pub use op::{decode_op, encode_op, op_bytes};
pub use paging::{ops_pages, snapshot_pages};
pub use query::QueryFault;
pub use row::row_bytes;
pub use value::{WireTy, read_value};

#[cfg(doc)]
use crate::protocol::{DecodedOp, PROTOCOL_VERSION};

/// The names and wire types of the compiled schema, as the codec needs them (D132).
///
/// The server implements it from the compiled program (T4); tests build one by hand. Ids are
/// positions in that program and never travel; names do. The field lookup covers the implicit
/// fields `made`, `changed` and `author` too, because a `snapshot` row carries them by name;
/// whether an op may write them is decided by the server (D116).
pub trait WireSchema {
    /// The model with this name.
    fn model_id(&self, name: &str) -> Option<ModelId>;
    /// The name of a model, for writing.
    fn model_name(&self, model: ModelId) -> Option<&str>;
    /// The declared field of `model` with this name.
    fn field_id(&self, model: ModelId, name: &str) -> Option<FieldId>;
    /// The name of a field, for writing.
    fn field_name(&self, model: ModelId, field: FieldId) -> Option<&str>;
    /// The wire type of a field.
    fn field_ty(&self, model: ModelId, field: FieldId) -> Option<&WireTy>;
    /// The server function with this name.
    fn function_id(&self, name: &str) -> Option<FnId>;
    /// The name of a server function, for writing.
    fn function_name(&self, function: FnId) -> Option<&str>;
    /// The wire types of the parameters of a server function, in declaration order. A model
    /// parameter is a [`WireTy::Ref`] to that model.
    fn function_params(&self, function: FnId) -> Option<&[WireTy]>;
    /// The wire type of the result of a server function.
    fn function_result(&self, function: FnId) -> Option<&WireTy>;
}

/// What one side of a session knows about its own open requests (module documentation).
///
/// The client implements it from its subscriptions and the calls it sent and has not settled;
/// the server from the subscriptions of the session and the calls it has not answered yet,
/// because [`encode_server`] checks the result of a `return` against its function.
pub trait Requests {
    /// The model of an open subscription.
    fn subscription_model(&self, sub: SubId) -> Option<ModelId>;
    /// The function of a call that waits for its `return`.
    fn call_function(&self, call: CallId) -> Option<FnId>;
}

/// Longest key or name a [`WireError`] repeats from a received frame, in bytes.
pub const MAX_ECHO_BYTES: usize = 64;

/// Why a frame could not be read or written. Every kind except [`WireError::FrameTooLarge`]
/// and a query over the filter limit has the code [`ErrorCode::Protocol`]
/// ([`WireError::code`]); four kinds keep the connection open
/// ([`WireError::closes_connection`]).
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
    /// A number in an integer field of a message outside the safe range
    /// `-(2^53 - 1)..=2^53 - 1` (D24), whatever its form (`2^64`, `1e20`). Numbers are checked
    /// where their type is known, never as bare tokens, so a canonical `Float` such as `2^53`
    /// still reads ([`json`]).
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
    /// A server message names a model the receiver's schema does not have (cut to
    /// [`MAX_ECHO_BYTES`]). Client and server share one compiled schema, so this is a fatal
    /// mismatch; an unknown model in a client op only makes that op invalid.
    UnknownModel(String),
    /// Writing: the schema does not know this kind of id (`"model"`, `"field"` or
    /// `"function"`). A defect of the caller, never caused by a peer.
    NotInSchema(&'static str),
    /// Writing: a [`DecodedOp::Invalid`] with this op id. Such an op exists only on the
    /// receiving side and is never sent (D135). A defect of the caller.
    InvalidOp(OpId),
    /// Writing: a client op whose canonical encoding is over the op limit (D109). The client
    /// does not apply such a write and sends no op; a defect of the caller.
    OpTooLarge { op: OpId, bytes: usize },
    /// Writing: a row whose canonical encoding is over the row limit (D121). The server never
    /// stores one; a defect of the caller.
    RowTooLarge { row: RowId, bytes: usize },
    /// A server message names a field the receiver's schema does not have (cut to
    /// [`MAX_ECHO_BYTES`]); fatal like [`WireError::UnknownModel`].
    UnknownField(String),
    /// A value of a server message does not fit its type in the receiver's schema; fatal like
    /// [`WireError::UnknownModel`]. On writing the result of a `return`, a defect of the
    /// caller.
    Value {
        key: &'static str,
        fault: ValueFault,
    },
    /// An op of a server `ops` message does not fit the receiver's schema; fatal like
    /// [`WireError::UnknownModel`].
    ServerOp { op: OpId, fault: OpFault },
    /// Not fatal: the query of this `subscribe` does not fit; the server answers `error` with
    /// the subscription id and keeps the connection (D106).
    InvalidQuery { sub: SubId, fault: QueryFault },
    /// Not fatal: the function or the arguments of this `call` do not fit; the server answers
    /// `return` with this error and keeps the connection. An unknown function is
    /// [`CallError::NotFound`] (D99), anything else [`CallError::Invalid`].
    InvalidCall { call: CallId, error: CallError },
    /// Not fatal: a `snapshot` for a subscription the receiver does not have, for example one
    /// it just ended. The client drops it. On writing, a defect of the caller.
    UnknownSubscription(SubId),
    /// Not fatal: a `return` for a call that is not waiting, for example one that already
    /// failed with `OutcomeUnknown`; the client drops it (ARCHITECTURE 5.7). On writing a
    /// successful `return`, a defect of the caller.
    UnknownCall(CallId),
}

impl WireError {
    /// The code of the `error` the server answers with (ARCHITECTURE 5.9). A refused call is
    /// answered by `return` instead ([`WireError::reply`]).
    pub fn code(&self) -> ErrorCode {
        match self {
            WireError::FrameTooLarge { .. } => ErrorCode::FrameTooLarge,
            WireError::InvalidQuery {
                fault: QueryFault::TooManyNodes,
                ..
            } => ErrorCode::FilterTooLarge,
            _ => ErrorCode::Protocol,
        }
    }

    /// False for the faults that concern one request only (module documentation).
    pub fn closes_connection(&self) -> bool {
        !matches!(
            self,
            WireError::InvalidQuery { .. }
                | WireError::InvalidCall { .. }
                | WireError::UnknownSubscription(_)
                | WireError::UnknownCall(_)
        )
    }

    /// The server's answer to a client frame that [`decode_client`] refused: `error` with the
    /// subscription id for a refused query, `return` with the call error for a refused call,
    /// and a fatal `error` for everything else, after which the server closes the connection.
    pub fn reply(&self) -> ServerMsg {
        match self {
            WireError::InvalidCall { call, error } => ServerMsg::Return {
                call: *call,
                result: Err(error.clone()),
            },
            WireError::InvalidQuery { sub, .. } => ServerMsg::Error(ProtocolError {
                sub: Some(*sub),
                code: self.code(),
            }),
            _ => ServerMsg::Error(ProtocolError {
                sub: None,
                code: self.code(),
            }),
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
            WireError::UnknownModel(name) => write!(f, "unknown model {name:?}"),
            WireError::NotInSchema(kind) => write!(f, "{kind} id is not in the schema"),
            WireError::InvalidOp(id) => {
                write!(f, "op {} is invalid and cannot be sent", id.to_hex())
            }
            WireError::OpTooLarge { op, bytes } => {
                write!(
                    f,
                    "op {} of {bytes} bytes is over the op limit",
                    op.to_hex()
                )
            }
            WireError::RowTooLarge { row, bytes } => {
                write!(
                    f,
                    "row {} of {bytes} bytes is over the row limit",
                    row.to_hex()
                )
            }
            WireError::UnknownField(name) => write!(f, "unknown field {name:?}"),
            WireError::Value { key, fault } => write!(f, "key {key:?}: {fault}"),
            WireError::ServerOp { op, fault } => {
                write!(
                    f,
                    "server op {} does not fit the schema: {fault:?}",
                    op.to_hex()
                )
            }
            WireError::InvalidQuery { sub, fault } => {
                write!(f, "query of subscription {sub} is invalid: {fault}")
            }
            WireError::InvalidCall { call, error } => {
                write!(f, "call {call} is refused: {error:?}")
            }
            WireError::UnknownSubscription(sub) => write!(f, "no subscription {sub}"),
            WireError::UnknownCall(call) => write!(f, "no call {call} is waiting"),
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
