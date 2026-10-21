//! Status of the device key behind a session token (ARCHITECTURE 8, D102, AC-63 (d)).
//!
//! A session token names the key that signed the sign in challenge in its claim `kid`. The
//! status of that key is checked on every `Hello` and on every HTTP request: a token of a
//! revoked key is invalid, whatever its signature and expiry say. Revoking a key closes every
//! open WebSocket session of that key.
//!
//! [`KeyStatusBoard`] is the one place of a server process that does both:
//!
//! * [`KeyStatusBoard::check`] decides whether a verified token may be used for one HTTP
//!   request. [`KeyCheckedVerifier`] wraps it as a [`TokenVerifier`], so
//!   [`crate::identity::resolve_principal`] rejects the token of a revoked key like any other
//!   invalid token.
//! * [`KeyStatusBoard::open_session`] does the same check for a `Hello` and registers the
//!   session under its key. The registration lives as long as the returned [`SessionGuard`].
//! * [`KeyStatusBoard::revoke`] marks a key as revoked and closes every registered session of
//!   that key with [`CloseReason::KeyRevoked`]. The transport sends `Error Unauthorized`
//!   (ARCHITECTURE 5.4) for it and closes the connection.
//!
//! # Ordering
//!
//! The persistent status of a key comes from a [`KeyStatusSource`] (the key table of the std
//! `User` model), which may be slow. Every check therefore reads the source first and then,
//! in the same synchronous block that registers the session or accepts the request, looks at
//! the revocations this process has seen (ARCHITECTURE 8: the status is checked again after
//! the last `await`). The revoking code must persist the revocation first and call
//! [`KeyStatusBoard::revoke`] afterwards. With that order no `Hello` can slip through:
//!
//! * a `Hello` that reads the source before the revocation is persisted either registers
//!   before [`KeyStatusBoard::revoke`] runs, and is then closed by it, or registers after,
//!   and is then rejected by the second look;
//! * a `Hello` that reads the source after the revocation is persisted is rejected by the
//!   source.
//!
//! Who may revoke which key (a device key only itself, the user key or the operator any
//! device key, D128) is decided before [`KeyStatusBoard::revoke`] is called and is not part
//! of this module. The user key can never be revoked (D73); a source reports it as active.
//!
//! Keys are identified by the pair of user and `kid`, never by the `kid` alone, so a `kid`
//! that happens to occur for two users can never revoke or admit the key of the other user.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use ostrel_core::ids::RowId;

use crate::identity::{TokenRejected, TokenVerifier};

/// Upper bound for the length of a `kid` in bytes, the same as the token layer
/// (`ostrel_auth::MAX_KID_LEN`).
pub const MAX_KEY_ID_LEN: usize = 128;

/// The `kid` of a session token: the id of the key that signed the sign in challenge.
///
/// It is not secret, but it is client supplied text that reaches logs, so it is limited to
/// 1 to [`MAX_KEY_ID_LEN`] bytes of printable ASCII without spaces.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct KeyId(String);

impl KeyId {
    /// The key id `kid`, or `None` when it is empty, too long or contains a byte outside
    /// printable ASCII (`!` to `~`).
    pub fn new(kid: &str) -> Option<Self> {
        let ok = !kid.is_empty()
            && kid.len() <= MAX_KEY_ID_LEN
            && kid.bytes().all(|b| b.is_ascii_graphic());
        ok.then(|| KeyId(kid.to_owned()))
    }

    /// The key id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyId({})", self.0)
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a token says about its user once signature, assertion and claims are verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionToken {
    /// Claim `sub`: the user.
    pub user: RowId,
    /// Claim `kid`: the key that signed the sign in challenge.
    pub kid: KeyId,
}

/// Verifies a session token (signature, implicit assertion, claims) without looking at the
/// status of its key. [`KeyCheckedVerifier`] adds the key status.
pub trait SessionTokenVerifier {
    /// The user and key of `token`, or why the token is not accepted.
    fn verify_session(&self, token: &str) -> Result<SessionToken, TokenRejected>;
}

/// Persistent status of a key of a user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStatus {
    /// The key may sign in. The user key is always active.
    Active,
    /// The key was revoked. Its tokens are invalid.
    Revoked,
    /// The user has no key with this id. Its tokens are invalid (fail closed).
    Unknown,
}

