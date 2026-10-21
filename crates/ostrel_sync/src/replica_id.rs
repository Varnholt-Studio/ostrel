//! Which replica a connection works as: the server's answer to [`Hello`] (ARCHITECTURE 5.3,
//! D103, D111, D122).
//!
//! # Rules
//!
//! * A [`Hello`] without a replica always gets a new random [`ReplicaId`], also when the same
//!   user and device key already own replicas (D103). Two browsers that sign in with the same
//!   imported user key are such a pair: if the second one got the first one's replica, both
//!   would count `seq` from 1 and the server would acknowledge the second browser's ops as
//!   replays without applying them. A new id is never one that was issued before, so the op
//!   log can never mix the ops of two replicas.
//! * A [`Hello`] that names a replica is confirmed only for its owner. In an app with `auth`
//!   the owner is the token's user and device key. In an app without `auth` it is whoever
//!   knows the `replica_secret` that came with the replica; the server keeps only its SHA-256
//!   (D111). Every mismatch is a fatal [`ErrorCode::Unauthorized`].
//! * A `replica_secret` in an app with `auth`, or one without a replica, is a fatal
//!   [`ErrorCode::Protocol`] (D122). So is a `token` in an app without `auth`, also an empty
//!   one (D150). Both are checked before every replica rule, so such a [`Hello`] never learns
//!   whether a replica exists.
//! * A replica unused for [`REPLICA_LIFETIME_MS`] has expired: [`ErrorCode::ReplicaExpired`]
//!   (ARCHITECTURE 5.5, D111). It is reported only to the owner, so nobody learns from it that
//!   a foreign replica exists.
//! * A confirmed replica whose `last_server_seq` lies above the end of the op log is
//!   [`ErrorCode::ReplicaExpired`] as well (ARCHITECTURE 5.4, D161): the log it was applied
//!   against is gone, for example a memory database after a restart. The client then drops the
//!   replica's state and outbox instead of dropping new ops below its mark. This check sees only
//!   a mark above the log end; a restart with new ops above the mark is not detected (D162).
//!   [`check_resume_mark`] applies it after [`answer_hello`].
//!
//! # What this module leaves to its caller
//!
//! It is pure: no clock, no randomness, no hashing and no storage of its own, so every rule
//! above is testable without a server. The caller verifies the token first (ARCHITECTURE 8)
//! and passes the result as [`Caller`]. It supplies the current time, an [`Entropy`] source
//! (operating system randomness in the server) and the SHA-256 function as `digest`. After an
//! [`Answer::Issued`] it stores the new [`ReplicaRecord`] before it sends [`Welcome`], so a
//! client never holds a replica the server does not know.
//!
//! Not covered here: rejecting an op whose [`OpId`](crate::protocol::OpId) is known with other
//! content. D103 (b) put that off until after v0.3; within v0.3, fresh ids per [`Hello`] keep
//! honest clients from ever reusing an op id.

use crate::protocol::{ErrorCode, Hello, ReplicaId, RowId, ServerSeq};

/// A replica unused this long has expired: 30 days in milliseconds (ARCHITECTURE 5.5).
pub const REPLICA_LIFETIME_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// Bytes of randomness in a `replica_secret` (128 bits, D111).
pub const SECRET_BYTES: usize = 16;

/// Attempts to draw an unused [`ReplicaId`] before giving up. With 64 random bits a single
/// collision is already improbable; several in a row mean the entropy source is broken.
pub const ISSUE_ATTEMPTS: usize = 8;

/// Who the connection is, as far as the server knows before it looks at replicas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller<'a> {
    /// App with `auth` and a verified token: its user (`sub`) and device key (`kid`).
    Signed { user: RowId, device_key: &'a str },
    /// App with `auth`, but the [`Hello`] carried no token or one that failed verification.
    Unsigned,
    /// App without `auth`. Replicas are bound by their secret.
    NoAuth,
}

/// What a replica is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// App with `auth`: the user and device key that received the replica.
    Device { user: RowId, device_key: String },
    /// App without `auth`: SHA-256 of the `replica_secret` issued with the replica.
    Secret { digest: [u8; 32] },
}

