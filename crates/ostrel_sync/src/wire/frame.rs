//! Messages to and from frames: the envelope and the fields of each message.

use ostrel_core::canon::{encode, encode_object, encode_str};
use ostrel_core::ids::{Hlc, InvalidId, OpId, ReplicaId, RowId, ServerSeq};
use ostrel_core::value::INT_MAX;

use super::json::{self, Json, NotSafeInt};
use super::op::{decode_op, encode_op};
use super::query::{decode_query, encode_query};
use super::row::{decode_row, encode_row};
use super::value::read_value;
use super::{Requests, WireError, WireSchema, clip};
use crate::protocol::{
    Ack, CallError, CallId, ClientMsg, DecodedOp, ErrorCode, FnId, Hello, LIMITS, Limits, LoggedOp,
    ModelId, Op, PROTOCOL_VERSION, ProtocolError, RejectReason, ServerMsg, Value, Welcome,
};

/// Reads one frame sent by a client. Models, fields and functions are looked up by name in
/// `schema`.
///
/// A refused `subscribe` or `call` is not fatal ([`WireError::closes_connection`]); the server
/// answers it with [`WireError::reply`].
pub fn decode_client(frame: &[u8], schema: &dyn WireSchema) -> Result<ClientMsg, WireError> {
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
        "ops" => {
            ClientMsg::Ops(fields.req("ops", |k, j| list(k, j, |_, op| decode_op(op, schema)))?)
        }
        "subscribe" => {
            let sub = fields.req("sub", uint32)?;
            let query = fields.req("query", any)?;
            fields.finish()?;
            let query = decode_query(query, schema)
                .map_err(|fault| WireError::InvalidQuery { sub, fault })?;
            return Ok(ClientMsg::Subscribe { sub, query });
        }
        "call" => {
            let call = fields.req("call", uint32)?;
            let function = fields.req("function", any)?;
            let args = fields.req("args", any)?;
            fields.finish()?;
            let (function, args) = call_args(function, args, schema)
                .map_err(|error| WireError::InvalidCall { call, error })?;
            return Ok(ClientMsg::Call {
                call,
                function,
                args,
            });
        }
        other => return Err(WireError::UnknownMessage(clip(other))),
    };
    fields.finish()?;
    Ok(msg)
}

/// Reads one frame sent by the server. Models, fields and functions are looked up by name in
/// `schema`; the model of a `snapshot` and the function of a `return` come from `requests`.
///
/// A `snapshot` or `return` for a request the client no longer has is not fatal
/// ([`WireError::UnknownSubscription`], [`WireError::UnknownCall`]); the client drops it.
pub fn decode_server(
    frame: &[u8],
    schema: &dyn WireSchema,
    requests: &dyn Requests,
) -> Result<ServerMsg, WireError> {
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
            model: fields.req("model", |k, j| model_id(k, j, schema))?,
            rows: fields.req("rows", |k, j| list(k, j, row_id))?,
        },
        "reset" => ServerMsg::Reset {
            sub: fields.req("sub", uint32)?,
        },
        "error" => ServerMsg::Error(ProtocolError {
            sub: fields.opt("sub", uint32)?,
            code: fields.req("code", error_code)?,
        }),
        "ops" => ServerMsg::Ops(fields.req("ops", |k, j| {
            list(k, j, |_, logged| logged_op(logged, schema))
        })?),
        "snapshot" => {
            let sub = fields.req("sub", uint32)?;
            let rows = fields.req("rows", any)?;
            let seq = fields.req("seq", server_seq)?;
            let last = fields.req("last", boolean)?;
            fields.finish()?;
            let model = requests
                .subscription_model(sub)
                .ok_or(WireError::UnknownSubscription(sub))?;
            let rows = list("rows", rows, |_, row| decode_row(row, model, schema))?;
            return Ok(ServerMsg::Snapshot {
                sub,
                rows,
                seq,
                last,
            });
        }
        "return" => {
            let call = fields.req("call", uint32)?;
            let result = fields.req("result", any)?;
            fields.finish()?;
            let result = call_result(call, result, schema, requests)?;
            return Ok(ServerMsg::Return { call, result });
        }
        other => return Err(WireError::UnknownMessage(clip(other))),
    };
    fields.finish()?;
    Ok(msg)
}

