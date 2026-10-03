//! Sync protocol v0: messages, op bodies, reject reasons and limits (ARCHITECTURE 5.4, 5.5, 5.9).
//!
//! Draft until the RED re-check of ARCHITECTURE 5.3 to 5.6; after that it is a frozen contract
//! (ARCHITECTURE 13) and changes only by an architect decision.
//!
//! # Wire form
//!
//! One WebSocket per browser carries JSON frames, one message per frame. Every frame is an
//! object with the protocol version `"v"` ([`PROTOCOL_VERSION`]) and the message name `"t"`
//! ([`ClientMsg::name`], [`ServerMsg::name`]); the other keys are the fields of the message in
//! snake case. Ids, [`Hlc`] and [`ServerSeq`] travel as fixed width lowercase hex strings,
//! values as in the type table (ARCHITECTURE 5.1, 5.3). A frame with an unknown version, an
//! unknown name, a missing or extra key is a fatal [`ErrorCode::Protocol`].
//!
//! # Replicas (D103)
//!
//! A [`Hello`] without a replica always gets a new random [`ReplicaId`] in [`Welcome`], also
//! when the pair (user, device key) already owns replicas. A [`Hello`] with a replica is
//! confirmed only when the token's user and device key match the record of that replica.

pub use ostrel_core::ids::{Hlc, OpId, ReplicaId, RowId, ServerSeq};
pub use ostrel_core::value::Value;
pub use ostrel_db::api::{FieldId, ModelId, Query, Row};

/// Version carried in every frame. JSON v0.
pub const PROTOCOL_VERSION: u32 = 0;

/// Subscription id, chosen by the client, unique within one session.
pub type SubId = u32;

/// Server function call id, chosen by the client, unique within one session.
pub type CallId = u32;

/// Index of a server function in the compiled program.
pub type FnId = u32;

/// Messages from client to server.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientMsg {
    Hello(Hello),
    /// Start a subscription. The server accepts a filter and sort only over client readable
    /// fields; every reference hop carries the `see` rule of its target model as an `AND`
    /// (D101). Otherwise, and beyond [`Limits::subscriptions_per_session`] or
    /// [`Limits::filter_nodes`], the answer is an [`ServerMsg::Error`] for this subscription.
    Subscribe {
        sub: SubId,
        query: Query,
    },
    Unsubscribe {
        sub: SubId,
    },
    /// One page of the row ids the client holds for a resubscribed subscription, at most
    /// [`Limits::ids_per_page`]. `last` marks the final page.
    Held {
        sub: SubId,
        rows: Vec<RowId>,
        last: bool,
    },
    /// A batch of local ops, at most [`Limits::ops_per_batch`], in `seq` order of the replica.
    Ops(Vec<Op>),
    Call {
        call: CallId,
        function: FnId,
        args: Vec<Value>,
    },
}

/// Messages from server to client.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerMsg {
    Welcome(Welcome),
    /// One page of rows of a subscription, at most [`Limits::rows_per_snapshot`], at log
    /// position `seq`. `last` marks the final page. A snapshot page sets the client's
    /// `applied_server_seq` to `seq` (D62).
    Snapshot {
        sub: SubId,
        rows: Vec<Row>,
        seq: ServerSeq,
        last: bool,
    },
    /// Ops of other replicas, in non decreasing [`ServerSeq`] within one session (D62).
    Ops(Vec<LoggedOp>),
    Ack(Ack),
    Reject {
        op: OpId,
        reason: RejectReason,
    },
    /// One page of row ids, at most [`Limits::ids_per_page`], that left the reader's scope or
    /// were deleted.
    Evict {
        sub: SubId,
        model: ModelId,
        rows: Vec<RowId>,
    },
    /// The client drops the rows of the subscription and subscribes again.
    Reset {
        sub: SubId,
    },
    Return {
        call: CallId,
        result: Result<Value, CallError>,
    },
    Error(ProtocolError),
}