/// The server's record of one replica (ARCHITECTURE 5.3, without `last_seq`, which the
/// database keeps next to the op outcomes, D120).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaRecord {
    pub replica: ReplicaId,
    pub owner: Owner,
    /// Server time in ms since the epoch when the replica was issued.
    pub issued_ms: u64,
    /// Server time in ms of the last acknowledged op, `None` before the first one.
    pub last_ack_ms: Option<u64>,
}

impl ReplicaRecord {
    /// True when the replica was unused for at least [`REPLICA_LIFETIME_MS`] at `now_ms`.
    /// Use is the last acknowledged op, or the issue when there is none.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        let last_use = self
            .last_ack_ms
            .unwrap_or(self.issued_ms)
            .max(self.issued_ms);
        now_ms.saturating_sub(last_use) >= REPLICA_LIFETIME_MS
    }
}

/// The replicas the server knows.
pub trait ReplicaRecords {
    /// The record of `replica`, if it is live.
    fn find(&self, replica: ReplicaId) -> Option<&ReplicaRecord>;

    /// True when `replica` was ever issued, also when its record was removed since. Ops of a
    /// removed replica can still be in the log, so its id must never be issued again.
    fn was_issued(&self, replica: ReplicaId) -> bool;
}

/// Source of randomness for new replicas.
pub trait Entropy {
    /// A uniformly random 64 bit number.
    fn next_u64(&mut self) -> u64;

    /// [`SECRET_BYTES`] uniformly random bytes.
    fn secret(&mut self) -> [u8; SECRET_BYTES];
}

/// The answer to a [`Hello`] that is not refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// The named replica belongs to the caller and stays in use.
    Confirmed(ReplicaId),
    /// A new replica. The caller stores `record`, then sends [`Welcome`] with its id and, in an
    /// app without `auth`, with `secret`.
    Issued {
        record: ReplicaRecord,
        /// The `replica_secret` for [`Welcome`]: 32 lowercase hex digits, only in an app
        /// without `auth`. It is never stored and never logged.
        secret: Option<String>,
    },
}

/// Why a [`Hello`] is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Fatal protocol error for the client.
    Error(ErrorCode),
    /// [`ISSUE_ATTEMPTS`] random ids in a row were already issued. This is a server defect
    /// (broken entropy source), not a client error.
    EntropyExhausted,
}

/// Decides which replica the connection that sent `hello` works as.
///
/// `digest` is SHA-256. See the module documentation for the rules.
pub fn answer_hello(
    hello: &Hello,
    caller: Caller<'_>,
    records: &impl ReplicaRecords,
    entropy: &mut impl Entropy,
    digest: impl Fn(&[u8]) -> [u8; 32],
    now_ms: u64,
) -> Result<Answer, Refusal> {
    if hello.token.is_some() && caller == Caller::NoAuth {
        return Err(Refusal::Error(ErrorCode::Protocol));
    }
    if hello.replica_secret.is_some() && (hello.replica.is_none() || caller != Caller::NoAuth) {
        return Err(Refusal::Error(ErrorCode::Protocol));
    }
    if caller == Caller::Unsigned {
        return Err(Refusal::Error(ErrorCode::Unauthorized));
    }
    match hello.replica {
        Some(replica) => confirm(hello, replica, caller, records, digest, now_ms),
        None => issue(caller, records, entropy, digest, now_ms),
    }
}

/// Checks the `last_server_seq` of `hello` against `log_end`, the highest position of the op
/// log, after [`answer_hello`] gave `answer`.
///
/// Only a [`Answer::Confirmed`] replica is checked: its owner is established, so the refusal
/// tells nobody else anything. A new replica has applied nothing, so its mark is ignored.
pub fn check_resume_mark(
    answer: &Answer,
    hello: &Hello,
    log_end: ServerSeq,
) -> Result<(), Refusal> {
    match (answer, hello.last_server_seq) {
        (Answer::Confirmed(_), Some(mark)) if mark > log_end => {
            Err(Refusal::Error(ErrorCode::ReplicaExpired))
        }
        _ => Ok(()),
    }
}

