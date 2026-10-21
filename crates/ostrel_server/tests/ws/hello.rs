//! The `Hello` path: order of the checks, one session per replica, sessions per principal,
//! device key status and the `Call` rate per principal (ARCHITECTURE 5.4, 5.9, 8; SPEC 12.14;
//! D156, D158, D159, D161; henrik #2526).
//!
//! These are the server side parts of the SPEC 12.14 tests `ac_34_sessions_per_principal`,
//! `ac_34_call_rate`, `ac_57_superseded_refused_hello`, `ac_57_replica_expired` and
//! `ac_57_unknown_replica`, driven without a socket. They keep descriptive names until AC-34
//! and AC-57 are covered as a whole (D72: a name is never evidence).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};

use ostrel_core::ids::{Hlc, ReplicaId, RowId, ServerSeq};
use ostrel_server::identity::TokenRejected;
use ostrel_server::key_status::{
    KeyId, KeyStatus, KeyStatusBoard, KeyStatusSource, SessionToken, SessionTokenVerifier,
};
use ostrel_server::ws::{
    Admitted, HelloInput, HelloRefused, Hellos, Identity, Incoming, PrincipalKey, ReplicaStore,
    Session, SessionEnd, SessionLimits, StoreFailed, fatal_frame, read_hello, welcome_frame,
};
use ostrel_sync::protocol::{CallError, ErrorCode, Hello};
use ostrel_sync::replica_id::{Entropy, Owner, ReplicaRecord, ReplicaRecords, SECRET_BYTES};

use super::{Schema, call_frame};

// ---------------------------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------------------------

/// Tokens of the form `user:<n>:<kid>`; everything else is rejected.
struct Tokens;

impl SessionTokenVerifier for Tokens {
    fn verify_session(&self, token: &str) -> Result<SessionToken, TokenRejected> {
        let mut parts = token.split(':');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("user"), Some(n), Some(kid), None) => {
                let n: u16 = n.parse().map_err(|_| TokenRejected("bad user".into()))?;
                let kid = KeyId::new(kid).ok_or_else(|| TokenRejected("bad kid".into()))?;
                Ok(SessionToken { user: user(n), kid })
            }
            _ => Err(TokenRejected("bad token".into())),
        }
    }
}

/// Every key is active unless listed as revoked.
#[derive(Default)]
struct Keys {
    revoked: Mutex<HashSet<(RowId, String)>>,
}

impl KeyStatusSource for &Keys {
    fn status(&self, user: RowId, kid: &KeyId) -> KeyStatus {
        let revoked = self.revoked.lock().unwrap();
        if revoked.contains(&(user, kid.as_str().to_owned())) {
            KeyStatus::Revoked
        } else {
            KeyStatus::Active
        }
    }
}

#[derive(Default)]
struct Records {
    live: HashMap<ReplicaId, ReplicaRecord>,
    issued: HashSet<ReplicaId>,
    fail: bool,
}

impl ReplicaRecords for Records {
    fn find(&self, replica: ReplicaId) -> Option<&ReplicaRecord> {
        self.live.get(&replica)
    }

    fn was_issued(&self, replica: ReplicaId) -> bool {
        self.issued.contains(&replica)
    }
}

impl ReplicaStore for Records {
    fn store(&mut self, record: ReplicaRecord) -> Result<(), StoreFailed> {
        if self.fail {
            return Err(StoreFailed("disk full".into()));
        }
        self.issued.insert(record.replica);
        self.live.insert(record.replica, record);
        Ok(())
    }
}

/// Counts up from 1; the secret is the counter in every byte.
struct Counter(u64);

impl Entropy for Counter {
    fn next_u64(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }

    fn secret(&mut self) -> [u8; SECRET_BYTES] {
        [self.0 as u8; SECRET_BYTES]
    }
}

/// Records what the transport was told.
#[derive(Default)]
struct Ends(Mutex<Vec<ErrorCode>>);

impl SessionEnd for Ends {
    fn end(&self, code: ErrorCode) {
        self.0.lock().unwrap().push(code);
    }
}

impl Ends {
    fn seen(&self) -> Vec<ErrorCode> {
        self.0.lock().unwrap().clone()
    }
}