/// Where the persistent status of keys is read, for example the key table of the std
/// `User` model.
pub trait KeyStatusSource {
    /// The status of the key `kid` of `user`.
    fn status(&self, user: RowId, kid: &KeyId) -> KeyStatus;
}

/// Why the server closes a session from outside its own message flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// The key of the session's token was revoked (D102). The transport sends
    /// `Error Unauthorized` and closes the connection.
    KeyRevoked,
}

/// Closes one open session. Implemented by the WebSocket transport.
///
/// [`KeyStatusBoard::revoke`] calls it after it has released its own lock, so an
/// implementation may drop the session's [`SessionGuard`] or call back into the board.
pub trait SessionCloser: Send + Sync {
    /// Ends the session with `reason`. Called at most once per registration.
    fn close(&self, reason: CloseReason);
}

/// Text of every rejection because of the key status. It names neither the token nor the
/// key, so it can go to the client and to the log alike.
const REVOKED: &str = "the key of this token is revoked";
const UNKNOWN: &str = "the key of this token is unknown";

type KeyOf = (RowId, KeyId);

#[derive(Default)]
struct State {
    /// Keys this process has seen revoked. Revocations are rare and keys are never
    /// unrevoked, so the set only grows, bounded by the number of keys.
    revoked: HashSet<KeyOf>,
    /// Open sessions per key. A key without sessions has no entry.
    sessions: HashMap<KeyOf, HashMap<u64, Arc<dyn SessionCloser>>>,
    /// Id of the next registration, unique within the process.
    next_session: u64,
}

/// The key status of one server process: checks tokens, tracks open sessions per key and
/// closes them on revocation. See the module documentation for the required order.
pub struct KeyStatusBoard<S> {
    source: S,
    state: Arc<Mutex<State>>,
}

