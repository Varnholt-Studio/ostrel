//! Who may revoke which device key, and in which order a revocation takes effect
//! (ARCHITECTURE 8, D73, D128, D153, AC-63 (d)).
//!
//! # Scope
//!
//! * A device key revokes only itself. A request signed by a device key that names any other
//!   key is refused, and that other key stays valid: a stolen device key cannot lock the
//!   owner out of the other devices (D128).
//! * The user key revokes any device key of its own user.
//! * The operator CLI revokes any device key of any user.
//! * The user key itself can never be revoked (D73), not even by the operator.
//! * Keys are always the pair of user and `kid`; a key of one user never acts on a key of
//!   another user, whatever their `kid`s.
//!
//! The key that signs a revocation request must itself be valid: [`revoke_device_key`] runs
//! the same status check as every HTTP request ([`KeyStatusBoard::check`]), so a revoked or
//! unknown key revokes nothing.
//!
//! # Order
//!
//! [`revoke_device_key`] persists the revocation first and only then calls
//! [`KeyStatusBoard::revoke`], which closes the open sessions of the key in this process
//! (module documentation of [`crate::key_status`], D153). If persisting fails, nothing is
//! closed and the key stays valid, so the process never acts on a revocation that a restart
//! would forget.

use std::fmt;

use ostrel_core::ids::RowId;

use crate::key_status::{KeyId, KeyStatusBoard, KeyStatusSource, SessionToken};

/// The kind of a key of a user, as the key table of the std `User` model stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    /// The one user key of the user: the root, never revocable (D73).
    User,
    /// A device key added at registration or by a signed device link.
    Device,
}

/// Who asks for a revocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Revoker {
    /// A request signed with a session token: the user and the key named by its `kid`.
    Key(SessionToken),
    /// The operator's CLI on the server host. It is not reachable over the network.
    Operator,
}

/// The persistent key table a revocation reads and writes, for example the key table of the
/// std `User` model.
pub trait KeyTable {
    /// The kind of the key `kid` of `user`, or `None` when the user has no such key.
    fn kind(&self, user: RowId, kid: &KeyId) -> Option<KeyKind>;

    /// Stores that the key `kid` of `user` is revoked. Storing an already revoked key again
    /// succeeds. Once this returns `Ok`, every later status read reports the key revoked.
    fn persist_revocation(&self, user: RowId, kid: &KeyId) -> Result<(), PersistFailed>;
}

/// The key table could not store a revocation. The text is for the server log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistFailed(pub String);

/// Why a revocation was not carried out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevokeError {
    /// The revoker may not revoke this key (D128): a device key names another key, or a key
    /// names a key of another user, or the signing key is not valid.
    Denied,
    /// The target is the user key, which can never be revoked (D73).
    UserKey,
    /// The user has no key with this `kid`.
    UnknownKey,
    /// The revocation could not be persisted. No session was closed.
    Persist(PersistFailed),
}

impl fmt::Display for RevokeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RevokeError::Denied => f.write_str("not allowed to revoke this key"),
            RevokeError::UserKey => f.write_str("the user key cannot be revoked"),
            RevokeError::UnknownKey => f.write_str("the user has no such key"),
            RevokeError::Persist(why) => write!(f, "could not store the revocation: {}", why.0),
        }
    }
}

impl std::error::Error for RevokeError {}

/// Whether `by` may revoke the key `kid` of `user`, by the rules of the module documentation.
///
/// This reads only the key table. [`revoke_device_key`] also checks the status of the
/// signing key before it calls this.
pub fn authorize(
    keys: &dyn KeyTable,
    by: &Revoker,
    user: RowId,
    kid: &KeyId,
) -> Result<(), RevokeError> {
    match keys.kind(user, kid) {
        None => return Err(RevokeError::UnknownKey),
        Some(KeyKind::User) => return Err(RevokeError::UserKey),
        Some(KeyKind::Device) => {}
    }
    let signer = match by {
        Revoker::Operator => return Ok(()),
        Revoker::Key(signer) => signer,
    };
    if signer.user != user {
        return Err(RevokeError::Denied);
    }
    match keys.kind(signer.user, &signer.kid) {
        Some(KeyKind::User) => Ok(()),
        Some(KeyKind::Device) if signer.kid == *kid => Ok(()),
        Some(KeyKind::Device) | None => Err(RevokeError::Denied),
    }
}