fn user(n: u16) -> RowId {
    RowId::new(Hlc::new(1_000, n, ReplicaId(7)).unwrap())
}

fn kid(s: &str) -> KeyId {
    KeyId::new(s).unwrap()
}

const NOW: u64 = 1_700_000_000_000;
const PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
const OTHER_PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

fn hello(token: Option<&str>, replica: Option<ReplicaId>) -> Hello {
    Hello {
        token: token.map(str::to_owned),
        replica,
        replica_secret: None,
        last_server_seq: None,
    }
}

/// A server of an app with `auth`.
struct Auth {
    keys: Keys,
    hellos: Hellos,
    records: RefCell<Records>,
    entropy: RefCell<Counter>,
    log_end: ServerSeq,
}

impl Auth {
    fn new(limits: SessionLimits) -> Self {
        Auth {
            keys: Keys::default(),
            hellos: Hellos::new(limits),
            records: RefCell::new(Records::default()),
            entropy: RefCell::new(Counter(0)),
            log_end: ServerSeq(100),
        }
    }

    fn admit_with(
        &self,
        board: &KeyStatusBoard<&Keys>,
        hello: &Hello,
        end: &Arc<Ends>,
    ) -> Result<Admitted, HelloRefused> {
        let end: Arc<dyn SessionEnd> = end.clone();
        self.hellos.admit(
            Identity::Auth {
                tokens: &Tokens,
                board,
            },
            HelloInput {
                hello,
                peer: PEER,
                records: &mut *self.records.borrow_mut(),
                entropy: &mut *self.entropy.borrow_mut(),
                now_ms: NOW,
                log_end: self.log_end,
                end,
            },
        )
    }
}

/// A server of an app without `auth`.
struct NoAuth {
    hellos: Hellos,
    records: RefCell<Records>,
    entropy: RefCell<Counter>,
}

impl NoAuth {
    fn new(limits: SessionLimits) -> Self {
        NoAuth {
            hellos: Hellos::new(limits),
            records: RefCell::new(Records::default()),
            entropy: RefCell::new(Counter(0)),
        }
    }

    fn admit(
        &self,
        hello: &Hello,
        peer: IpAddr,
        end: &Arc<Ends>,
    ) -> Result<Admitted, HelloRefused> {
        let end: Arc<dyn SessionEnd> = end.clone();
        self.hellos.admit::<Tokens, &Keys, _, _>(
            Identity::NoAuth,
            HelloInput {
                hello,
                peer,
                records: &mut *self.records.borrow_mut(),
                entropy: &mut *self.entropy.borrow_mut(),
                now_ms: NOW,
                log_end: ServerSeq(0),
                end,
            },
        )
    }
}

fn refused(result: Result<Admitted, HelloRefused>) -> HelloRefused {
    match result {
        Ok(admitted) => panic!("accepted: {admitted:?}"),
        Err(refusal) => refusal,
    }
}

fn error(code: ErrorCode) -> HelloRefused {
    HelloRefused::Error(code)
}

// ---------------------------------------------------------------------------------------------
// Order of the checks
// ---------------------------------------------------------------------------------------------

#[test]
fn a_signed_hello_without_a_replica_gets_a_new_stored_replica() {
    let s = Auth::new(SessionLimits::default());
    let board = KeyStatusBoard::new(&s.keys);
    let end = Arc::new(Ends::default());
    let admitted = s
        .admit_with(&board, &hello(Some("user:1:k1"), None), &end)
        .unwrap();
    assert_eq!(admitted.principal, Some(user(1)));
    assert_eq!(admitted.welcome.replica_secret, None);
    assert_eq!(admitted.welcome.server_ms, NOW);
    let record = s
        .records
        .borrow()
        .find(admitted.welcome.replica)
        .cloned()
        .expect("the new replica is stored before the welcome");
    assert_eq!(
        record.owner,
        Owner::Device {
            user: user(1),
            device_key: "k1".into()
        }
    );
    assert!(s.hellos.is_open(admitted.welcome.replica));
    assert_eq!(board.open_sessions(user(1), &kid("k1")), 1);
    assert_eq!(s.hellos.open_sessions(PrincipalKey::User(user(1))), 1);

    drop(admitted);
    assert_eq!(board.open_sessions(user(1), &kid("k1")), 0);
    assert_eq!(s.hellos.open_sessions(PrincipalKey::User(user(1))), 0);
    assert!(end.seen().is_empty());
}