impl<S: KeyStatusSource> KeyStatusBoard<S> {
    /// A board that reads persistent key status from `source`.
    pub fn new(source: S) -> Self {
        KeyStatusBoard {
            source,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// Whether `token` may be used for one HTTP request.
    pub fn check(&self, token: &SessionToken) -> Result<(), TokenRejected> {
        self.check_source(token)?;
        let state = self.lock();
        Self::check_revoked(&state, token)
    }

    /// Checks `token` for a `Hello` and registers the session under its key. Revoking the
    /// key later calls `closer` once. The registration ends when the guard is dropped.
    pub fn open_session(
        &self,
        token: &SessionToken,
        closer: Arc<dyn SessionCloser>,
    ) -> Result<SessionGuard, TokenRejected> {
        self.check_source(token)?;
        // The second look and the registration happen under one lock, so a revocation is
        // either seen here or sees this session (module documentation, Ordering).
        let mut state = self.lock();
        Self::check_revoked(&state, token)?;
        let id = state.next_session;
        // 2^64 registrations do not happen in the life of a process; wrapping keeps the
        // counter total without a panic path.
        state.next_session = id.wrapping_add(1);
        let key = (token.user, token.kid.clone());
        state
            .sessions
            .entry(key.clone())
            .or_default()
            .insert(id, closer);
        Ok(SessionGuard {
            state: Arc::downgrade(&self.state),
            key,
            id,
        })
    }

    /// Marks the key `kid` of `user` as revoked in this process and closes every open session
    /// of that key. Call it after the revocation is persisted. Returns the number of sessions
    /// closed; revoking a key twice closes nothing the second time.
    pub fn revoke(&self, user: RowId, kid: &KeyId) -> usize {
        let key = (user, kid.clone());
        let closers: Vec<Arc<dyn SessionCloser>> = {
            let mut state = self.lock();
            state.revoked.insert(key.clone());
            state
                .sessions
                .remove(&key)
                .map(|open| open.into_values().collect())
                .unwrap_or_default()
        };
        // Outside the lock: a closer may drop its guard or call back into the board.
        for closer in &closers {
            closer.close(CloseReason::KeyRevoked);
        }
        closers.len()
    }

    /// Number of open sessions of the key `kid` of `user`.
    pub fn open_sessions(&self, user: RowId, kid: &KeyId) -> usize {
        self.lock()
            .sessions
            .get(&(user, kid.clone()))
            .map_or(0, HashMap::len)
    }

    fn check_source(&self, token: &SessionToken) -> Result<(), TokenRejected> {
        match self.source.status(token.user, &token.kid) {
            KeyStatus::Active => Ok(()),
            KeyStatus::Revoked => Err(TokenRejected(REVOKED.to_owned())),
            KeyStatus::Unknown => Err(TokenRejected(UNKNOWN.to_owned())),
        }
    }

    fn check_revoked(state: &State, token: &SessionToken) -> Result<(), TokenRejected> {
        if state.revoked.contains(&(token.user, token.kid.clone())) {
            Err(TokenRejected(REVOKED.to_owned()))
        } else {
            Ok(())
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }
}

impl<S> fmt::Debug for KeyStatusBoard<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyStatusBoard").finish_non_exhaustive()
    }
}

/// Every update of [`State`] is a single insert or remove, so the state stays consistent
/// even if a thread panicked while holding the lock; the board keeps working instead of
/// failing every later request.
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The registration of one open session. Dropping it removes the registration; after a
/// revocation it is already gone and dropping does nothing.
pub struct SessionGuard {
    state: Weak<Mutex<State>>,
    key: KeyOf,
    id: u64,
}

impl fmt::Debug for SessionGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionGuard")
            .field("kid", &self.key.1)
            .field("id", &self.id)
            .finish()
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let mut state = lock(&state);
        if let Some(open) = state.sessions.get_mut(&self.key) {
            open.remove(&self.id);
            if open.is_empty() {
                state.sessions.remove(&self.key);
            }
        }
    }
}

/// A [`TokenVerifier`] for HTTP requests: verifies the token, then the status of its key.
pub struct KeyCheckedVerifier<'a, V, S> {
    tokens: &'a V,
    board: &'a KeyStatusBoard<S>,
}

impl<'a, V, S> KeyCheckedVerifier<'a, V, S> {
    /// Checks tokens with `tokens` and key status with `board`.
    pub fn new(tokens: &'a V, board: &'a KeyStatusBoard<S>) -> Self {
        KeyCheckedVerifier { tokens, board }
    }
}