/// First message of every connection.
#[derive(Clone, Debug, PartialEq)]
pub struct Hello {
    pub version: u32,
    /// PASETO v4.public token; `None` only in apps without `auth`.
    pub token: Option<String>,
    /// The replica the client stored next to its device key; `None` asks for a new one (D103).
    pub replica: Option<ReplicaId>,
    /// Highest log position the client applied; `None` on a fresh replica.
    pub last_server_seq: Option<ServerSeq>,
}

/// Answer to [`Hello`].
#[derive(Clone, Debug, PartialEq)]
pub struct Welcome {
    /// The confirmed or newly issued replica.
    pub replica: ReplicaId,
    /// Server time in ms since the epoch, for the client's clock offset.
    pub server_ms: u64,
    pub limits: Limits,
}

/// One op: a change of exactly one row, field, element or key (G12).
#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub id: OpId,
    /// Client stamp; the server may re-stamp it (ARCHITECTURE 5.5 step 3).
    pub hlc: Hlc,
    pub model: ModelId,
    pub row: RowId,
    pub body: OpBody,
}

/// An op as the server log holds and broadcasts it, with its accepted stamp.
#[derive(Clone, Debug, PartialEq)]
pub struct LoggedOp {
    pub seq: ServerSeq,
    pub op: Op,
}

/// What an op does. The bodies of `Set`, `Map` and `Rank` fields follow the shared vectors in
/// `tests/crdt-vectors` (D49, G6, G12).
#[derive(Clone, Debug, PartialEq)]
pub enum OpBody {
    /// Create the row with its initial column values. The row id carries the op's replica,
    /// otherwise `Forged`; an id that exists or existed in any model is `Conflict`.
    /// ASSUMPTION: one `make` op carries all initial column values, because the `make` rule
    /// needs the whole row.
    Make { fields: Vec<(FieldId, Value)> },
    /// Delete the row.
    Drop,
    /// Write one column field: last writer wins, also for `Rank` keys.
    Set { field: FieldId, value: Value },
    /// Add one element to a `Set` field; the op id is its tag.
    SetAdd { field: FieldId, elem: Value },
    /// Remove one element of a `Set` field by the add tags the replica observed, 1 to
    /// [`Limits::tags_per_remove`]; only tags of the same row, field and element count (D96).
    SetRemove {
        field: FieldId,
        elem: Value,
        tags: Vec<OpId>,
    },
    /// Set one key of a `Map` field.
    MapPut {
        field: FieldId,
        key: Value,
        value: Value,
    },
    /// Remove one key of a `Map` field.
    MapRemove { field: FieldId, key: Value },
}

/// Acknowledgement of one client [`ClientMsg::Ops`] batch.
#[derive(Clone, Debug, PartialEq)]
pub struct Ack {
    /// Highest accepted op of the batch.
    pub op: OpId,
    /// Log position assigned to that op.
    pub seq: ServerSeq,
    /// New stamps of ops the server re-stamped (ARCHITECTURE 5.5 step 3).
    pub restamped: Vec<(OpId, Hlc)>,
}

/// Why the server rejected an op (ARCHITECTURE 5.5). There is no `ClockSkew`: ops with a wrong
/// clock are re-stamped, never rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    Denied,
    Conflict,
    Forged,
    Invalid,
    Limit,
}

impl RejectReason {
    pub const ALL: [RejectReason; 5] = [
        RejectReason::Denied,
        RejectReason::Conflict,
        RejectReason::Forged,
        RejectReason::Invalid,
        RejectReason::Limit,
    ];

    /// Wire name, equal to the `RejectReason` kind of `runtime/js/api.d.ts`.
    pub fn name(self) -> &'static str {
        match self {
            RejectReason::Denied => "Denied",
            RejectReason::Conflict => "Conflict",
            RejectReason::Forged => "Forged",
            RejectReason::Invalid => "Invalid",
            RejectReason::Limit => "Limit",
        }
    }
}

/// Typed failure of a server function call (ARCHITECTURE 5.7). `Offline` and `OutcomeUnknown`
/// arise on the client only and never travel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    Denied,
    /// The function is unknown, or a model argument is missing or unreadable (same kind and
    /// text for both, D99).
    NotFound,
    Limit,
    Invalid,
    /// A runtime error of the server VM, by its kind name (ARCHITECTURE 3.4).
    Runtime {
        kind: String,
    },
}