#[test]
fn protocol_fields_are_checked_before_the_token() {
    let s = Auth::new(SessionLimits::default());
    let board = KeyStatusBoard::new(&s.keys);
    let end = Arc::new(Ends::default());
    // A replica_secret in an app with auth, together with a token that fails verification.
    let mut h = hello(Some("forged"), Some(ReplicaId(1)));
    h.replica_secret = Some("00".repeat(16));
    assert_eq!(
        refused(s.admit_with(&board, &h, &end)),
        error(ErrorCode::Protocol)
    );

    let n = NoAuth::new(SessionLimits::default());
    let h = hello(Some(""), None);
    assert_eq!(refused(n.admit(&h, PEER, &end)), error(ErrorCode::Protocol));
}

#[test]
fn a_missing_forged_or_revoked_token_is_unauthorized() {
    let s = Auth::new(SessionLimits::default());
    let board = KeyStatusBoard::new(&s.keys);
    let end = Arc::new(Ends::default());
    for token in [None, Some("forged"), Some("user:x:k1")] {
        let r = refused(s.admit_with(&board, &hello(token, None), &end));
        assert_eq!(r, error(ErrorCode::Unauthorized), "token {token:?}");
    }
    // Revoked in the persistent source.
    s.keys
        .revoked
        .lock()
        .unwrap()
        .insert((user(1), "k1".into()));
    let r = refused(s.admit_with(&board, &hello(Some("user:1:k1"), None), &end));
    assert_eq!(r, error(ErrorCode::Unauthorized));
    // Revoked in this process only.
    board.revoke(user(2), &kid("k2"));
    let r = refused(s.admit_with(&board, &hello(Some("user:2:k2"), None), &end));
    assert_eq!(r, error(ErrorCode::Unauthorized));
    assert!(
        s.records.borrow().live.is_empty(),
        "no replica for a refused Hello"
    );
}

#[test]
fn revoking_the_device_key_ends_the_session_with_unauthorized() {
    let s = Auth::new(SessionLimits::default());
    let board = KeyStatusBoard::new(&s.keys);
    let end = Arc::new(Ends::default());
    let admitted = s
        .admit_with(&board, &hello(Some("user:1:k1"), None), &end)
        .unwrap();
    assert_eq!(board.revoke(user(1), &kid("k1")), 1);
    assert_eq!(end.seen(), vec![ErrorCode::Unauthorized]);
    // The transport drops the ticket when it closes the connection.
    drop(admitted);
    assert_eq!(s.hellos.open_sessions(PrincipalKey::User(user(1))), 0);
}

#[test]
fn an_unknown_replica_is_unauthorized_also_after_a_memory_restart() {
    // An id the server never issued, with a mark far above the log end: Unauthorized, never
    // ReplicaExpired, so no answer tells whether a replica id exists (henrik #2526).
    let s = Auth::new(SessionLimits::default());
    let board = KeyStatusBoard::new(&s.keys);
    let end = Arc::new(Ends::default());
    let mut h = hello(Some("user:1:k1"), Some(ReplicaId(0xdead)));
    h.last_server_seq = Some(ServerSeq(1_000_000));
    assert_eq!(
        refused(s.admit_with(&board, &h, &end)),
        error(ErrorCode::Unauthorized)
    );

    // The memory adapter restarts and loses every record: the client's next Hello names a
    // replica the server has no record of.
    let first = s
        .admit_with(&board, &hello(Some("user:1:k1"), None), &end)
        .unwrap();
    let replica = first.welcome.replica;
    drop(first);
    *s.records.borrow_mut() = Records::default();
    let mut h = hello(Some("user:1:k1"), Some(replica));
    h.last_server_seq = Some(ServerSeq(50));
    let restarted = Auth {
        log_end: ServerSeq(0),
        ..Auth::new(SessionLimits::default())
    };
    let board = KeyStatusBoard::new(&restarted.keys);
    assert_eq!(
        refused(restarted.admit_with(&board, &h, &end)),
        error(ErrorCode::Unauthorized)
    );
}