/// Confirms a named replica for its owner.
fn confirm(
    hello: &Hello,
    replica: ReplicaId,
    caller: Caller<'_>,
    records: &impl ReplicaRecords,
    digest: impl Fn(&[u8]) -> [u8; 32],
    now_ms: u64,
) -> Result<Answer, Refusal> {
    let unauthorized = Err(Refusal::Error(ErrorCode::Unauthorized));
    let Some(record) = records.find(replica) else {
        return unauthorized;
    };
    let owned = match (&record.owner, caller) {
        (
            Owner::Device { user, device_key },
            Caller::Signed {
                user: u,
                device_key: k,
            },
        ) => *user == u && device_key == k,
        (Owner::Secret { digest: stored }, Caller::NoAuth) => {
            match hello.replica_secret.as_deref().and_then(parse_secret) {
                Some(secret) => same_digest(&digest(&secret), stored),
                None => false,
            }
        }
        // A record of the other app mode, for example after `auth` was added to the app.
        _ => false,
    };
    if !owned {
        return unauthorized;
    }
    if record.is_expired(now_ms) {
        return Err(Refusal::Error(ErrorCode::ReplicaExpired));
    }
    Ok(Answer::Confirmed(replica))
}

/// Issues a new replica that was never issued before.
fn issue(
    caller: Caller<'_>,
    records: &impl ReplicaRecords,
    entropy: &mut impl Entropy,
    digest: impl Fn(&[u8]) -> [u8; 32],
    now_ms: u64,
) -> Result<Answer, Refusal> {
    let replica = (0..ISSUE_ATTEMPTS)
        .map(|_| ReplicaId(entropy.next_u64()))
        .find(|id| !records.was_issued(*id))
        .ok_or(Refusal::EntropyExhausted)?;
    let (owner, secret) = match caller {
        Caller::Signed { user, device_key } => (
            Owner::Device {
                user,
                device_key: device_key.to_owned(),
            },
            None,
        ),
        Caller::NoAuth => {
            let secret = entropy.secret();
            let owner = Owner::Secret {
                digest: digest(&secret),
            };
            (owner, Some(to_hex(&secret)))
        }
        // `answer_hello` refuses an unsigned caller before it issues anything.
        Caller::Unsigned => return Err(Refusal::Error(ErrorCode::Unauthorized)),
    };
    let record = ReplicaRecord {
        replica,
        owner,
        issued_ms: now_ms,
        last_ack_ms: None,
    };
    Ok(Answer::Issued { record, secret })
}

