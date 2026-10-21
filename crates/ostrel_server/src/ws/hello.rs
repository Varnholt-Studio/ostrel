//! The `Hello` of a WebSocket session and the limits that hold across sessions (ARCHITECTURE
//! 5.4, 5.9, 8; SPEC 12.14; D156, D158, D159, D161).
//!
//! [`Hellos`] is the one place of a server process that admits sessions. It holds no I/O: the
//! transport reads the first frame with [`read_hello`], asks [`Hellos::admit`] and sends what
//! comes back, the `welcome` of an [`Admitted`] session or the fatal `error` of a
//! [`HelloRefused`], after which it closes the connection.
//!
//! # Order of the checks
//!
//! A `Hello` is checked in this order (ARCHITECTURE 5.4, kira #2294 (a)), and the first check
//! that fails decides the answer:
//!
//! 1. Protocol fields: a `token` in an app without `auth`, a `replica_secret` in an app with
//!    `auth` or without a replica: fatal `Error Protocol` (D122, D150).
//! 2. Token and identity: in an app with `auth` the token is verified and the status of its
//!    device key is checked on the [`KeyStatusBoard`] (D102). A missing or rejected token is
//!    fatal `Error Unauthorized`.
//! 3. Replica: a named replica must belong to the caller (the token's user and device key, or
//!    the `replica_secret` without `auth`). A replica the server has no record of is
//!    `Unauthorized` as well, also after a restart of the memory adapter, so no answer tells
//!    whether a replica id exists (dana #2440, henrik #2526). An expired replica of its owner
//!    is `ReplicaExpired`. These rules are [`answer_hello`].
//! 4. Resume mark: a confirmed replica whose `last_server_seq` lies above the end of the log
//!    is `ReplicaExpired` ([`check_resume_mark`], D161). A new replica is never checked.
//! 5. Sessions per principal: at most [`SessionLimits::per_principal`] open sessions per user,
//!    in an app without `auth` [`SessionLimits::per_ip`] per IP address (`--max-sessions-per-ip`,
//!    D159). Over it: fatal `Error TooManySessions`. A `Hello` that supersedes a session of
//!    the same replica does not count, because the session it replaces is closed first (Q27 (a)).
//!
//! Only a `Hello` that passes all five is accepted. Then, and only then, an older session of
//! the same replica is closed with the fatal `Error Superseded` (D158 (2)): a refused `Hello`
//! never closes, delays or changes another session (SPEC 12.14, S-9).
//!
//! # Call rate
//!
//! `Call` frames are limited per principal over all its sessions, in an app without `auth` per
//! IP address, at [`LIMITS`] `calls_per_second` with burst `calls_burst` (D161). The transport
//! asks [`SessionTicket::admit_call`] before it runs a call; a refusal is the typed
//! [`CallError::Limit`], written with [`super::Session::finish`], and the function does not
//! run. A call that its pre-check refuses later has counted already. The buckets outlive the
//! sessions, so closing and reopening sessions never refills them early.
//!
//! # Closing sessions from outside
//!
//! An admitted session gets a [`SessionTicket`]. It keeps the session registered here and,
//! with `auth`, on the [`KeyStatusBoard`]; dropping it ends both registrations. The transport
//! hands [`Hellos::admit`] a [`SessionEnd`], which is called at most once with the code of the
//! fatal `error` the transport sends before it closes the connection: `Superseded` when a newer
//! `Hello` took the replica, `Unauthorized` when the device key was revoked (D102). It is
//! always called without a lock of this module or of the board held, so it may drop the ticket.
//!
//! ASSUMPTION: in an app without `auth`, a `Hello` that supersedes a session opened from
//! another IP address frees a place of that other address, not of its own; it counts against
//! its own address like any new session. The session it replaces is closed first, as Q27 (a)
//! says, and the limit is checked on the address the `Hello` came from.

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use ostrel_core::ids::RowId;
use ostrel_sync::protocol::{
    CallError, ClientMsg, ErrorCode, Hello, LIMITS, ProtocolError, ReplicaId, ServerMsg, ServerSeq,
    Welcome,
};
use ostrel_sync::replica_id::{
    Answer, Caller, Entropy, Refusal, ReplicaRecord, ReplicaRecords, answer_hello,
    check_resume_mark,
};
use ostrel_sync::wire::{WireSchema, decode_client, encode_server};
use sha2::{Digest, Sha256};