#[test]
fn a_confirmed_replica_above_the_log_end_is_replica_expired() {
    let s = Auth::new(SessionLimits::default());
    let board = KeyStatusBoard::new(&s.keys);
    let end = Arc::new(Ends::default());
    let first = s
        .admit_with(&board, &hello(Some("user:1:k1"), None), &end)
        .unwrap();
    let replica = first.welcome.replica;

    // Confirmed for its owner, mark above the log end (100).
    let mut h = hello(Some("user:1:k1"), Some(replica));
    h.last_server_seq = Some(ServerSeq(101));
    assert_eq!(
        refused(s.admit_with(&board, &h, &end)),
        error(ErrorCode::ReplicaExpired)
    );
    // The refused Hello did not supersede the open session.
    assert!(end.seen().is_empty());
    assert!(s.hellos.is_open(replica));

    // The same mark from another device of the same user is Unauthorized: the owner check
    // comes first.
    let mut h = hello(Some("user:1:k2"), Some(replica));
    h.last_server_seq = Some(ServerSeq(101));
    assert_eq!(
        refused(s.admit_with(&board, &h, &end)),
        error(ErrorCode::Unauthorized)
    );

    // At the log end the replica resumes and supersedes the open session.
    let mut h = hello(Some("user:1:k1"), Some(replica));
    h.last_server_seq = Some(ServerSeq(100));
    let _second = s.admit_with(&board, &h, &end).unwrap();
    assert_eq!(end.seen(), vec![ErrorCode::Superseded]);
    drop(first);
}

// ---------------------------------------------------------------------------------------------
// One session per replica
// ---------------------------------------------------------------------------------------------

#[test]
fn an_accepted_hello_supersedes_the_open_session_of_its_replica() {
    let s = Auth::new(SessionLimits::default());
    let board = KeyStatusBoard::new(&s.keys);
    let old_end = Arc::new(Ends::default());
    let new_end = Arc::new(Ends::default());
    let old = s
        .admit_with(&board, &hello(Some("user:1:k1"), None), &old_end)
        .unwrap();
    let replica = old.welcome.replica;

    let new = s
        .admit_with(&board, &hello(Some("user:1:k1"), Some(replica)), &new_end)
        .unwrap();
    assert_eq!(new.welcome.replica, replica);
    assert_eq!(old_end.seen(), vec![ErrorCode::Superseded]);
    assert!(new_end.seen().is_empty());
    assert_eq!(s.hellos.open_sessions(PrincipalKey::User(user(1))), 1);

    // The transport closes the old connection; that frees nothing of the new session.
    drop(old);
    assert!(s.hellos.is_open(replica));
    assert_eq!(s.hellos.open_sessions(PrincipalKey::User(user(1))), 1);
    assert_eq!(board.open_sessions(user(1), &kid("k1")), 1);
    drop(new);
    assert!(!s.hellos.is_open(replica));
    assert_eq!(s.hellos.open_sessions(PrincipalKey::User(user(1))), 0);
}