/// A protocol error. Without `sub` it is fatal and the server closes the connection; with
/// `sub` only that [`ClientMsg::Subscribe`] failed (ARCHITECTURE 5.9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolError {
    pub sub: Option<SubId>,
    pub code: ErrorCode,
}

/// Cause of a [`ProtocolError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    /// Malformed frame, unknown version or message, or a message out of order.
    Protocol,
    /// Frame larger than [`Limits::frame_bytes`].
    FrameTooLarge,
    /// Token missing, invalid or of a revoked device key (D102), or replica not owned by the
    /// token's user and device key.
    Unauthorized,
    /// More than [`Limits::subscriptions_per_session`] subscriptions.
    TooManySubscriptions,
    /// Filter larger than [`Limits::filter_nodes`].
    FilterTooLarge,
    /// Filter or sort names a field the client may not read (D101).
    FilterNotReadable,
}

impl ClientMsg {
    /// Value of the `"t"` key.
    pub fn name(&self) -> &'static str {
        match self {
            ClientMsg::Hello(_) => "hello",
            ClientMsg::Subscribe { .. } => "subscribe",
            ClientMsg::Unsubscribe { .. } => "unsubscribe",
            ClientMsg::Held { .. } => "held",
            ClientMsg::Ops(_) => "ops",
            ClientMsg::Call { .. } => "call",
        }
    }
}

impl ServerMsg {
    /// Value of the `"t"` key.
    pub fn name(&self) -> &'static str {
        match self {
            ServerMsg::Welcome(_) => "welcome",
            ServerMsg::Snapshot { .. } => "snapshot",
            ServerMsg::Ops(_) => "ops",
            ServerMsg::Ack(_) => "ack",
            ServerMsg::Reject { .. } => "reject",
            ServerMsg::Evict { .. } => "evict",
            ServerMsg::Reset { .. } => "reset",
            ServerMsg::Return { .. } => "return",
            ServerMsg::Error(_) => "error",
        }
    }
}

/// The limits of ARCHITECTURE 5.9 that the protocol enforces or announces in [`Welcome`].
/// Values change only by decision. Sign in, sidecar and VM limits live with their owners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// WebSocket frame; beyond it fatal [`ErrorCode::FrameTooLarge`].
    pub frame_bytes: u32,
    /// HTTP request body; beyond it 413.
    pub http_body_bytes: u32,
    /// Ops per batch; beyond it `Reject Limit` for the batch.
    pub ops_per_batch: u32,
    /// Ops per principal per second, sustained and burst; beyond it `Reject Limit`.
    pub ops_per_second: u32,
    pub ops_burst: u32,
    /// Rows created per principal and model per hour; beyond it `Reject Limit`.
    pub rows_per_hour: u32,
    /// Value nesting depth; beyond it `Invalid`.
    pub nesting_depth: u32,
    /// Size of one `Text` field in bytes; beyond it `Invalid`.
    pub text_bytes: u32,
    pub subscriptions_per_session: u32,
    /// Query IR nodes per filter, equal to `ostrel_db::api::MAX_FILTER_NODES`.
    pub filter_nodes: u32,
    pub rows_per_snapshot: u32,
    /// Row ids per `Held` or `Evict` page.
    pub ids_per_page: u32,
    /// Held ids per subscription on reconnect; beyond it `Reset`.
    pub held_per_subscription: u32,
    /// Outbox ops a client sends per reconnect before the first `Ack`.
    pub outbox_before_ack: u32,
    /// `Call` rate per session, sustained and burst; beyond it `CallError::Limit`.
    pub calls_per_second: u32,
    pub calls_burst: u32,
    /// A sent `Call` without `Return` fails on the client with `OutcomeUnknown` (D81).
    pub call_timeout_ms: u32,
    /// Rows per bulk `edit` or `drop`, and attempts after version conflicts (D100).
    pub bulk_rows: u32,
    pub bulk_attempts: u32,
    /// Future clock tolerance; later stamps are re-stamped (ARCHITECTURE 5.5 step 3).
    pub clock_tolerance_ms: u32,
    /// Add tags named by one [`OpBody::SetRemove`]; beyond it `Invalid`.
    pub tags_per_remove: u32,
    /// Digits of a `Rank` key (D88); beyond it `Invalid`.
    pub rank_digits: u32,
}