/// Writes one client message as a canonical frame. Ids are written as their names in `schema`.
///
/// A [`DecodedOp::Invalid`] in an `ops` batch is [`WireError::InvalidOp`]: it exists only on
/// the receiving side, and no client sends an op it could not encode. An op over the op limit
/// is [`WireError::OpTooLarge`]: the client does not apply such a write and sends no op (D109).
/// How many ops a batch holds is up to the caller; the server answers a batch over
/// [`LIMITS`]`.ops_per_batch` with one `Reject Limit` per op (D106).
pub fn encode_client(msg: &ClientMsg, schema: &dyn WireSchema) -> Result<String, WireError> {
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
        ClientMsg::Ops(ops) => vec![("ops", client_ops(ops, schema)?)],
        ClientMsg::Subscribe { sub, query } => vec![
            ("sub", sub.to_string()),
            ("query", encode_query(query, schema)?),
        ],
        ClientMsg::Call {
            call,
            function,
            args,
        } => vec![
            ("call", call.to_string()),
            (
                "function",
                encode_str(
                    schema
                        .function_name(*function)
                        .ok_or(WireError::NotInSchema("function"))?,
                ),
            ),
            ("args", array(args.iter().map(encode))),
        ],
    };
    close(msg.name(), fields)
}

/// Writes one server message as a canonical frame. Ids are written as their names in
/// `schema`; the model of a `snapshot` comes from `requests`.
///
/// A frame over the frame limit is an error: the server never sends one (D121). The one
/// exception is a `return` whose result does not fit: it is written with `CallError::Limit`
/// instead of the result (D121). The result of a `return` must fit the result type of the
/// function of its call, which comes from `requests`; one that does not is
/// [`WireError::Value`], because the client would refuse it and close the connection. [`super::snapshot_pages`] and [`super::ops_pages`] split
/// rows and ops into frames that fit.
pub fn encode_server(
    msg: &ServerMsg,
    schema: &dyn WireSchema,
    requests: &dyn Requests,
) -> Result<String, WireError> {
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
            ("model", encode_str(model_name(*model, schema)?)),
            ("rows", array(rows.iter().map(|r| hex(&r.to_hex())))),
        ],
        ServerMsg::Reset { sub } => vec![("sub", sub.to_string())],
        ServerMsg::Error(e) => vec![
            ("sub", opt(e.sub, |s| s.to_string())),
            ("code", encode_str(error_code_name(e.code))),
        ],
        ServerMsg::Snapshot {
            sub,
            rows,
            seq,
            last,
        } => {
            let model = requests
                .subscription_model(*sub)
                .ok_or(WireError::UnknownSubscription(*sub))?;
            let rows = rows
                .iter()
                .map(|row| encode_row(row, model, schema))
                .collect::<Result<Vec<_>, _>>()?;
            vec![
                ("sub", sub.to_string()),
                ("rows", array(rows)),
                ("seq", hex(&seq.to_hex())),
                ("last", last.to_string()),
            ]
        }
        ServerMsg::Ops(ops) => vec![("ops", server_ops(ops, schema)?)],
        ServerMsg::Return { call, result } => {
            if let Ok(value) = result {
                check_result(*call, value, schema, requests)?;
            }
            let frame = close(msg.name(), return_fields(*call, result)?);
            return match frame {
                Err(WireError::FrameTooLarge { .. }) if result.is_ok() => {
                    close(msg.name(), return_fields(*call, &Err(CallError::Limit))?)
                }
                other => other,
            };
        }
    };
    close(msg.name(), fields)
}