#[test]
fn a_refused_hello_leaves_the_open_session_untouched() {
    // With auth: a token of another user and a token of another device key of the same user
    // name the open replica. Both are refused and the open session stays untouched.
    let s = Auth::new(SessionLimits::default());
    let board = KeyStatusBoard::new(&s.keys);
    let end = Arc::new(Ends::default());
    let open = s
        .admit_with(&board, &hello(Some("user:1:k1"), None), &end)
        .unwrap();
    let replica = open.welcome.replica;
    for token in ["user:2:k1", "user:1:k2", "forged"] {
        let r = refused(s.admit_with(&board, &hello(Some(token), Some(replica)), &end));
        assert_eq!(r, error(ErrorCode::Unauthorized), "token {token}");
    }
    assert!(end.seen().is_empty());
    assert!(s.hellos.is_open(replica));
    assert_eq!(s.hellos.open_sessions(PrincipalKey::User(user(1))), 1);
    assert_eq!(s.hellos.open_sessions(PrincipalKey::User(user(2))), 0);

    // Without auth: the open replica named with a wrong secret and without one.
    let n = NoAuth::new(SessionLimits::default());
    let end = Arc::new(Ends::default());
    let open = n.admit(&hello(None, None), PEER, &end).unwrap();
    let replica = open.welcome.replica;
    let secret = open.welcome.replica_secret.clone().unwrap();
    let mut wrong = hello(None, Some(replica));
    wrong.replica_secret = Some("ff".repeat(16));
    assert_ne!(wrong.replica_secret.as_deref(), Some(secret.as_str()));
    assert_eq!(
        refused(n.admit(&wrong, OTHER_PEER, &end)),
        error(ErrorCode::Unauthorized)
    );
    assert_eq!(
        refused(n.admit(&hello(None, Some(replica)), OTHER_PEER, &end)),
        error(ErrorCode::Unauthorized)
    );
    assert!(end.seen().is_empty());
    assert!(n.hellos.is_open(replica));

    // The right secret supersedes.
    let mut right = hello(None, Some(replica));
    right.replica_secret = Some(secret);
    let _new = n.admit(&right, PEER, &end).unwrap();
    assert_eq!(end.seen(), vec![ErrorCode::Superseded]);
}

// ---------------------------------------------------------------------------------------------
// Sessions per principal
// ---------------------------------------------------------------------------------------------

#[test]
fn the_seventeenth_verified_hello_gets_too_many_sessions() {
    let s = Auth::new(SessionLimits::default());
    let board = KeyStatusBoard::new(&s.keys);
    let end = Arc::new(Ends::default());
    let open: Vec<Admitted> = (0..16)
        .map(|_| {
            s.admit_with(&board, &hello(Some("user:1:k1"), None), &end)
                .unwrap()
        })
        .collect();
    assert_eq!(s.hellos.open_sessions(PrincipalKey::User(user(1))), 16);
    let stored = s.records.borrow().live.len();

    // The 17th verified Hello.
    assert_eq!(
        refused(s.admit_with(&board, &hello(Some("user:1:k1"), None), &end)),
        error(ErrorCode::TooManySessions)
    );
    assert_eq!(s.records.borrow().live.len(), stored, "no replica stored");
    // Verification comes first: an invalid token is Unauthorized, not TooManySessions.
    assert_eq!(
        refused(s.admit_with(&board, &hello(Some("forged"), None), &end)),
        error(ErrorCode::Unauthorized)
    );
    // So is a token of a revoked device key of the same user.
    s.keys
        .revoked
        .lock()
        .unwrap()
        .insert((user(1), "k2".into()));
    assert_eq!(
        refused(s.admit_with(&board, &hello(Some("user:1:k2"), None), &end)),
        error(ErrorCode::Unauthorized)
    );
    // Another user is not limited by user 1.
    let _other = s
        .admit_with(&board, &hello(Some("user:2:k1"), None), &end)
        .unwrap();
    // A superseding Hello at 16 is accepted and does not count.
    let replica = open[3].welcome.replica;
    let _again = s
        .admit_with(&board, &hello(Some("user:1:k1"), Some(replica)), &end)
        .unwrap();
    assert_eq!(end.seen(), vec![ErrorCode::Superseded]);
    assert_eq!(s.hellos.open_sessions(PrincipalKey::User(user(1))), 16);
    // Closing one session frees a place.
    drop(open);
    assert_eq!(s.hellos.open_sessions(PrincipalKey::User(user(1))), 1);
    let _fresh = s
        .admit_with(&board, &hello(Some("user:1:k1"), None), &end)
        .unwrap();
}