/// Revokes the device key `kid` of `user` on behalf of `by`: checks the signing key and the
/// scope, persists the revocation and then closes the open sessions of the key through
/// `board`. Returns the number of sessions closed.
pub fn revoke_device_key<S: KeyStatusSource>(
    board: &KeyStatusBoard<S>,
    keys: &dyn KeyTable,
    by: &Revoker,
    user: RowId,
    kid: &KeyId,
) -> Result<usize, RevokeError> {
    if let Revoker::Key(signer) = by {
        board.check(signer).map_err(|_| RevokeError::Denied)?;
    }
    authorize(keys, by, user, kid)?;
    keys.persist_revocation(user, kid)
        .map_err(RevokeError::Persist)?;
    Ok(board.revoke(user, kid))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

    use super::*;
    use crate::key_status::{CloseReason, KeyStatus, SessionCloser};

    fn user(n: u8) -> RowId {
        RowId::from_hex(&format!("{n:032x}")).unwrap_or_else(|_| unreachable!("valid hex"))
    }

    fn kid(text: &str) -> KeyId {
        KeyId::new(text).unwrap_or_else(|| unreachable!("valid kid"))
    }

    fn signed(n: u8, key: &str) -> Revoker {
        Revoker::Key(SessionToken {
            user: user(n),
            kid: kid(key),
        })
    }

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What happened, in order: persisted revocations and closed sessions.
    type Log = Arc<Mutex<Vec<String>>>;

    /// Key table and status source in one: user 1 has the user key `root` and the device keys
    /// `dev-a`, `dev-b`; user 2 has `root` and `dev-a`.
    struct Table {
        keys: Mutex<HashMap<(RowId, KeyId), (KeyKind, bool)>>,
        fail_persist: bool,
        log: Log,
    }

    impl Table {
        fn new(log: &Log) -> Self {
            let mut keys = HashMap::new();
            for (n, key, kind) in [
                (1, "root", KeyKind::User),
                (1, "dev-a", KeyKind::Device),
                (1, "dev-b", KeyKind::Device),
                (2, "root", KeyKind::User),
                (2, "dev-a", KeyKind::Device),
            ] {
                keys.insert((user(n), kid(key)), (kind, false));
            }
            Table {
                keys: Mutex::new(keys),
                fail_persist: false,
                log: Arc::clone(log),
            }
        }

        fn failing(log: &Log) -> Self {
            Table {
                fail_persist: true,
                ..Table::new(log)
            }
        }

        fn is_revoked(&self, n: u8, key: &str) -> bool {
            lock(&self.keys)
                .get(&(user(n), kid(key)))
                .is_some_and(|(_, revoked)| *revoked)
        }
    }

    impl KeyTable for Table {
        fn kind(&self, user: RowId, kid: &KeyId) -> Option<KeyKind> {
            lock(&self.keys)
                .get(&(user, kid.clone()))
                .map(|(kind, _)| *kind)
        }

        fn persist_revocation(&self, user: RowId, kid: &KeyId) -> Result<(), PersistFailed> {
            if self.fail_persist {
                return Err(PersistFailed("disk full".to_owned()));
            }
            if let Some((_, revoked)) = lock(&self.keys).get_mut(&(user, kid.clone())) {
                *revoked = true;
            }
            lock(&self.log).push(format!("persist {kid}"));
            Ok(())
        }
    }

    impl KeyStatusSource for &Table {
        fn status(&self, user: RowId, kid: &KeyId) -> KeyStatus {
            match lock(&self.keys).get(&(user, kid.clone())) {
                None => KeyStatus::Unknown,
                Some((_, true)) => KeyStatus::Revoked,
                Some((_, false)) => KeyStatus::Active,
            }
        }
    }

    /// Records its close in the shared log, after reading whether the revocation was
    /// already persisted at that moment.
    struct Closer {
        name: &'static str,
        table: Arc<Table>,
        user: u8,
        key: &'static str,
    }

    impl SessionCloser for Closer {
        fn close(&self, reason: CloseReason) {
            assert_eq!(reason, CloseReason::KeyRevoked);
            let persisted = self.table.is_revoked(self.user, self.key);
            lock(&self.table.log).push(format!("close {} persisted={persisted}", self.name));
        }
    }

    fn open(
        board: &KeyStatusBoard<&Table>,
        table: &Arc<Table>,
        name: &'static str,
        n: u8,
        key: &'static str,
    ) -> crate::key_status::SessionGuard {
        let closer = Closer {
            name,
            table: Arc::clone(table),
            user: n,
            key,
        };
        board
            .open_session(
                &SessionToken {
                    user: user(n),
                    kid: kid(key),
                },
                Arc::new(closer),
            )
            .unwrap_or_else(|_| unreachable!("active key opens a session"))
    }

    fn log_of(log: &Log) -> Vec<String> {
        lock(log).clone()
    }

    #[test]
    fn device_key_revokes_itself() {
        let log = Log::default();
        let table = Arc::new(Table::new(&log));
        let board = KeyStatusBoard::new(&*table);
        let _s = open(&board, &table, "a1", 1, "dev-a");
        let closed =
            revoke_device_key(&board, &*table, &signed(1, "dev-a"), user(1), &kid("dev-a"));
        assert_eq!(closed, Ok(1));
        assert!(table.is_revoked(1, "dev-a"));
    }

    #[test]
    fn device_key_cannot_revoke_a_sibling() {
        // D128: a revocation of another device key signed by a device key is rejected and
        // that key stays valid: it keeps its session and can still sign in.
        let log = Log::default();
        let table = Arc::new(Table::new(&log));
        let board = KeyStatusBoard::new(&*table);
        let _s = open(&board, &table, "b1", 1, "dev-b");
        let result =
            revoke_device_key(&board, &*table, &signed(1, "dev-a"), user(1), &kid("dev-b"));
        assert_eq!(result, Err(RevokeError::Denied));
        assert!(!table.is_revoked(1, "dev-b"));
        assert_eq!(board.open_sessions(user(1), &kid("dev-b")), 1);
        let token = SessionToken {
            user: user(1),
            kid: kid("dev-b"),
        };
        assert!(board.check(&token).is_ok(), "the sibling stays valid");
        assert!(log_of(&log).is_empty(), "nothing persisted, nothing closed");
    }

    #[test]
    fn user_key_and_operator_revoke_any_device_key_of_the_user() {
        let log = Log::default();
        let table = Arc::new(Table::new(&log));
        let board = KeyStatusBoard::new(&*table);
        let _s = open(&board, &table, "b1", 1, "dev-b");
        assert_eq!(
            revoke_device_key(&board, &*table, &signed(1, "root"), user(1), &kid("dev-b")),
            Ok(1)
        );
        assert_eq!(
            revoke_device_key(&board, &*table, &Revoker::Operator, user(2), &kid("dev-a")),
            Ok(0)
        );
        assert!(table.is_revoked(1, "dev-b"));
        assert!(table.is_revoked(2, "dev-a"));
        assert!(!table.is_revoked(1, "dev-a"));
    }

    #[test]
    fn no_key_acts_on_another_user() {
        let log = Log::default();
        let table = Table::new(&log);
        // Same kid `dev-a` for both users, and the user key of user 2: none may touch user 1.
        for by in [signed(2, "dev-a"), signed(2, "root")] {
            assert_eq!(
                authorize(&table, &by, user(1), &kid("dev-a")),
                Err(RevokeError::Denied),
                "{by:?}"
            );
        }
    }

    #[test]
    fn user_key_is_never_revoked() {
        let log = Log::default();
        let table = Arc::new(Table::new(&log));
        let board = KeyStatusBoard::new(&*table);
        for by in [signed(1, "root"), signed(1, "dev-a"), Revoker::Operator] {
            assert_eq!(
                revoke_device_key(&board, &*table, &by, user(1), &kid("root")),
                Err(RevokeError::UserKey),
                "{by:?}"
            );
        }
        assert!(!table.is_revoked(1, "root"));
    }

    #[test]
    fn unknown_target_is_refused_and_unknown_signer_denied() {
        let log = Log::default();
        let table = Arc::new(Table::new(&log));
        let board = KeyStatusBoard::new(&*table);
        assert_eq!(
            revoke_device_key(&board, &*table, &Revoker::Operator, user(1), &kid("dev-z")),
            Err(RevokeError::UnknownKey)
        );
        assert_eq!(
            revoke_device_key(&board, &*table, &signed(1, "dev-z"), user(1), &kid("dev-a")),
            Err(RevokeError::Denied)
        );
        // Through the scope check alone an unknown signer is denied too.
        assert_eq!(
            authorize(&*table, &signed(1, "dev-z"), user(1), &kid("dev-a")),
            Err(RevokeError::Denied)
        );
    }

    #[test]
    fn revoked_signing_key_revokes_nothing() {
        let log = Log::default();
        let table = Arc::new(Table::new(&log));
        let board = KeyStatusBoard::new(&*table);
        assert_eq!(
            revoke_device_key(&board, &*table, &signed(1, "dev-a"), user(1), &kid("dev-a")),
            Ok(0)
        );
        // dev-a is revoked now; its token may not even revoke itself again.
        assert_eq!(
            revoke_device_key(&board, &*table, &signed(1, "dev-a"), user(1), &kid("dev-a")),
            Err(RevokeError::Denied)
        );
    }

    #[test]
    fn revocation_is_persisted_before_sessions_close() {
        // uma A2 #2169: persist first, then KeyStatusBoard::revoke.
        let log = Log::default();
        let table = Arc::new(Table::new(&log));
        let board = KeyStatusBoard::new(&*table);
        let _s1 = open(&board, &table, "a1", 1, "dev-a");
        let _s2 = open(&board, &table, "a2", 1, "dev-a");
        assert_eq!(
            revoke_device_key(&board, &*table, &signed(1, "root"), user(1), &kid("dev-a")),
            Ok(2)
        );
        let events = log_of(&log);
        assert_eq!(events.first().map(String::as_str), Some("persist dev-a"));
        assert_eq!(events.len(), 3);
        for event in events.iter().skip(1) {
            assert!(
                event.starts_with("close ") && event.ends_with("persisted=true"),
                "{event}"
            );
        }
    }

    #[test]
    fn failed_persist_closes_nothing_and_key_stays_valid() {
        let log = Log::default();
        let table = Arc::new(Table::failing(&log));
        let board = KeyStatusBoard::new(&*table);
        let _s = open(&board, &table, "a1", 1, "dev-a");
        let result = revoke_device_key(&board, &*table, &signed(1, "root"), user(1), &kid("dev-a"));
        assert_eq!(
            result,
            Err(RevokeError::Persist(PersistFailed("disk full".to_owned())))
        );
        assert_eq!(board.open_sessions(user(1), &kid("dev-a")), 1);
        let token = SessionToken {
            user: user(1),
            kid: kid("dev-a"),
        };
        assert!(board.check(&token).is_ok());
        assert!(log_of(&log).is_empty());
    }

    #[test]
    fn errors_name_no_key() {
        for (error, text) in [
            (RevokeError::Denied, "not allowed to revoke this key"),
            (RevokeError::UserKey, "the user key cannot be revoked"),
            (RevokeError::UnknownKey, "the user has no such key"),
            (
                RevokeError::Persist(PersistFailed("disk full".to_owned())),
                "could not store the revocation: disk full",
            ),
        ] {
            assert_eq!(error.to_string(), text);
        }
    }
}