/// Defines [`call_error_result`] and [`CALL_ERRORS`] from one list, like [`error_codes`]: a
/// new [`CallError`] variant does not compile until it is in the list. The list holds the call
/// errors that travel by name; `Runtime` travels with its kind.
macro_rules! call_errors {
    ($($error:ident),* $(,)?) => {
        /// The key and content of the `result` of a failed call.
        fn call_error_result(error: &CallError) -> (&'static str, String) {
            match error {
                $(CallError::$error => ("err", encode_str(stringify!($error))),)*
                CallError::Runtime { kind } => ("runtime", encode_str(kind)),
            }
        }

        /// Every call error that travels by name, with its name, for the reader.
        const CALL_ERRORS: &[(&str, CallError)] = &[$((stringify!($error), CallError::$error)),*];
    };
}

call_errors![Denied, NotFound, Limit, Invalid, Unavailable];

/// Defines [`error_code_name`] and [`ERROR_CODES`] from one list, so the two cannot drift
/// apart: the `match` is exhaustive, so a new [`ErrorCode`] variant does not compile until it
/// is in the list, and then it is also in the array the reader searches.
macro_rules! error_codes {
    ($($code:ident),* $(,)?) => {
        /// Wire name of an error code: its variant name, like [`RejectReason::name`].
        fn error_code_name(code: ErrorCode) -> &'static str {
            match code {
                $(ErrorCode::$code => stringify!($code),)*
            }
        }

        /// Every error code, for the reader.
        const ERROR_CODES: &[ErrorCode] = &[$(ErrorCode::$code),*];
    };
}