#[test]
fn sessions_per_ip_without_auth_follow_the_server_option() {
    assert_eq!(SessionLimits::with_sessions_per_ip(0), None);
    let limits = SessionLimits::with_sessions_per_ip(2).unwrap();
    assert_eq!(limits.per_principal, 16);
    let n = NoAuth::new(limits);
    let end = Arc::new(Ends::default());
    let a = n.admit(&hello(None, None), PEER, &end).unwrap();
    let _b = n.admit(&hello(None, None), PEER, &end).unwrap();
    assert_eq!(
        refused(n.admit(&hello(None, None), PEER, &end)),
        error(ErrorCode::TooManySessions)
    );
    // Another address has its own count.
    let _c = n.admit(&hello(None, None), OTHER_PEER, &end).unwrap();

    // A Hello from the full address that supersedes a session of that address is accepted.
    let mut same = hello(None, Some(a.welcome.replica));
    same.replica_secret = a.welcome.replica_secret.clone();
    let a2 = n.admit(&same, PEER, &end).unwrap();
    assert_eq!(end.seen(), vec![ErrorCode::Superseded]);
    assert_eq!(n.hellos.open_sessions(PrincipalKey::Ip(PEER)), 2);

    // ASSUMPTION of the module: superseding a session of another address counts against the
    // address the Hello came from. OTHER_PEER has one place left, so this is accepted ...
    let mut moved = hello(None, Some(a2.welcome.replica));
    moved.replica_secret = a.welcome.replica_secret.clone();
    let _moved = n.admit(&moved, OTHER_PEER, &end).unwrap();
    assert_eq!(n.hellos.open_sessions(PrincipalKey::Ip(PEER)), 1);
    assert_eq!(n.hellos.open_sessions(PrincipalKey::Ip(OTHER_PEER)), 2);
    // ... and moving one more to the full OTHER_PEER is refused and closes nothing.
    let b2 = n.admit(&hello(None, None), PEER, &end).unwrap();
    let mut full = hello(None, Some(b2.welcome.replica));
    full.replica_secret = b2.welcome.replica_secret.clone();
    let before = end.seen().len();
    assert_eq!(
        refused(n.admit(&full, OTHER_PEER, &end)),
        error(ErrorCode::TooManySessions)
    );
    assert_eq!(end.seen().len(), before);
    assert!(n.hellos.is_open(b2.welcome.replica));
}

#[test]
fn a_replica_that_cannot_be_stored_opens_no_session() {
    let s = Auth::new(SessionLimits::default());
    s.records.borrow_mut().fail = true;
    let board = KeyStatusBoard::new(&s.keys);
    let end = Arc::new(Ends::default());
    let r = refused(s.admit_with(&board, &hello(Some("user:1:k1"), None), &end));
    assert_eq!(r, HelloRefused::Store(StoreFailed("disk full".into())));
    assert_eq!(s.hellos.open_sessions(PrincipalKey::User(user(1))), 0);
    assert_eq!(board.open_sessions(user(1), &kid("k1")), 0);
}

// ---------------------------------------------------------------------------------------------
// Call rate per principal
// ---------------------------------------------------------------------------------------------

#[test]
fn the_fifty_first_call_of_a_principal_gets_limit() {
    let s = Auth::new(SessionLimits::default());
    let board = KeyStatusBoard::new(&s.keys);
    let end = Arc::new(Ends::default());
    let one = s
        .admit_with(&board, &hello(Some("user:1:k1"), None), &end)
        .unwrap();
    let two = s
        .admit_with(&board, &hello(Some("user:1:k1"), None), &end)
        .unwrap();
    let other = s
        .admit_with(&board, &hello(Some("user:2:k1"), None), &end)
        .unwrap();

    // A burst of 50 over two sessions of one principal, all at the same instant.
    for i in 0..50 {
        let ticket = if i % 2 == 0 { &one.ticket } else { &two.ticket };
        assert_eq!(ticket.admit_call(NOW), Ok(()), "call {i}");
    }
    assert_eq!(one.ticket.admit_call(NOW), Err(CallError::Limit));
    assert_eq!(two.ticket.admit_call(NOW), Err(CallError::Limit));
    // Another principal has its own bucket.
    assert_eq!(other.ticket.admit_call(NOW), Ok(()));

    // The refused call gets its typed return; the function never ran.
    let mut session = Session::new(one.principal);
    let Incoming::Call(job) = session.receive(&call_frame(1, "count", "[]"), &Schema) else {
        panic!("a call");
    };
    let refusal = one.ticket.admit_call(NOW).unwrap_err();
    let frame = session.finish(job.call, Err(refusal), &Schema).unwrap();
    assert_eq!(
        frame,
        r#"{"call":1,"result":{"err":"Limit"},"t":"return","v":0}"#
    );
    assert!(session.open_calls().is_empty());

    // 20 per second: one call per 50 ms.
    assert_eq!(one.ticket.admit_call(NOW + 49), Err(CallError::Limit));
    assert_eq!(two.ticket.admit_call(NOW + 50), Ok(()));
    assert_eq!(one.ticket.admit_call(NOW + 50), Err(CallError::Limit));
    // A clock that goes back refills nothing.
    assert_eq!(one.ticket.admit_call(NOW), Err(CallError::Limit));

    // Closing and reopening sessions does not refill the bucket.
    drop(one);
    drop(two);
    let three = s
        .admit_with(&board, &hello(Some("user:1:k1"), None), &end)
        .unwrap();
    assert_eq!(three.ticket.admit_call(NOW + 50), Err(CallError::Limit));
    // After 2.5 s the burst is back, and not more.
    for i in 0..50 {
        assert_eq!(three.ticket.admit_call(NOW + 10_000), Ok(()), "call {i}");
    }
    assert_eq!(three.ticket.admit_call(NOW + 10_000), Err(CallError::Limit));
}