use super::session::OpenCalls;
use crate::key_status::{
    CloseReason, KeyStatusBoard, KeyStatusSource, SessionCloser, SessionGuard, SessionTokenVerifier,
};

/// Open sessions per principal in an app with `auth` (ARCHITECTURE 5.9).
pub const SESSIONS_PER_PRINCIPAL: usize = 16;

/// Default of `--max-sessions-per-ip`, the open sessions per IP address in an app without
/// `auth` (ARCHITECTURE 5.9, D159).
pub const DEFAULT_SESSIONS_PER_IP: usize = 16;

/// The fatal `error` frame with code `Protocol`, written by hand only for the case that the
/// encoder refused an `error` frame, which carries no value and names no model.
const PROTOCOL_ERROR_FRAME: &str =
    r#"{"code":"Protocol","retry_after_ms":null,"sub":null,"t":"error","v":0}"#;

/// Token units per call: the buckets count in thousandths of a call, so a refill of
/// `calls_per_second` per second is a whole number per millisecond.
const MILLI: u64 = 1_000;

/// The session limits of one server process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionLimits {
    /// Open sessions per user in an app with `auth`.
    pub per_principal: usize,
    /// Open sessions per IP address in an app without `auth` (`--max-sessions-per-ip`).
    pub per_ip: usize,
    /// `Call` frames per second per principal, sustained.
    pub calls_per_second: u32,
    /// `Call` frames per principal in one burst.
    pub calls_burst: u32,
}

impl Default for SessionLimits {
    fn default() -> Self {
        SessionLimits {
            per_principal: SESSIONS_PER_PRINCIPAL,
            per_ip: DEFAULT_SESSIONS_PER_IP,
            calls_per_second: LIMITS.calls_per_second,
            calls_burst: LIMITS.calls_burst,
        }
    }
}

impl SessionLimits {
    /// The default limits with `per_ip` open sessions per IP address, the value of
    /// `--max-sessions-per-ip`. Zero would refuse every session and is not a limit: `None`.
    pub fn with_sessions_per_ip(per_ip: usize) -> Option<Self> {
        (per_ip > 0).then(|| SessionLimits {
            per_ip,
            ..SessionLimits::default()
        })
    }
}

/// How a `Hello` proves who sent it: the app mode and the verifiers of an app with `auth`.
pub enum Identity<'a, V, S> {
    /// App with `auth paseto`: tokens are verified by `tokens`, their device key status is
    /// checked on `board`.
    Auth {
        tokens: &'a V,
        board: &'a KeyStatusBoard<S>,
    },
    /// App without `auth`: replicas are bound by their secret, limits count per IP address.
    NoAuth,
}

/// The replicas the server knows, and where it stores a new one.
pub trait ReplicaStore: ReplicaRecords {
    /// Stores the record of a new replica. Called before its `welcome` is sent, so a client
    /// never holds a replica the server does not know.
    fn store(&mut self, record: ReplicaRecord) -> Result<(), StoreFailed>;
}

/// A new replica record could not be stored. The text is for the server log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreFailed(pub String);

/// Ends one open session from outside its own frame flow. Implemented by the transport.
pub trait SessionEnd: Send + Sync {
    /// Sends the fatal `error` with `code` ([`fatal_frame`]) and closes the connection.
    /// Called at most once per admitted session, never while a lock of [`Hellos`] or of the
    /// [`KeyStatusBoard`] is held.
    fn end(&self, code: ErrorCode);
}

/// What one `Hello` needs besides the frame itself.
pub struct HelloInput<'a, R, E> {
    /// The decoded `Hello`.
    pub hello: &'a Hello,
    /// The address the connection came from.
    pub peer: IpAddr,
    /// Known replicas; a new one is stored here before the `welcome`.
    pub records: &'a mut R,
    /// Randomness for a new replica id and secret.
    pub entropy: &'a mut E,
    /// Server time in ms since the epoch.
    pub now_ms: u64,
    /// The highest position of the op log.
    pub log_end: ServerSeq,
    /// How the transport closes this session later.
    pub end: Arc<dyn SessionEnd>,
}