error_codes![
    Protocol,
    FrameTooLarge,
    Unauthorized,
    ReplicaExpired,
    TooManySubscriptions,
    FilterTooLarge,
    FilterNotReadable,
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
    let version = fields.req("v", safe_int)?;
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
pub(super) struct Fields {
    pub(super) entries: Vec<(String, Json)>,
}

impl Fields {
    /// Takes a required key and reads it with `read`.
    pub(super) fn req<T>(
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
    pub(super) fn opt<T>(
        &mut self,
        key: &'static str,
        read: impl FnOnce(&'static str, Json) -> Result<T, WireError>,
    ) -> Result<Option<T>, WireError> {
        self.req(key, |k, j| match j {
            Json::Null => Ok(None),
            other => read(k, other).map(Some),
        })
    }

    /// True when the object still holds `key`.
    pub(super) fn has(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    /// Fails on the first key no field has read.
    pub(super) fn finish(self) -> Result<(), WireError> {
        match self.entries.first() {
            Some((key, _)) => Err(WireError::UnknownKey(clip(key))),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Readers: one per field form. Each gets the key for its error and the JSON node.

pub(super) fn string(key: &'static str, j: Json) -> Result<String, WireError> {
    match j {
        Json::Str(s) => Ok(s),
        _ => Err(WireError::WrongType(key)),
    }
}

pub(super) fn boolean(key: &'static str, j: Json) -> Result<bool, WireError> {
    match j {
        Json::Bool(b) => Ok(b),
        _ => Err(WireError::WrongType(key)),
    }
}

/// An integer of a message field. A number outside the safe range is
/// [`WireError::NumberOutOfRange`] whatever its form (D24); one in range with a fraction or an
/// exponent has the wrong form.
pub(super) fn safe_int(key: &'static str, j: Json) -> Result<i64, WireError> {
    match j {
        Json::Num(n) => n.safe_int().map_err(|why| match why {
            NotSafeInt::OutOfRange => WireError::NumberOutOfRange,
            NotSafeInt::NotAnInteger => WireError::WrongType(key),
        }),
        _ => Err(WireError::WrongType(key)),
    }
}

pub(super) fn uint32(key: &'static str, j: Json) -> Result<u32, WireError> {
    u32::try_from(safe_int(key, j)?).map_err(|_| WireError::FieldRange(key))
}

fn uint64(key: &'static str, j: Json) -> Result<u64, WireError> {
    u64::try_from(safe_int(key, j)?).map_err(|_| WireError::FieldRange(key))
}

pub(super) fn list<T>(
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

pub(super) fn op_id(key: &'static str, j: Json) -> Result<OpId, WireError> {
    hex_id(key, j, OpId::from_hex)
}

pub(super) fn row_id(key: &'static str, j: Json) -> Result<RowId, WireError> {
    hex_id(key, j, RowId::from_hex)
}

pub(super) fn hlc(key: &'static str, j: Json) -> Result<Hlc, WireError> {
    hex_id(key, j, Hlc::from_hex)
}

/// Any JSON, read later by a typed reader.
fn any(_: &'static str, j: Json) -> Result<Json, WireError> {
    Ok(j)
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

/// A model name of a server message. Client and server share one compiled schema, so a name
/// the receiver does not know is a fatal mismatch, unlike an unknown model in a client op.
fn model_id(key: &'static str, j: Json, schema: &dyn WireSchema) -> Result<ModelId, WireError> {
    let name = string(key, j)?;
    schema
        .model_id(&name)
        .ok_or_else(|| WireError::UnknownModel(clip(&name)))
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
        .iter()
        .copied()
        .find(|c| error_code_name(*c) == name)
        .ok_or(WireError::WrongType(key))
}

/// `{"seq", "op"}`: one op of a server `ops` message. An op that does not fit the receiver's
/// schema is fatal: client and server share one compiled schema.
fn logged_op(j: Json, schema: &dyn WireSchema) -> Result<LoggedOp, WireError> {
    let Json::Obj(entries) = j else {
        return Err(WireError::WrongType("ops"));
    };
    let mut f = Fields { entries };
    let seq = f.req("seq", server_seq)?;
    let op = f.req("op", |_, op| decode_op(op, schema))?;
    f.finish()?;
    match op {
        DecodedOp::Valid(op) => Ok(LoggedOp { seq, op }),
        DecodedOp::Invalid { id, fault, .. } => Err(WireError::ServerOp { op: id, fault }),
    }
}

/// The function and arguments of a `call`. An unknown function is `NotFound` (D99); a name
/// that is not a string, arguments that are not an array of the right length, or a value that
/// does not fit its parameter is `Invalid`.
fn call_args(
    function: Json,
    args: Json,
    schema: &dyn WireSchema,
) -> Result<(FnId, Vec<Value>), CallError> {
    let Json::Str(name) = function else {
        return Err(CallError::Invalid);
    };
    let function = schema.function_id(&name).ok_or(CallError::NotFound)?;
    let params = schema
        .function_params(function)
        .ok_or(CallError::NotFound)?;
    let Json::Arr(args) = args else {
        return Err(CallError::Invalid);
    };
    if args.len() != params.len() {
        return Err(CallError::Invalid);
    }
    let args = params
        .iter()
        .zip(args)
        .map(|(ty, arg)| read_value(ty, arg).map_err(|_| CallError::Invalid))
        .collect::<Result<_, _>>()?;
    Ok((function, args))
}

/// `{"ok": value}`, `{"err": name}` or `{"runtime": kind}`. The value has the result type of
/// the function of the call; a value that does not fit is fatal like a server op that does not
/// fit.
fn call_result(
    call: CallId,
    j: Json,
    schema: &dyn WireSchema,
    requests: &dyn Requests,
) -> Result<Result<Value, CallError>, WireError> {
    let function = requests
        .call_function(call)
        .ok_or(WireError::UnknownCall(call))?;
    let Json::Obj(entries) = j else {
        return Err(WireError::WrongType("result"));
    };
    let Ok([(kind, content)]) = <[(String, Json); 1]>::try_from(entries) else {
        return Err(WireError::WrongType("result"));
    };
    match kind.as_str() {
        "ok" => {
            let ty = schema
                .function_result(function)
                .ok_or(WireError::NotInSchema("function"))?;
            read_value(ty, content)
                .map(Ok)
                .map_err(|fault| WireError::Value {
                    key: "result",
                    fault,
                })
        }
        "err" => {
            let name = string("result", content)?;
            CALL_ERRORS
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, error)| Err(error.clone()))
                .ok_or(WireError::WrongType("result"))
        }
        "runtime" => {
            let kind = string("result", content)?;
            if !is_runtime_kind(&kind) {
                return Err(WireError::WrongType("result"));
            }
            Ok(Err(CallError::Runtime { kind }))
        }
        _ => Err(WireError::WrongType("result")),
    }
}

/// A runtime error kind name (ARCHITECTURE 3.4): 1 to [`super::MAX_ECHO_BYTES`] ASCII letters.
/// The codec does not keep the list of kinds, which belongs to the VM; it only keeps the name
/// short and plain.
fn is_runtime_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= super::MAX_ECHO_BYTES
        && kind.bytes().all(|b| b.is_ascii_alphabetic())
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
        row_bytes: f.req("row_bytes", uint32)?,
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

pub(super) fn hex(text: &str) -> String {
    encode_str(text)
}

fn opt<T>(value: Option<T>, write: impl FnOnce(T) -> String) -> String {
    value.map_or_else(|| "null".to_owned(), write)
}

pub(super) fn array(items: impl IntoIterator<Item = String>) -> String {
    let items: Vec<String> = items.into_iter().collect();
    ostrel_core::canon::encode_array(&items)
}

fn model_name(model: ModelId, schema: &dyn WireSchema) -> Result<&str, WireError> {
    schema
        .model_name(model)
        .ok_or(WireError::NotInSchema("model"))
}

/// The ops of a client batch, each as [`encode_op`] writes it and within the op limit.
fn client_ops(ops: &[DecodedOp], schema: &dyn WireSchema) -> Result<String, WireError> {
    let items = ops
        .iter()
        .map(|op| match op {
            DecodedOp::Valid(op) => op_within_limit(op, schema),
            DecodedOp::Invalid { id, .. } => Err(WireError::InvalidOp(*id)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(array(items))
}

/// The ops of a server `ops` message, each as `{"seq", "op"}`.
fn server_ops(ops: &[LoggedOp], schema: &dyn WireSchema) -> Result<String, WireError> {
    let items = ops
        .iter()
        .map(|logged| logged_op_text(logged, schema))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(array(items))
}

/// One element of a server `ops` message.
pub(super) fn logged_op_text(
    logged: &LoggedOp,
    schema: &dyn WireSchema,
) -> Result<String, WireError> {
    encode_object(&[
        ("op", encode_op(&logged.op, schema)?),
        ("seq", hex(&logged.seq.to_hex())),
    ])
    .map_err(|_| WireError::Malformed("repeated field".into()))
}

/// [`encode_op`] for a client op, which must stay within the op limit (D109).
fn op_within_limit(op: &Op, schema: &dyn WireSchema) -> Result<String, WireError> {
    let text = encode_op(op, schema)?;
    if text.len() > LIMITS.op_bytes as usize {
        return Err(WireError::OpTooLarge {
            op: op.id,
            bytes: text.len(),
        });
    }
    Ok(text)
}

/// Checks that the result of a call fits the result type of its function, by reading its
/// encoding back the way [`decode_server`] does. Reading it back, rather than checking the
/// value on its own, keeps writer and reader from ever disagreeing on what fits.
fn check_result(
    call: CallId,
    value: &Value,
    schema: &dyn WireSchema,
    requests: &dyn Requests,
) -> Result<(), WireError> {
    let function = requests
        .call_function(call)
        .ok_or(WireError::UnknownCall(call))?;
    let ty = schema
        .function_result(function)
        .ok_or(WireError::NotInSchema("function"))?;
    let written = json::parse(encode(value).as_bytes())?;
    read_value(ty, written)
        .map(drop)
        .map_err(|fault| WireError::Value {
            key: "result",
            fault,
        })
}

/// The fields of a `return`.
fn return_fields(
    call: CallId,
    result: &Result<Value, CallError>,
) -> Result<Vec<(&'static str, String)>, WireError> {
    let result = match result {
        Ok(value) => ("ok", encode(value)),
        Err(error) => call_error_result(error),
    };
    let result =
        encode_object(&[result]).map_err(|_| WireError::Malformed("repeated field".into()))?;
    Ok(vec![("call", call.to_string()), ("result", result)])
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
        row_bytes,
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
        ("row_bytes", row_bytes),
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
