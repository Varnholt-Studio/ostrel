//! Messages to and from frames: the envelope and the fields of each message.

use ostrel_core::canon::{encode_object, encode_str};
use ostrel_core::ids::{Hlc, InvalidId, OpId, ReplicaId, RowId, ServerSeq};
use ostrel_core::value::INT_MAX;

use super::json::{self, Json};
use super::{WireError, clip};
use crate::protocol::{
    Ack, ClientMsg, ErrorCode, Hello, LIMITS, Limits, PROTOCOL_VERSION, ProtocolError,
    RejectReason, ServerMsg, Welcome,
};

/// Reads one frame sent by a client.
pub fn decode_client(frame: &[u8]) -> Result<ClientMsg, WireError> {
    let (name, mut fields) = open(frame)?;
    let msg = match name.as_str() {
        "hello" => ClientMsg::Hello(Hello {
            token: fields.opt("token", string)?,
            replica: fields.opt("replica", replica_id)?,
            replica_secret: fields.opt("replica_secret", string)?,
            last_server_seq: fields.opt("last_server_seq", server_seq)?,
        }),
        "unsubscribe" => ClientMsg::Unsubscribe {
            sub: fields.req("sub", uint32)?,
        },
        "held" => ClientMsg::Held {
            sub: fields.req("sub", uint32)?,
            rows: fields.req("rows", |k, j| list(k, j, row_id))?,
            last: fields.req("last", boolean)?,
        },
        "subscribe" => return Err(WireError::Unsupported("subscribe")),
        "ops" => return Err(WireError::Unsupported("ops")),
        "call" => return Err(WireError::Unsupported("call")),
        other => return Err(WireError::UnknownMessage(clip(other))),
    };
    fields.finish()?;
    Ok(msg)
}

/// Reads one frame sent by the server.
pub fn decode_server(frame: &[u8]) -> Result<ServerMsg, WireError> {
    let (name, mut fields) = open(frame)?;
    let msg = match name.as_str() {
        "welcome" => ServerMsg::Welcome(Welcome {
            replica: fields.req("replica", replica_id)?,
            replica_secret: fields.opt("replica_secret", string)?,
            server_ms: fields.req("server_ms", uint64)?,
            limits: fields.req("limits", limits)?,
        }),
        "ack" => ServerMsg::Ack(Ack {
            op: fields.req("op", op_id)?,
            seq: fields.req("seq", server_seq)?,
            restamped: fields.req("restamped", |k, j| list(k, j, restamp))?,
        }),
        "reject" => ServerMsg::Reject {
            op: fields.req("op", op_id)?,
            reason: fields.req("reason", reject_reason)?,
        },
        "defer" => ServerMsg::Defer {
            op: fields.req("op", op_id)?,
            retry_after_ms: fields.req("retry_after_ms", uint32)?,
        },
        "evict" => ServerMsg::Evict {
            sub: fields.req("sub", uint32)?,
            model: fields.req("model", uint32)?,
            rows: fields.req("rows", |k, j| list(k, j, row_id))?,
        },
        "reset" => ServerMsg::Reset {
            sub: fields.req("sub", uint32)?,
        },
        "error" => ServerMsg::Error(ProtocolError {
            sub: fields.opt("sub", uint32)?,
            code: fields.req("code", error_code)?,
        }),
        "snapshot" => return Err(WireError::Unsupported("snapshot")),
        "ops" => return Err(WireError::Unsupported("ops")),
        "return" => return Err(WireError::Unsupported("return")),
        other => return Err(WireError::UnknownMessage(clip(other))),
    };
    fields.finish()?;
    Ok(msg)
}

/// Writes one client message as a canonical frame.
pub fn encode_client(msg: &ClientMsg) -> Result<String, WireError> {
    let fields = match msg {
        ClientMsg::Hello(h) => vec![
            ("token", opt(h.token.as_deref(), encode_str)),
            ("replica", opt(h.replica, |r| hex(&r.to_hex()))),
            (
                "replica_secret",
                opt(h.replica_secret.as_deref(), encode_str),
            ),
            (
                "last_server_seq",
                opt(h.last_server_seq, |s| hex(&s.to_hex())),
            ),
        ],
        ClientMsg::Unsubscribe { sub } => vec![("sub", sub.to_string())],
        ClientMsg::Held { sub, rows, last } => vec![
            ("sub", sub.to_string()),
            ("rows", array(rows.iter().map(|r| hex(&r.to_hex())))),
            ("last", last.to_string()),
        ],
        ClientMsg::Subscribe { .. } => return Err(WireError::Unsupported("subscribe")),
        ClientMsg::Ops(_) => return Err(WireError::Unsupported("ops")),
        ClientMsg::Call { .. } => return Err(WireError::Unsupported("call")),
    };
    close(msg.name(), fields)
}