/// Who counts against the session and call limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrincipalKey {
    /// The user of the token, in an app with `auth`.
    User(RowId),
    /// The peer address, in an app without `auth`.
    Ip(IpAddr),
}

/// Why a `Hello` was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelloRefused {
    /// The client gets this fatal `error` ([`fatal_frame`]) and the connection closes.
    Error(ErrorCode),
    /// The entropy source gave only ids that were already issued: a server defect. The
    /// connection closes without a frame.
    EntropyExhausted,
    /// The new replica could not be stored. The connection closes without a frame; the
    /// client sends its `Hello` again on reconnect.
    Store(StoreFailed),
}

/// An accepted `Hello`.
pub struct Admitted {
    /// The `welcome` to send.
    pub welcome: Welcome,
    /// The principal every call of the session runs as, `None` without `auth`.
    pub principal: Option<RowId>,
    /// The registration of the session; drop it when the connection closes.
    pub ticket: SessionTicket,
}

impl fmt::Debug for Admitted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Admitted")
            .field("welcome", &self.welcome)
            .field("principal", &self.principal.map(RowId::to_hex))
            .field("ticket", &self.ticket)
            .finish()
    }
}

/// The open sessions and call buckets of one server process.
pub struct Hellos {
    limits: SessionLimits,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    /// The open session of each replica (one per replica, D156).
    by_replica: HashMap<ReplicaId, Open>,
    /// Open sessions per principal. A principal without sessions has no entry.
    per_key: HashMap<PrincipalKey, usize>,
    /// Call buckets per principal. A full bucket of a principal without sessions is the same
    /// as no bucket, so such buckets are dropped at the next `Hello`.
    buckets: HashMap<PrincipalKey, Bucket>,
    /// Id of the next session, unique within the process.
    next_id: u64,
}

struct Open {
    id: u64,
    key: PrincipalKey,
    end: Arc<dyn SessionEnd>,
}

/// A token bucket in thousandths of a call.
#[derive(Clone, Copy, Debug)]
struct Bucket {
    milli: u64,
    at_ms: u64,
}

impl Bucket {
    fn full(limits: &SessionLimits, now_ms: u64) -> Self {
        Bucket {
            milli: u64::from(limits.calls_burst) * MILLI,
            at_ms: now_ms,
        }
    }

    /// Refills up to `now_ms`. A clock that went back refills nothing.
    fn refill(&mut self, limits: &SessionLimits, now_ms: u64) {
        let cap = u64::from(limits.calls_burst) * MILLI;
        let elapsed = now_ms.saturating_sub(self.at_ms);
        let added = elapsed.saturating_mul(u64::from(limits.calls_per_second));
        self.milli = self.milli.saturating_add(added).min(cap);
        self.at_ms = self.at_ms.max(now_ms);
    }

    fn is_full(&self, limits: &SessionLimits) -> bool {
        self.milli >= u64::from(limits.calls_burst) * MILLI
    }
}

impl Default for Hellos {
    fn default() -> Self {
        Hellos::new(SessionLimits::default())
    }
}