impl<V: SessionTokenVerifier, S: KeyStatusSource> TokenVerifier for KeyCheckedVerifier<'_, V, S> {
    fn verify(&self, token: &str) -> Result<RowId, TokenRejected> {
        let session = self.tokens.verify_session(token)?;
        self.board.check(&session)?;
        Ok(session.user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Credentials, IdentityError, resolve_principal};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn user(n: u8) -> RowId {
        RowId::from_hex(&format!("{n:032x}")).unwrap_or_else(|_| unreachable!("valid hex"))
    }

    fn kid(text: &str) -> KeyId {
        KeyId::new(text).unwrap_or_else(|| unreachable!("valid kid"))
    }

    fn token(n: u8, key: &str) -> SessionToken {
        SessionToken {
            user: user(n),
            kid: kid(key),
        }
    }

    /// Persistent key table: listed keys are active, `revoked` ones revoked, all others
    /// unknown.
    #[derive(Default)]
    struct Table {
        active: Mutex<HashSet<KeyOf>>,
        revoked: Mutex<HashSet<KeyOf>>,
    }

    impl Table {
        fn with(keys: &[(u8, &str)]) -> Self {
            let table = Table::default();
            for (n, key) in keys {
                lock_set(&table.active).insert((user(*n), kid(key)));
            }
            table
        }

        fn persist_revocation(&self, n: u8, key: &str) {
            lock_set(&self.active).remove(&(user(n), kid(key)));
            lock_set(&self.revoked).insert((user(n), kid(key)));
        }
    }

    fn lock_set(set: &Mutex<HashSet<KeyOf>>) -> MutexGuard<'_, HashSet<KeyOf>> {
        set.lock().unwrap_or_else(PoisonError::into_inner)
    }

    impl KeyStatusSource for Table {
        fn status(&self, user: RowId, kid: &KeyId) -> KeyStatus {
            let key = (user, kid.clone());
            if lock_set(&self.revoked).contains(&key) {
                KeyStatus::Revoked
            } else if lock_set(&self.active).contains(&key) {
                KeyStatus::Active
            } else {
                KeyStatus::Unknown
            }
        }
    }

    impl<T: KeyStatusSource> KeyStatusSource for &T {
        fn status(&self, user: RowId, kid: &KeyId) -> KeyStatus {
            (**self).status(user, kid)
        }
    }

    /// Counts how often it was closed and with which reason.
    #[derive(Default)]
    struct Closer {
        closed: AtomicUsize,
    }

    impl SessionCloser for Closer {
        fn close(&self, reason: CloseReason) {
            assert_eq!(reason, CloseReason::KeyRevoked);
            self.closed.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Closer {
        fn times(&self) -> usize {
            self.closed.load(Ordering::SeqCst)
        }
    }

    /// Tokens of the form `<user>:<kid>`; anything else does not verify.
    struct Tokens;

    impl SessionTokenVerifier for Tokens {
        fn verify_session(&self, token: &str) -> Result<SessionToken, TokenRejected> {
            let bad = || TokenRejected("signature does not verify".to_owned());
            let (n, key) = token.split_once(':').ok_or_else(bad)?;
            let n: u8 = n.parse().map_err(|_| bad())?;
            Ok(SessionToken {
                user: user(n),
                kid: KeyId::new(key).ok_or_else(bad)?,
            })
        }
    }

    #[test]
    fn key_id_bounds() {
        assert!(KeyId::new("").is_none());
        assert!(KeyId::new("a").is_some());
        assert!(KeyId::new(&"k".repeat(MAX_KEY_ID_LEN)).is_some());
        assert!(KeyId::new(&"k".repeat(MAX_KEY_ID_LEN + 1)).is_none());
        for bad in ["a b", "a\tb", "a\nb", "a\u{7f}", "k\u{e9}y", "\u{feff}k"] {
            assert!(KeyId::new(bad).is_none(), "{bad:?}");
        }
        assert_eq!(kid("dev-1").as_str(), "dev-1");
        assert_eq!(format!("{:?}", kid("dev-1")), "KeyId(dev-1)");
    }

    #[test]
    fn active_key_passes_unknown_and_revoked_keys_fail() {
        let table = Table::with(&[(1, "dev-a")]);
        table.persist_revocation(1, "dev-old");
        let board = KeyStatusBoard::new(&table);
        assert_eq!(board.check(&token(1, "dev-a")), Ok(()));
        assert_eq!(
            board.check(&token(1, "dev-old")),
            Err(TokenRejected(REVOKED.to_owned()))
        );
        assert_eq!(
            board.check(&token(1, "dev-x")),
            Err(TokenRejected(UNKNOWN.to_owned()))
        );
    }

    #[test]
    fn a_key_belongs_to_its_user_only() {
        let table = Table::with(&[(1, "dev-a"), (2, "dev-a")]);
        let board = KeyStatusBoard::new(&table);
        assert!(board.check(&token(3, "dev-a")).is_err());
        let other = Arc::new(Closer::default());
        let _guard = board.open_session(&token(2, "dev-a"), other.clone());
        table.persist_revocation(1, "dev-a");
        assert_eq!(board.revoke(user(1), &kid("dev-a")), 0);
        assert_eq!(other.times(), 0);
        assert_eq!(board.check(&token(2, "dev-a")), Ok(()));
        assert!(board.check(&token(1, "dev-a")).is_err());
    }

    #[test]
    fn revoked_key_closes_every_open_session() {
        let table = Table::with(&[(1, "dev-a"), (1, "dev-b")]);
        let board = KeyStatusBoard::new(&table);
        let first = Arc::new(Closer::default());
        let second = Arc::new(Closer::default());
        let sibling = Arc::new(Closer::default());
        let g1 = board.open_session(&token(1, "dev-a"), first.clone());
        let g2 = board.open_session(&token(1, "dev-a"), second.clone());
        let g3 = board.open_session(&token(1, "dev-b"), sibling.clone());
        assert!(g1.is_ok() && g2.is_ok() && g3.is_ok());
        assert_eq!(board.open_sessions(user(1), &kid("dev-a")), 2);

        table.persist_revocation(1, "dev-a");
        assert_eq!(board.revoke(user(1), &kid("dev-a")), 2);
        assert_eq!((first.times(), second.times()), (1, 1));
        assert_eq!(sibling.times(), 0, "a revocation never touches other keys");
        assert_eq!(board.open_sessions(user(1), &kid("dev-a")), 0);
        assert_eq!(board.open_sessions(user(1), &kid("dev-b")), 1);

        // Twice is harmless and closes nothing again; dropping the guards afterwards too.
        assert_eq!(board.revoke(user(1), &kid("dev-a")), 0);
        drop((g1, g2));
        assert_eq!((first.times(), second.times()), (1, 1));
        assert_eq!(board.open_sessions(user(1), &kid("dev-b")), 1);
    }

    #[test]
    fn revoked_key_cannot_open_a_session_or_make_a_request() {
        let table = Table::with(&[(1, "dev-a")]);
        let board = KeyStatusBoard::new(&table);
        let token = token(1, "dev-a");
        let early = Arc::new(Closer::default());
        let _early = board.open_session(&token, early.clone());
        table.persist_revocation(1, "dev-a");
        board.revoke(token.user, &token.kid);

        let late = Arc::new(Closer::default());
        assert_eq!(
            board.open_session(&token, late.clone()).map(|_| ()),
            Err(TokenRejected(REVOKED.to_owned()))
        );
        assert_eq!(board.open_sessions(token.user, &token.kid), 0);
        assert_eq!(late.times(), 0);

        let verifier = KeyCheckedVerifier::new(&Tokens, &board);
        let creds = Credentials::anonymous().with_token("1:dev-a".to_owned());
        assert_eq!(
            resolve_principal(&creds, &verifier),
            Err(IdentityError::InvalidToken(TokenRejected(
                REVOKED.to_owned()
            )))
        );
    }

    /// The revocation is persisted while a `Hello` has already read the source as active.
    /// The second look under the lock still rejects it.
    #[test]
    fn hello_that_read_the_source_before_the_revocation_is_rejected() {
        /// Reports active once, then asks the board to revoke, like a concurrent revocation
        /// that completes between the source read and the registration.
        struct StaleOnce {
            board: Mutex<Option<Arc<KeyStatusBoard<Table>>>>,
        }
        impl KeyStatusSource for StaleOnce {
            fn status(&self, user: RowId, kid: &KeyId) -> KeyStatus {
                if let Some(board) = self
                    .board
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                {
                    board.source.persist_revocation(1, "dev-a");
                    board.revoke(user, kid);
                }
                KeyStatus::Active
            }
        }
        let other = Arc::new(KeyStatusBoard::new(Table::with(&[(1, "dev-a")])));
        let stale = StaleOnce {
            board: Mutex::new(Some(other.clone())),
        };
        // Same process state: the stale board shares the revocations of `other`.
        let board = KeyStatusBoard {
            source: stale,
            state: other.state.clone(),
        };
        let closer = Arc::new(Closer::default());
        let opened = board.open_session(&token(1, "dev-a"), closer.clone());
        assert!(opened.is_err(), "a revoked key must not open a session");
        assert_eq!(board.open_sessions(user(1), &kid("dev-a")), 0);
        assert_eq!(closer.times(), 0);
    }

    /// Hello and revocation race on many threads: afterwards no session of the key is open
    /// and every session that was opened was closed exactly once.
    #[test]
    fn concurrent_hellos_and_revocation_leave_no_open_session() {
        for _round in 0..50 {
            let table = Table::with(&[(1, "dev-a")]);
            let board = KeyStatusBoard::new(&table);
            let opened = AtomicUsize::new(0);
            let closers: Vec<Arc<Closer>> = (0..8).map(|_| Arc::new(Closer::default())).collect();
            let guards = Mutex::new(Vec::new());
            std::thread::scope(|scope| {
                for closer in &closers {
                    let (board, opened, guards) = (&board, &opened, &guards);
                    scope.spawn(move || {
                        if let Ok(guard) = board.open_session(&token(1, "dev-a"), closer.clone()) {
                            opened.fetch_add(1, Ordering::SeqCst);
                            guards
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push(guard);
                        }
                    });
                }
                scope.spawn(|| {
                    table.persist_revocation(1, "dev-a");
                    board.revoke(user(1), &kid("dev-a"));
                });
            });
            assert_eq!(board.open_sessions(user(1), &kid("dev-a")), 0);
            let closed: usize = closers.iter().map(|c| c.times()).sum();
            assert_eq!(closed, opened.load(Ordering::SeqCst));
            assert!(closers.iter().all(|c| c.times() <= 1));
        }
    }

    #[test]
    fn dropping_the_guard_ends_the_registration() {
        let table = Table::with(&[(1, "dev-a")]);
        let board = KeyStatusBoard::new(&table);
        let closer = Arc::new(Closer::default());
        let guard = board.open_session(&token(1, "dev-a"), closer.clone());
        assert_eq!(board.open_sessions(user(1), &kid("dev-a")), 1);
        drop(guard);
        assert_eq!(board.open_sessions(user(1), &kid("dev-a")), 0);
        assert!(
            lock(&board.state).sessions.is_empty(),
            "a key without sessions keeps no entry"
        );
        table.persist_revocation(1, "dev-a");
        assert_eq!(board.revoke(user(1), &kid("dev-a")), 0);
        assert_eq!(closer.times(), 0);
    }

    #[test]
    fn guard_outliving_the_board_is_harmless() {
        let table = Table::with(&[(1, "dev-a")]);
        let board = KeyStatusBoard::new(&table);
        let guard = board.open_session(&token(1, "dev-a"), Arc::new(Closer::default()));
        drop(board);
        drop(guard);
    }

    /// A closer that drops its own guard inside `close` must not deadlock the board.
    #[test]
    fn closer_may_drop_its_guard_while_being_closed() {
        struct SelfDropping(Mutex<Option<SessionGuard>>);
        impl SessionCloser for SelfDropping {
            fn close(&self, _reason: CloseReason) {
                drop(self.0.lock().unwrap_or_else(PoisonError::into_inner).take());
            }
        }
        let table = Table::with(&[(1, "dev-a")]);
        let board = KeyStatusBoard::new(&table);
        let closer = Arc::new(SelfDropping(Mutex::new(None)));
        let guard = board.open_session(&token(1, "dev-a"), closer.clone());
        *closer.0.lock().unwrap_or_else(PoisonError::into_inner) = guard.ok();
        assert_eq!(board.revoke(user(1), &kid("dev-a")), 1);
        assert_eq!(board.open_sessions(user(1), &kid("dev-a")), 0);
    }

    #[test]
    fn checked_verifier_keeps_the_token_rejection_and_passes_active_keys() {
        let table = Table::with(&[(4, "user-key")]);
        let board = KeyStatusBoard::new(&table);
        let verifier = KeyCheckedVerifier::new(&Tokens, &board);
        assert_eq!(verifier.verify("4:user-key"), Ok(user(4)));
        assert_eq!(
            verifier.verify("forged"),
            Err(TokenRejected("signature does not verify".to_owned()))
        );
        assert_eq!(
            verifier.verify("4:other"),
            Err(TokenRejected(UNKNOWN.to_owned()))
        );
    }
}