/// Writes one server message as a canonical frame. A frame over the frame limit is an error:
/// the server never sends one (D121).
pub fn encode_server(msg: &ServerMsg) -> Result<String, WireError> {
    let fields = match msg {
        ServerMsg::Welcome(w) => vec![
            ("replica", hex(&w.replica.to_hex())),
            (
                "replica_secret",
                opt(w.replica_secret.as_deref(), encode_str),
            ),
            ("server_ms", safe_uint64("server_ms", w.server_ms)?),
            ("limits", limits_object(&w.limits)?),
        ],
        ServerMsg::Ack(a) => vec![
            ("op", hex(&a.op.to_hex())),
            ("seq", hex(&a.seq.to_hex())),
            (
                "restamped",
                array(
                    a.restamped
                        .iter()
                        .map(|(op, hlc)| array([hex(&op.to_hex()), hex(&hlc.to_hex())])),
                ),
            ),
        ],
        ServerMsg::Reject { op, reason } => vec![
            ("op", hex(&op.to_hex())),
            ("reason", encode_str(reason.name())),
        ],
        ServerMsg::Defer { op, retry_after_ms } => vec![
            ("op", hex(&op.to_hex())),
            ("retry_after_ms", retry_after_ms.to_string()),
        ],
        ServerMsg::Evict { sub, model, rows } => vec![
            ("sub", sub.to_string()),
            ("model", model.to_string()),
            ("rows", array(rows.iter().map(|r| hex(&r.to_hex())))),
        ],
        ServerMsg::Reset { sub } => vec![("sub", sub.to_string())],
        ServerMsg::Error(e) => vec![
            ("sub", opt(e.sub, |s| s.to_string())),
            ("code", encode_str(error_code_name(e.code))),
        ],
        ServerMsg::Snapshot { .. } => return Err(WireError::Unsupported("snapshot")),
        ServerMsg::Ops(_) => return Err(WireError::Unsupported("ops")),
        ServerMsg::Return { .. } => return Err(WireError::Unsupported("return")),
    };
    close(msg.name(), fields)
}

/// Wire name of an error code: its variant name, like [`RejectReason::name`].
fn error_code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Protocol => "Protocol",
        ErrorCode::FrameTooLarge => "FrameTooLarge",
        ErrorCode::Unauthorized => "Unauthorized",
        ErrorCode::ReplicaExpired => "ReplicaExpired",
        ErrorCode::TooManySubscriptions => "TooManySubscriptions",
        ErrorCode::FilterTooLarge => "FilterTooLarge",
        ErrorCode::FilterNotReadable => "FilterNotReadable",
    }
}

const ERROR_CODES: [ErrorCode; 7] = [
    ErrorCode::Protocol,
    ErrorCode::FrameTooLarge,
    ErrorCode::Unauthorized,
    ErrorCode::ReplicaExpired,
    ErrorCode::TooManySubscriptions,
    ErrorCode::FilterTooLarge,
    ErrorCode::FilterNotReadable,
];

// ---------------------------------------------------------------------------------------------
// Envelope

/// Checks size, JSON and envelope of a received frame and returns the message name and the
/// remaining fields.
fn open(frame: &[u8]) -> Result<(String, Fields), WireError> {
    if frame.len() > LIMITS.frame_bytes as usize {
        return Err(WireError::FrameTooLarge { bytes: frame.len() });
    }
    let Json::Obj(entries) = json::parse(frame)? else {
        return Err(WireError::NotAnObject);
    };
    let mut fields = Fields { entries };
    // The version comes first: a frame of another version may name messages we do not know.
    let version = fields.req("v", |k, j| match j {
        Json::Int(v) => Ok(v),
        _ => Err(WireError::WrongType(k)),
    })?;
    if version != i64::from(PROTOCOL_VERSION) {
        return Err(WireError::UnsupportedVersion);
    }
    let name = fields.req("t", string)?;
    Ok((name, fields))
}

/// Adds the envelope to the encoded fields and checks the frame limit.
fn close(name: &str, mut fields: Vec<(&str, String)>) -> Result<String, WireError> {
    fields.push(("v", PROTOCOL_VERSION.to_string()));
    fields.push(("t", encode_str(name)));
    // Field names are fixed per message and never repeat, so this cannot fail; the error is
    // still mapped instead of unwrapped.
    let frame =
        encode_object(&fields).map_err(|_| WireError::Malformed("repeated field".into()))?;
    if frame.len() > LIMITS.frame_bytes as usize {
        return Err(WireError::FrameTooLarge { bytes: frame.len() });
    }
    Ok(frame)
}