/// Reads a `replica_secret` in its only valid form, 32 lowercase hex digits.
fn parse_secret(text: &str) -> Option<[u8; SECRET_BYTES]> {
    let digits = text.as_bytes();
    if digits.len() != 2 * SECRET_BYTES {
        return None;
    }
    let mut secret = [0u8; SECRET_BYTES];
    for (byte, pair) in secret.iter_mut().zip(digits.chunks_exact(2)) {
        *byte = hex_digit(pair[0])? << 4 | hex_digit(pair[1])?;
    }
    Some(secret)
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Compares two digests in time independent of where they differ, so the answer time does not
/// tell an attacker how much of a guessed secret was right.
fn same_digest(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Hlc, ServerSeq};
    use std::collections::{HashMap, HashSet};

    const NOW: u64 = 1_800_000_000_000;
    const DAY_MS: u64 = 24 * 60 * 60 * 1000;

    /// Stand in for SHA-256: any fixed injective map from 16 byte secrets will do here.
    fn digest(bytes: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, b) in bytes.iter().enumerate() {
            out[i] = *b;
            out[31 - i] ^= b.wrapping_mul(31).wrapping_add(7);
        }
        out
    }

    /// Replays a fixed list of ids. The first secret is all `0xab`; each later one has its
    /// first byte raised by one, so two replicas never share a secret.
    struct Script {
        ids: Vec<u64>,
        secret: [u8; SECRET_BYTES],
    }

    impl Script {
        fn ids(ids: &[u64]) -> Self {
            Self {
                ids: ids.iter().rev().copied().collect(),
                secret: [0xab; SECRET_BYTES],
            }
        }
    }

    impl Entropy for Script {
        fn next_u64(&mut self) -> u64 {
            self.ids.pop().expect("test drew more ids than scripted")
        }

        fn secret(&mut self) -> [u8; SECRET_BYTES] {
            let secret = self.secret;
            self.secret[0] = self.secret[0].wrapping_add(1);
            secret
        }
    }

    #[derive(Default)]
    struct Store {
        live: HashMap<ReplicaId, ReplicaRecord>,
        removed: HashSet<ReplicaId>,
    }

    impl Store {
        fn keep(&mut self, answer: &Answer) -> ReplicaId {
            match answer {
                Answer::Issued { record, .. } => {
                    self.live.insert(record.replica, record.clone());
                    record.replica
                }
                Answer::Confirmed(replica) => *replica,
            }
        }
    }

    impl ReplicaRecords for Store {
        fn find(&self, replica: ReplicaId) -> Option<&ReplicaRecord> {
            self.live.get(&replica)
        }

        fn was_issued(&self, replica: ReplicaId) -> bool {
            self.live.contains_key(&replica) || self.removed.contains(&replica)
        }
    }

    fn user() -> RowId {
        RowId::new(Hlc::new(1, 0, ReplicaId(9)).expect("valid clock"))
    }

    fn signed(device_key: &str) -> Caller<'_> {
        Caller::Signed {
            user: user(),
            device_key,
        }
    }

    fn hello(replica: Option<u64>, secret: Option<&str>) -> Hello {
        Hello {
            token: None,
            replica: replica.map(ReplicaId),
            replica_secret: secret.map(str::to_owned),
            last_server_seq: Some(ServerSeq(0)),
        }
    }

    fn answer(
        hello: &Hello,
        caller: Caller<'_>,
        store: &Store,
        entropy: &mut Script,
        now_ms: u64,
    ) -> Result<Answer, Refusal> {
        answer_hello(hello, caller, store, entropy, digest, now_ms)
    }

    fn refused(code: ErrorCode) -> Result<Answer, Refusal> {
        Err(Refusal::Error(code))
    }

    #[test]
    fn two_browsers_with_the_same_user_key_get_different_replicas() {
        // Both browsers imported the same user key and sign in with it, so caller is equal.
        let mut store = Store::default();
        let mut entropy = Script::ids(&[0x11, 0x22]);
        let first = answer(
            &hello(None, None),
            signed("user-key"),
            &store,
            &mut entropy,
            NOW,
        );
        let first = store.keep(&first.expect("first browser gets a replica"));
        let second = answer(
            &hello(None, None),
            signed("user-key"),
            &store,
            &mut entropy,
            NOW,
        );
        let second = store.keep(&second.expect("second browser gets a replica"));
        assert_eq!(first, ReplicaId(0x11));
        assert_eq!(second, ReplicaId(0x22));
        assert_eq!(store.live.len(), 2, "one pair owns both replicas");
    }

    #[test]
    fn a_new_replica_skips_ids_that_are_live_or_were_removed() {
        let mut store = Store::default();
        let mut entropy = Script::ids(&[0x11]);
        store.keep(&answer(&hello(None, None), signed("k"), &store, &mut entropy, NOW).unwrap());
        store.removed.insert(ReplicaId(0x22));
        let mut entropy = Script::ids(&[0x11, 0x22, 0x33]);
        let issued = answer(&hello(None, None), signed("k"), &store, &mut entropy, NOW);
        assert_eq!(store.keep(&issued.unwrap()), ReplicaId(0x33));
    }

    #[test]
    fn a_broken_entropy_source_is_a_server_defect() {
        let mut store = Store::default();
        let mut entropy = Script::ids(&[7]);
        store.keep(&answer(&hello(None, None), signed("k"), &store, &mut entropy, NOW).unwrap());
        let mut entropy = Script::ids(&[7; ISSUE_ATTEMPTS]);
        let result = answer(&hello(None, None), signed("k"), &store, &mut entropy, NOW);
        assert_eq!(result, Err(Refusal::EntropyExhausted));
    }

    #[test]
    fn a_new_replica_is_bound_to_the_token_user_and_device_key() {
        let mut entropy = Script::ids(&[5]);
        let result = answer(
            &hello(None, None),
            signed("k1"),
            &Store::default(),
            &mut entropy,
            NOW,
        );
        let expected = ReplicaRecord {
            replica: ReplicaId(5),
            owner: Owner::Device {
                user: user(),
                device_key: "k1".to_owned(),
            },
            issued_ms: NOW,
            last_ack_ms: None,
        };
        assert_eq!(
            result,
            Ok(Answer::Issued {
                record: expected,
                secret: None
            })
        );
    }

    #[test]
    fn a_replica_is_confirmed_only_for_its_user_and_device_key() {
        let mut store = Store::default();
        let mut entropy = Script::ids(&[5]);
        store.keep(&answer(&hello(None, None), signed("k1"), &store, &mut entropy, NOW).unwrap());
        let mut entropy = Script::ids(&[]);
        let back = answer(
            &hello(Some(5), None),
            signed("k1"),
            &store,
            &mut entropy,
            NOW,
        );
        assert_eq!(back, Ok(Answer::Confirmed(ReplicaId(5))));
        let other_key = answer(
            &hello(Some(5), None),
            signed("k2"),
            &store,
            &mut entropy,
            NOW,
        );
        assert_eq!(other_key, refused(ErrorCode::Unauthorized));
        let other_user = Caller::Signed {
            user: RowId::new(Hlc::new(2, 0, ReplicaId(9)).unwrap()),
            device_key: "k1",
        };
        let other_user = answer(&hello(Some(5), None), other_user, &store, &mut entropy, NOW);
        assert_eq!(other_user, refused(ErrorCode::Unauthorized));
        let unknown = answer(
            &hello(Some(6), None),
            signed("k1"),
            &store,
            &mut entropy,
            NOW,
        );
        assert_eq!(unknown, refused(ErrorCode::Unauthorized));
    }

    #[test]
    fn without_a_valid_token_an_auth_app_gives_no_replica() {
        let mut entropy = Script::ids(&[]);
        let store = Store::default();
        for h in [hello(None, None), hello(Some(5), None)] {
            let result = answer(&h, Caller::Unsigned, &store, &mut entropy, NOW);
            assert_eq!(result, refused(ErrorCode::Unauthorized));
        }
    }

    #[test]
    fn a_new_replica_without_auth_comes_with_a_secret() {
        let mut entropy = Script::ids(&[5]);
        let result = answer(
            &hello(None, None),
            Caller::NoAuth,
            &Store::default(),
            &mut entropy,
            NOW,
        );
        let Ok(Answer::Issued { record, secret }) = result else {
            panic!("expected a new replica, got {result:?}");
        };
        let secret = secret.expect("an app without auth gets a secret");
        assert_eq!(secret, "ab".repeat(SECRET_BYTES));
        assert_eq!(
            record.owner,
            Owner::Secret {
                digest: digest(&[0xab; SECRET_BYTES])
            },
            "the record keeps only the digest"
        );
    }

    #[test]
    fn without_auth_a_replica_needs_its_exact_secret() {
        let mut store = Store::default();
        let mut entropy = Script::ids(&[5]);
        let issued = answer(
            &hello(None, None),
            Caller::NoAuth,
            &store,
            &mut entropy,
            NOW,
        );
        let issued = issued.unwrap();
        let Answer::Issued {
            secret: Some(secret),
            ..
        } = &issued
        else {
            panic!("expected a secret");
        };
        let secret = secret.clone();
        store.keep(&issued);
        let mut entropy = Script::ids(&[]);
        let mut check = |s: Option<&str>| {
            answer(
                &hello(Some(5), s),
                Caller::NoAuth,
                &store,
                &mut entropy,
                NOW,
            )
        };
        assert_eq!(check(Some(&secret)), Ok(Answer::Confirmed(ReplicaId(5))));
        let unauthorized = refused(ErrorCode::Unauthorized);
        assert_eq!(check(None), unauthorized, "missing secret");
        assert_eq!(check(Some(&"ac".repeat(16))), unauthorized, "wrong secret");
        assert_eq!(
            check(Some(&secret.to_uppercase())),
            unauthorized,
            "uppercase"
        );
        assert_eq!(check(Some(&secret[..30])), unauthorized, "too short");
        assert_eq!(
            check(Some(&format!("{secret}00"))),
            unauthorized,
            "too long"
        );
        assert_eq!(
            check(Some(&format!(" {}", &secret[1..]))),
            unauthorized,
            "not hex"
        );
    }

    #[test]
    fn a_foreign_replica_id_taken_from_a_logged_op_is_refused_without_auth() {
        // Any reader sees replica ids in broadcast ops; without the secret the id is useless.
        let mut store = Store::default();
        let mut entropy = Script::ids(&[5, 6]);
        let victim = answer(
            &hello(None, None),
            Caller::NoAuth,
            &store,
            &mut entropy,
            NOW,
        );
        store.keep(&victim.unwrap());
        let attacker = answer(
            &hello(None, None),
            Caller::NoAuth,
            &store,
            &mut entropy,
            NOW,
        );
        let Answer::Issued {
            secret: Some(own_secret),
            ..
        } = store_and_return(&mut store, attacker)
        else {
            panic!("expected a secret");
        };
        let result = answer(
            &hello(Some(5), Some(&own_secret)),
            Caller::NoAuth,
            &store,
            &mut entropy,
            NOW,
        );
        assert_eq!(result, refused(ErrorCode::Unauthorized));
    }

    fn store_and_return(store: &mut Store, answer: Result<Answer, Refusal>) -> Answer {
        let answer = answer.expect("replica issued");
        store.keep(&answer);
        answer
    }

    #[test]
    fn a_secret_where_none_belongs_is_a_protocol_error() {
        let mut entropy = Script::ids(&[]);
        let store = Store::default();
        let secret = "ab".repeat(SECRET_BYTES);
        let cases = [
            (hello(None, Some(&secret)), Caller::NoAuth),
            (hello(Some(5), Some(&secret)), signed("k")),
            (hello(None, Some(&secret)), signed("k")),
            (hello(Some(5), Some(&secret)), Caller::Unsigned),
        ];
        for (h, caller) in cases {
            let result = answer(&h, caller, &store, &mut entropy, NOW);
            assert_eq!(result, refused(ErrorCode::Protocol), "{caller:?}");
        }
    }

    #[test]
    fn a_record_of_the_other_app_mode_is_never_confirmed() {
        let mut store = Store::default();
        let mut entropy = Script::ids(&[5, 6]);
        store.keep(&answer(&hello(None, None), signed("k"), &store, &mut entropy, NOW).unwrap());
        let no_auth = answer(
            &hello(None, None),
            Caller::NoAuth,
            &store,
            &mut entropy,
            NOW,
        );
        let Answer::Issued {
            secret: Some(secret),
            ..
        } = store_and_return(&mut store, no_auth)
        else {
            panic!("expected a secret");
        };
        let mut entropy = Script::ids(&[]);
        let as_no_auth = answer(
            &hello(Some(5), Some(&secret)),
            Caller::NoAuth,
            &store,
            &mut entropy,
            NOW,
        );
        assert_eq!(as_no_auth, refused(ErrorCode::Unauthorized));
        let as_signed = answer(
            &hello(Some(6), None),
            signed("k"),
            &store,
            &mut entropy,
            NOW,
        );
        assert_eq!(as_signed, refused(ErrorCode::Unauthorized));
    }

    #[test]
    fn a_replica_expires_after_thirty_days_without_use() {
        let mut store = Store::default();
        let mut entropy = Script::ids(&[5]);
        store.keep(&answer(&hello(None, None), signed("k"), &store, &mut entropy, NOW).unwrap());
        let mut entropy = Script::ids(&[]);
        let at = |store: &Store, entropy: &mut Script, now| {
            answer(&hello(Some(5), None), signed("k"), store, entropy, now)
        };
        let alive = Ok(Answer::Confirmed(ReplicaId(5)));
        assert_eq!(at(&store, &mut entropy, NOW + 30 * DAY_MS - 1), alive);
        assert_eq!(
            at(&store, &mut entropy, NOW + 30 * DAY_MS),
            refused(ErrorCode::ReplicaExpired)
        );
        // An acknowledged op counts as use.
        store.live.get_mut(&ReplicaId(5)).unwrap().last_ack_ms = Some(NOW + 20 * DAY_MS);
        assert_eq!(at(&store, &mut entropy, NOW + 49 * DAY_MS), alive);
        // Expiry is told only to the owner; anyone else learns nothing about the replica.
        let stranger = answer(
            &hello(Some(5), None),
            signed("other"),
            &store,
            &mut entropy,
            NOW + 60 * DAY_MS,
        );
        assert_eq!(stranger, refused(ErrorCode::Unauthorized));
    }

    /// D150: a token in an app without `auth`, also an empty one, is a protocol error before
    /// any replica rule, so the answer is the same whether the named replica exists or not.
    #[test]
    fn a_token_in_an_app_without_auth_is_a_protocol_error() {
        let mut store = Store::default();
        let mut entropy = Script::ids(&[5]);
        let issued = answer(
            &hello(None, None),
            Caller::NoAuth,
            &store,
            &mut entropy,
            NOW,
        );
        let Answer::Issued {
            secret: Some(secret),
            ..
        } = store_and_return(&mut store, issued)
        else {
            panic!("expected a secret");
        };
        let mut entropy = Script::ids(&[]);
        let with_token = |replica: Option<u64>, secret: Option<&str>, token: &str| Hello {
            token: Some(token.to_owned()),
            ..hello(replica, secret)
        };
        let cases = [
            with_token(None, None, "v4.public.x"),
            with_token(None, None, ""),
            with_token(Some(5), Some(&secret), "v4.public.x"),
            with_token(Some(5), Some(&secret), ""),
            with_token(Some(6), Some(&secret), ""),
            with_token(Some(5), None, ""),
        ];
        for h in cases {
            let result = answer(&h, Caller::NoAuth, &store, &mut entropy, NOW);
            assert_eq!(result, refused(ErrorCode::Protocol), "{h:?}");
        }
        // The same Hello without the token is fine, so the token alone decided.
        let plain = answer(
            &hello(Some(5), Some(&secret)),
            Caller::NoAuth,
            &store,
            &mut entropy,
            NOW,
        );
        assert_eq!(plain, Ok(Answer::Confirmed(ReplicaId(5))));
    }

    /// D161: a confirmed replica whose mark lies above the log end has expired; a mark at or
    /// below it, a missing mark and a new replica pass.
    #[test]
    fn a_mark_above_the_log_end_expires_the_replica() {
        let with_mark = |mark: Option<u64>| Hello {
            last_server_seq: mark.map(ServerSeq),
            ..hello(Some(5), None)
        };
        let confirmed = Answer::Confirmed(ReplicaId(5));
        let end = ServerSeq(10);
        assert_eq!(
            check_resume_mark(&confirmed, &with_mark(Some(10)), end),
            Ok(())
        );
        assert_eq!(
            check_resume_mark(&confirmed, &with_mark(Some(0)), end),
            Ok(())
        );
        assert_eq!(check_resume_mark(&confirmed, &with_mark(None), end), Ok(()));
        assert_eq!(
            check_resume_mark(&confirmed, &with_mark(Some(11)), end),
            Err(Refusal::Error(ErrorCode::ReplicaExpired))
        );
        // A memory database after a restart: the log is empty, every mark lies above it.
        assert_eq!(
            check_resume_mark(&confirmed, &with_mark(Some(1)), ServerSeq(0)),
            Err(Refusal::Error(ErrorCode::ReplicaExpired))
        );
        let issued = Answer::Issued {
            record: ReplicaRecord {
                replica: ReplicaId(6),
                owner: Owner::Secret { digest: [0; 32] },
                issued_ms: NOW,
                last_ack_ms: None,
            },
            secret: None,
        };
        assert_eq!(
            check_resume_mark(&issued, &with_mark(Some(99)), end),
            Ok(())
        );
    }

    #[test]
    fn secrets_round_trip_through_their_hex_form() {
        let secret: [u8; SECRET_BYTES] = std::array::from_fn(|i| (i as u8) * 17);
        assert_eq!(parse_secret(&to_hex(&secret)), Some(secret));
        assert_eq!(to_hex(&[0x00, 0x0f, 0xf0, 0xff]), "000ff0ff");
    }
}