#[test]
fn the_call_rate_without_auth_is_per_address() {
    let n = NoAuth::new(SessionLimits::default());
    let end = Arc::new(Ends::default());
    let a = n.admit(&hello(None, None), PEER, &end).unwrap();
    let b = n.admit(&hello(None, None), PEER, &end).unwrap();
    let c = n.admit(&hello(None, None), OTHER_PEER, &end).unwrap();
    for _ in 0..25 {
        a.ticket.admit_call(NOW).unwrap();
        b.ticket.admit_call(NOW).unwrap();
    }
    assert_eq!(a.ticket.admit_call(NOW), Err(CallError::Limit));
    assert_eq!(c.ticket.admit_call(NOW), Ok(()));
    assert_eq!(a.principal, None);
}

// ---------------------------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------------------------

#[test]
fn the_first_frame_must_be_a_hello() {
    let frame = br#"{"last_server_seq":null,"replica":"0123456789abcdef","replica_secret":null,"t":"hello","token":null,"v":0}"#;
    let h = read_hello(frame, &Schema).unwrap();
    assert_eq!(h.replica, Some(ReplicaId(0x0123_4567_89ab_cdef)));

    let protocol = r#"{"code":"Protocol","retry_after_ms":null,"sub":null,"t":"error","v":0}"#;
    assert_eq!(
        read_hello(&call_frame(1, "count", "[]"), &Schema),
        Err(protocol.to_owned())
    );
    assert_eq!(read_hello(b"not json", &Schema), Err(protocol.to_owned()));
    // A call frame the decoder refuses gets no `return` before the welcome.
    assert_eq!(
        read_hello(&call_frame(1, "nope", "[]"), &Schema),
        Err(protocol.to_owned())
    );
}

#[test]
fn refusals_and_the_welcome_have_their_frames() {
    assert_eq!(
        fatal_frame(ErrorCode::Superseded, &Schema),
        r#"{"code":"Superseded","retry_after_ms":null,"sub":null,"t":"error","v":0}"#
    );
    assert_eq!(
        fatal_frame(ErrorCode::TooManySessions, &Schema),
        r#"{"code":"TooManySessions","retry_after_ms":null,"sub":null,"t":"error","v":0}"#
    );
    let n = NoAuth::new(SessionLimits::default());
    let end = Arc::new(Ends::default());
    let a = n.admit(&hello(None, None), PEER, &end).unwrap();
    let frame = welcome_frame(&a.welcome, &Schema);
    assert!(frame.contains(r#""t":"welcome""#), "{frame}");
    assert!(frame.contains(&a.welcome.replica.to_hex()), "{frame}");
    assert!(frame.contains(a.welcome.replica_secret.as_deref().unwrap()));
    // The secret never reaches Debug output.
    assert!(!format!("{a:?}").contains(a.welcome.replica_secret.as_deref().unwrap()));
}