/// The keys of one received object that have not been read yet.
struct Fields {
    entries: Vec<(String, Json)>,
}

impl Fields {
    /// Takes a required key and reads it with `read`.
    fn req<T>(
        &mut self,
        key: &'static str,
        read: impl FnOnce(&'static str, Json) -> Result<T, WireError>,
    ) -> Result<T, WireError> {
        let at = self
            .entries
            .iter()
            .position(|(k, _)| k == key)
            .ok_or(WireError::MissingKey(key))?;
        let (_, value) = self.entries.swap_remove(at);
        read(key, value)
    }

    /// Takes a required key whose value may be `null`.
    fn opt<T>(
        &mut self,
        key: &'static str,
        read: impl FnOnce(&'static str, Json) -> Result<T, WireError>,
    ) -> Result<Option<T>, WireError> {
        self.req(key, |k, j| match j {
            Json::Null => Ok(None),
            other => read(k, other).map(Some),
        })
    }

    /// Fails on the first key no field has read.
    fn finish(self) -> Result<(), WireError> {
        match self.entries.first() {
            Some((key, _)) => Err(WireError::UnknownKey(clip(key))),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Readers: one per field form. Each gets the key for its error and the JSON node.

fn string(key: &'static str, j: Json) -> Result<String, WireError> {
    match j {
        Json::Str(s) => Ok(s),
        _ => Err(WireError::WrongType(key)),
    }
}

fn boolean(key: &'static str, j: Json) -> Result<bool, WireError> {
    match j {
        Json::Bool(b) => Ok(b),
        _ => Err(WireError::WrongType(key)),
    }
}

fn uint32(key: &'static str, j: Json) -> Result<u32, WireError> {
    match j {
        Json::Int(v) => u32::try_from(v).map_err(|_| WireError::FieldRange(key)),
        _ => Err(WireError::WrongType(key)),
    }
}

fn uint64(key: &'static str, j: Json) -> Result<u64, WireError> {
    match j {
        Json::Int(v) => u64::try_from(v).map_err(|_| WireError::FieldRange(key)),
        _ => Err(WireError::WrongType(key)),
    }
}

fn list<T>(
    key: &'static str,
    j: Json,
    item: impl Fn(&'static str, Json) -> Result<T, WireError>,
) -> Result<Vec<T>, WireError> {
    match j {
        Json::Arr(items) => items.into_iter().map(|i| item(key, i)).collect(),
        _ => Err(WireError::WrongType(key)),
    }
}

/// Reads a hex string with the strict parser of its id type.
fn hex_id<T>(
    key: &'static str,
    j: Json,
    parse: fn(&str) -> Result<T, InvalidId>,
) -> Result<T, WireError> {
    let text = string(key, j)?;
    parse(&text).map_err(|why| WireError::InvalidId(key, why))
}

fn replica_id(key: &'static str, j: Json) -> Result<ReplicaId, WireError> {
    hex_id(key, j, ReplicaId::from_hex)
}

fn server_seq(key: &'static str, j: Json) -> Result<ServerSeq, WireError> {
    hex_id(key, j, ServerSeq::from_hex)
}

fn op_id(key: &'static str, j: Json) -> Result<OpId, WireError> {
    hex_id(key, j, OpId::from_hex)
}

fn row_id(key: &'static str, j: Json) -> Result<RowId, WireError> {
    hex_id(key, j, RowId::from_hex)
}

fn hlc(key: &'static str, j: Json) -> Result<Hlc, WireError> {
    hex_id(key, j, Hlc::from_hex)
}

/// `[op, hlc]`.
fn restamp(key: &'static str, j: Json) -> Result<(OpId, Hlc), WireError> {
    match j {
        Json::Arr(pair) => match <[Json; 2]>::try_from(pair) {
            Ok([op, stamp]) => Ok((op_id(key, op)?, hlc(key, stamp)?)),
            Err(_) => Err(WireError::WrongType(key)),
        },
        _ => Err(WireError::WrongType(key)),
    }
}

fn reject_reason(key: &'static str, j: Json) -> Result<RejectReason, WireError> {
    let name = string(key, j)?;
    RejectReason::ALL
        .into_iter()
        .find(|r| r.name() == name)
        .ok_or(WireError::WrongType(key))
}

fn error_code(key: &'static str, j: Json) -> Result<ErrorCode, WireError> {
    let name = string(key, j)?;
    ERROR_CODES
        .into_iter()
        .find(|c| error_code_name(*c) == name)
        .ok_or(WireError::WrongType(key))
}

fn limits(key: &'static str, j: Json) -> Result<Limits, WireError> {
    let Json::Obj(entries) = j else {
        return Err(WireError::WrongType(key));
    };
    let mut f = Fields { entries };
    let limits = Limits {
        frame_bytes: f.req("frame_bytes", uint32)?,
        http_body_bytes: f.req("http_body_bytes", uint32)?,
        ops_per_batch: f.req("ops_per_batch", uint32)?,
        ops_per_second: f.req("ops_per_second", uint32)?,
        ops_burst: f.req("ops_burst", uint32)?,
        rows_per_hour: f.req("rows_per_hour", uint32)?,
        op_bytes: f.req("op_bytes", uint32)?,
        nesting_depth: f.req("nesting_depth", uint32)?,
        text_bytes: f.req("text_bytes", uint32)?,
        subscriptions_per_session: f.req("subscriptions_per_session", uint32)?,
        filter_nodes: f.req("filter_nodes", uint32)?,
        rows_per_snapshot: f.req("rows_per_snapshot", uint32)?,
        ids_per_page: f.req("ids_per_page", uint32)?,
        held_per_subscription: f.req("held_per_subscription", uint32)?,
        outbox_before_ack: f.req("outbox_before_ack", uint32)?,
        calls_per_second: f.req("calls_per_second", uint32)?,
        calls_burst: f.req("calls_burst", uint32)?,
        call_timeout_ms: f.req("call_timeout_ms", uint32)?,
        bulk_rows: f.req("bulk_rows", uint32)?,
        bulk_attempts: f.req("bulk_attempts", uint32)?,
        clock_tolerance_ms: f.req("clock_tolerance_ms", uint32)?,
        tags_per_remove: f.req("tags_per_remove", uint32)?,
        rank_digits: f.req("rank_digits", uint32)?,
    };
    f.finish()?;
    Ok(limits)
}

// ---------------------------------------------------------------------------------------------
// Writers: each returns the canonical JSON text of one field value.

fn hex(text: &str) -> String {
    encode_str(text)
}

fn opt<T>(value: Option<T>, write: impl FnOnce(T) -> String) -> String {
    value.map_or_else(|| "null".to_owned(), write)
}

fn array(items: impl IntoIterator<Item = String>) -> String {
    let items: Vec<String> = items.into_iter().collect();
    ostrel_core::canon::encode_array(&items)
}

/// A `u64` as a JSON integer; above the safe range it cannot travel (D24).
fn safe_uint64(key: &'static str, v: u64) -> Result<String, WireError> {
    if v > INT_MAX.unsigned_abs() {
        return Err(WireError::FieldRange(key));
    }
    Ok(v.to_string())
}

fn limits_object(l: &Limits) -> Result<String, WireError> {
    // Destructured without `..`, so a new limit does not compile until it is on the wire.
    let Limits {
        frame_bytes,
        http_body_bytes,
        ops_per_batch,
        ops_per_second,
        ops_burst,
        rows_per_hour,
        op_bytes,
        nesting_depth,
        text_bytes,
        subscriptions_per_session,
        filter_nodes,
        rows_per_snapshot,
        ids_per_page,
        held_per_subscription,
        outbox_before_ack,
        calls_per_second,
        calls_burst,
        call_timeout_ms,
        bulk_rows,
        bulk_attempts,
        clock_tolerance_ms,
        tags_per_remove,
        rank_digits,
    } = *l;
    let fields = [
        ("frame_bytes", frame_bytes),
        ("http_body_bytes", http_body_bytes),
        ("ops_per_batch", ops_per_batch),
        ("ops_per_second", ops_per_second),
        ("ops_burst", ops_burst),
        ("rows_per_hour", rows_per_hour),
        ("op_bytes", op_bytes),
        ("nesting_depth", nesting_depth),
        ("text_bytes", text_bytes),
        ("subscriptions_per_session", subscriptions_per_session),
        ("filter_nodes", filter_nodes),
        ("rows_per_snapshot", rows_per_snapshot),
        ("ids_per_page", ids_per_page),
        ("held_per_subscription", held_per_subscription),
        ("outbox_before_ack", outbox_before_ack),
        ("calls_per_second", calls_per_second),
        ("calls_burst", calls_burst),
        ("call_timeout_ms", call_timeout_ms),
        ("bulk_rows", bulk_rows),
        ("bulk_attempts", bulk_attempts),
        ("clock_tolerance_ms", clock_tolerance_ms),
        ("tags_per_remove", tags_per_remove),
        ("rank_digits", rank_digits),
    ];
    let encoded: Vec<(&str, String)> = fields
        .iter()
        .map(|(name, value)| (*name, value.to_string()))
        .collect();
    encode_object(&encoded).map_err(|_| WireError::Malformed("repeated limit".into()))
}