/// The limits table of ARCHITECTURE 5.9.
pub const LIMITS: Limits = Limits {
    frame_bytes: 1 << 20,
    http_body_bytes: 1 << 20,
    ops_per_batch: 500,
    ops_per_second: 50,
    ops_burst: 200,
    rows_per_hour: 1_000,
    nesting_depth: 32,
    text_bytes: 256 << 10,
    subscriptions_per_session: 64,
    filter_nodes: 64,
    rows_per_snapshot: 1_000,
    ids_per_page: 1_000,
    held_per_subscription: 100_000,
    outbox_before_ack: 10_000,
    calls_per_second: 20,
    calls_burst: 50,
    call_timeout_ms: 30_000,
    bulk_rows: 1_000,
    bulk_attempts: 3,
    clock_tolerance_ms: 2_000,
    tags_per_remove: 64,
    rank_digits: 1_024,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_match_the_architecture_table() {
        assert_eq!(LIMITS.frame_bytes, 1_048_576);
        assert_eq!(LIMITS.http_body_bytes, 1_048_576);
        assert_eq!(LIMITS.text_bytes, 262_144);
        assert_eq!(
            LIMITS.filter_nodes as usize,
            ostrel_db::api::MAX_FILTER_NODES
        );
        const {
            assert!(LIMITS.ops_burst >= LIMITS.ops_per_second);
            assert!(LIMITS.calls_burst >= LIMITS.calls_per_second);
            assert!(LIMITS.held_per_subscription >= LIMITS.ids_per_page);
        }
    }

    #[test]
    fn reject_reasons_are_the_five_of_5_5() {
        let names: Vec<_> = RejectReason::ALL.iter().map(|r| r.name()).collect();
        assert_eq!(names, ["Denied", "Conflict", "Forged", "Invalid", "Limit"]);
    }

    #[test]
    fn message_names_are_distinct_per_direction() {
        let r = ReplicaId(1);
        let query = Query {
            model: 0,
            filter: None,
            order: Vec::new(),
            limit: None,
            after: None,
        };
        let client = [
            ClientMsg::Hello(Hello {
                version: PROTOCOL_VERSION,
                token: None,
                replica: None,
                last_server_seq: None,
            }),
            ClientMsg::Subscribe { sub: 1, query },
            ClientMsg::Unsubscribe { sub: 1 },
            ClientMsg::Held {
                sub: 1,
                rows: Vec::new(),
                last: true,
            },
            ClientMsg::Ops(Vec::new()),
            ClientMsg::Call {
                call: 1,
                function: 0,
                args: Vec::new(),
            },
        ];
        let server = [
            ServerMsg::Welcome(Welcome {
                replica: r,
                server_ms: 0,
                limits: LIMITS,
            }),
            ServerMsg::Snapshot {
                sub: 1,
                rows: Vec::new(),
                seq: ServerSeq(0),
                last: true,
            },
            ServerMsg::Ops(Vec::new()),
            ServerMsg::Ack(Ack {
                op: OpId { replica: r, seq: 1 },
                seq: ServerSeq(1),
                restamped: Vec::new(),
            }),
            ServerMsg::Reject {
                op: OpId { replica: r, seq: 1 },
                reason: RejectReason::Denied,
            },
            ServerMsg::Evict {
                sub: 1,
                model: 0,
                rows: Vec::new(),
            },
            ServerMsg::Reset { sub: 1 },
            ServerMsg::Return {
                call: 1,
                result: Err(CallError::NotFound),
            },
            ServerMsg::Error(ProtocolError {
                sub: None,
                code: ErrorCode::Protocol,
            }),
        ];
        let mut c: Vec<_> = client.iter().map(ClientMsg::name).collect();
        let mut s: Vec<_> = server.iter().map(ServerMsg::name).collect();
        c.sort_unstable();
        c.dedup();
        s.sort_unstable();
        s.dedup();
        assert_eq!(c.len(), client.len());
        assert_eq!(s.len(), server.len());
    }
}