impl fmt::Debug for Hellos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hellos")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Hellos {
    /// The sessions of a server process with `limits`.
    pub fn new(limits: SessionLimits) -> Self {
        Hellos {
            limits,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// The limits in force.
    pub fn limits(&self) -> SessionLimits {
        self.limits
    }

    /// Number of open sessions of `key`.
    pub fn open_sessions(&self, key: PrincipalKey) -> usize {
        lock(&self.state).per_key.get(&key).copied().unwrap_or(0)
    }

    /// True when `replica` has an open session.
    pub fn is_open(&self, replica: ReplicaId) -> bool {
        lock(&self.state).by_replica.contains_key(&replica)
    }

    /// Decides one `Hello` in the order of the module documentation. On acceptance the older
    /// session of the same replica, if any, has been told [`ErrorCode::Superseded`] when this
    /// returns.
    pub fn admit<V, S, R, E>(
        &self,
        identity: Identity<'_, V, S>,
        input: HelloInput<'_, R, E>,
    ) -> Result<Admitted, HelloRefused>
    where
        V: SessionTokenVerifier,
        S: KeyStatusSource,
        R: ReplicaStore,
        E: Entropy,
    {
        let HelloInput {
            hello,
            peer,
            records,
            entropy,
            now_ms,
            log_end,
            end,
        } = input;

        // 2. Token and identity. Protocol fields (1.) are checked first by `answer_hello`,
        // which refuses them before it looks at the caller.
        let (token, board) = match &identity {
            Identity::Auth { tokens, board } => {
                let token = hello
                    .token
                    .as_deref()
                    .and_then(|t| tokens.verify_session(t).ok())
                    .filter(|t| board.check(t).is_ok());
                (token, Some(*board))
            }
            Identity::NoAuth => (None, None),
        };
        let caller = match (&identity, &token) {
            (Identity::NoAuth, _) => Caller::NoAuth,
            (Identity::Auth { .. }, Some(t)) => Caller::Signed {
                user: t.user,
                device_key: t.kid.as_str(),
            },
            (Identity::Auth { .. }, None) => Caller::Unsigned,
        };

        // 3. and 4. Replica and resume mark.
        let digest = |bytes: &[u8]| -> [u8; 32] { Sha256::digest(bytes).into() };
        let answer =
            answer_hello(hello, caller, &*records, entropy, digest, now_ms).map_err(refused)?;
        check_resume_mark(&answer, hello, log_end).map_err(refused)?;

        let (key, principal) = match &token {
            Some(t) => (PrincipalKey::User(t.user), Some(t.user)),
            None => (PrincipalKey::Ip(peer), None),
        };
        let limit = match identity {
            Identity::Auth { .. } => self.limits.per_principal,
            Identity::NoAuth => self.limits.per_ip,
        };
        let (replica, secret, record) = match answer {
            Answer::Confirmed(replica) => (replica, None, None),
            Answer::Issued { record, secret } => (record.replica, secret, Some(record)),
        };

        let mut state = lock(&self.state);
        // 5. Sessions per principal; the session this one replaces is closed first.
        let replaced_here = state
            .by_replica
            .get(&replica)
            .is_some_and(|open| open.key == key);
        let open_here = state.per_key.get(&key).copied().unwrap_or(0);
        if open_here - usize::from(replaced_here) >= limit {
            return Err(HelloRefused::Error(ErrorCode::TooManySessions));
        }

        // Accepted by every check. The device key registration repeats its status check in
        // the same step that registers, so a revocation after the first look is not missed.
        let key_guard = match (board, &token) {
            (Some(board), Some(t)) => {
                let closer: Arc<dyn SessionCloser> = Arc::new(KeyCloser(Arc::clone(&end)));
                match board.open_session(t, closer) {
                    Ok(guard) => Some(guard),
                    Err(_) => return Err(HelloRefused::Error(ErrorCode::Unauthorized)),
                }
            }
            _ => None,
        };
        if let Some(record) = record {
            records.store(record).map_err(HelloRefused::Store)?;
        }

        let id = state.next_id;
        // 2^64 sessions do not happen in the life of a process; wrapping keeps the counter
        // total without a panic path.
        state.next_id = id.wrapping_add(1);
        let replaced = state.by_replica.insert(
            replica,
            Open {
                id,
                key,
                end: Arc::clone(&end),
            },
        );
        if let Some(old) = &replaced {
            decrement(&mut state.per_key, old.key);
        }
        *state.per_key.entry(key).or_insert(0) += 1;
        let limits = self.limits;
        state
            .buckets
            .retain(|k, bucket| *k == key || !bucket_is_idle(bucket, &limits, now_ms));
        drop(state);

        // Outside the lock: the transport may drop the old ticket from inside `end`.
        if let Some(old) = replaced {
            old.end.end(ErrorCode::Superseded);
        }
        Ok(Admitted {
            welcome: Welcome {
                replica,
                replica_secret: secret,
                server_ms: now_ms,
                limits: LIMITS,
            },
            principal,
            ticket: SessionTicket {
                state: Arc::downgrade(&self.state),
                limits,
                replica,
                key,
                id,
                _key_guard: key_guard,
            },
        })
    }
}

/// Removes idle buckets: a full one at `now_ms` behaves like none.
fn bucket_is_idle(bucket: &Bucket, limits: &SessionLimits, now_ms: u64) -> bool {
    let mut at_now = *bucket;
    at_now.refill(limits, now_ms);
    at_now.is_full(limits)
}

fn decrement(per_key: &mut HashMap<PrincipalKey, usize>, key: PrincipalKey) {
    if let Some(n) = per_key.get_mut(&key) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            per_key.remove(&key);
        }
    }
}

fn refused(refusal: Refusal) -> HelloRefused {
    match refusal {
        Refusal::Error(code) => HelloRefused::Error(code),
        Refusal::EntropyExhausted => HelloRefused::EntropyExhausted,
    }
}

/// Passes a revocation of the device key on as `Error Unauthorized` (ARCHITECTURE 5.4).
struct KeyCloser(Arc<dyn SessionEnd>);

impl SessionCloser for KeyCloser {
    fn close(&self, reason: CloseReason) {
        match reason {
            CloseReason::KeyRevoked => self.0.end(ErrorCode::Unauthorized),
        }
    }
}

/// The registration of one admitted session. Dropping it frees its place under the session
/// limits and ends its device key registration; after a supersession it frees nothing twice.
pub struct SessionTicket {
    state: Weak<Mutex<State>>,
    limits: SessionLimits,
    replica: ReplicaId,
    key: PrincipalKey,
    id: u64,
    _key_guard: Option<SessionGuard>,
}

impl SessionTicket {
    /// The replica of the session.
    pub fn replica(&self) -> ReplicaId {
        self.replica
    }

    /// The principal the session counts against.
    pub fn key(&self) -> PrincipalKey {
        self.key
    }

    /// Takes one `Call` from the bucket of the principal at `now_ms`. Over the rate the call
    /// gets [`CallError::Limit`] and must not run (ARCHITECTURE 5.9, D161).
    pub fn admit_call(&self, now_ms: u64) -> Result<(), CallError> {
        let Some(state) = self.state.upgrade() else {
            return Err(CallError::Limit);
        };
        let mut state = lock(&state);
        let limits = self.limits;
        let bucket = state
            .buckets
            .entry(self.key)
            .or_insert_with(|| Bucket::full(&limits, now_ms));
        bucket.refill(&limits, now_ms);
        if bucket.milli >= MILLI {
            bucket.milli -= MILLI;
            Ok(())
        } else {
            Err(CallError::Limit)
        }
    }
}

impl fmt::Debug for SessionTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionTicket")
            .field("replica", &self.replica.to_hex())
            .field("id", &self.id)
            .finish()
    }
}

impl Drop for SessionTicket {
    fn drop(&mut self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let mut state = lock(&state);
        let ours = state
            .by_replica
            .get(&self.replica)
            .is_some_and(|open| open.id == self.id);
        if ours {
            state.by_replica.remove(&self.replica);
            decrement(&mut state.per_key, self.key);
        }
    }
}

/// Every update of [`State`] keeps it consistent between statements, so a panic while the
/// lock was held leaves usable state; the process keeps admitting sessions.
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Reads the first frame of a connection. Anything but a well formed `hello` is answered with
/// the returned fatal `error` frame, after which the transport closes the connection.
pub fn read_hello(frame: &[u8], schema: &dyn WireSchema) -> Result<Hello, String> {
    match decode_client(frame, schema) {
        Ok(ClientMsg::Hello(hello)) => Ok(hello),
        Ok(_) => Err(fatal_frame(ErrorCode::Protocol, schema)),
        Err(error) => {
            let msg = match error.reply() {
                ServerMsg::Error(e) if e.sub.is_none() => ServerMsg::Error(e),
                // Before `welcome` there is no subscription and no call to answer.
                _ => ServerMsg::Error(ProtocolError::fatal(ErrorCode::Protocol)),
            };
            Err(encode(&msg, schema))
        }
    }
}

/// The fatal `error` frame with `code`.
pub fn fatal_frame(code: ErrorCode, schema: &dyn WireSchema) -> String {
    encode(&ServerMsg::Error(ProtocolError::fatal(code)), schema)
}

/// The `welcome` frame of an admitted session.
pub fn welcome_frame(welcome: &Welcome, schema: &dyn WireSchema) -> String {
    encode(&ServerMsg::Welcome(welcome.clone()), schema)
}

fn encode(msg: &ServerMsg, schema: &dyn WireSchema) -> String {
    encode_server(msg, schema, &OpenCalls::default())
        .unwrap_or_else(|_| PROTOCOL_ERROR_FRAME.to_owned())
}
