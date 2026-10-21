//! Broadcast queue of one server process (ARCHITECTURE 5.6, D126).
//!
//! Row writers serialise writes, never sends. Two commits under different row writers can
//! finish in any order, so a commit that holds a later log position may be ready before an
//! earlier one. If the later commit were sent at once, its `see` filter would run against a
//! state that misses the earlier commit: a revocation at position k would not yet filter a
//! message at m > k, and the revoked reader would receive it (finding F10 of the T125 review).
//!
//! [`BroadcastFence`] closes that gap. It releases commits strictly in [`ServerSeq`] order and
//! holds every commit whose earlier positions are not all known yet. For each released commit
//! it
//!
//! 1. asks the [`CommittedScope`] for the subscriptions whose rows left the reader's scope and
//!    emits their `Evict` pages (or `Reset`) before anything else,
//! 2. then evaluates the `see` filter of every op of the commit, when it leaves the queue,
//!    against committed state that contains every commit up to the commit's last position,
//!    and emits one `Ops` message per admitted session.
//!
//! A revocation at k < m therefore always filters m. An op at m < k was visible when it was
//! committed and may be sent; when k committed before m left the queue, the committed state
//! already contains k and m is filtered as well, which never shows a reader more than D126
//! allows.
//!
//! Commits reach the fence from the log section of the process (ARCHITECTURE 5.4, D154): a
//! transaction calls `append_ops`, commits and pushes the range `next()..=last`, from
//! [`BroadcastFence::next`] to the last position `append_ops` returned. A transaction that rolls
//! back pushes nothing. The positions it allocated are given again to the next transaction
//! (memory, SQLite) or stay a gap (PostgreSQL); either way the next commit's range starts at
//! `next()` and covers them as positions without an op. Pushing a rolled back range would be
//! wrong: a reused position would then be refused as released and its op never sent live.
//!
//! The fence sends nothing itself; it returns the messages in the order they must be written to
//! the sessions. Splitting an `Ops` message into frames (D121) is the job of the encoder, which
//! keeps the order. Rows that entered a scope (`Snapshot` rows, 5.6) are not handled here.

use std::collections::BTreeMap;
use std::fmt;

use crate::protocol::{LoggedOp, ModelId, RowId, ServerMsg, ServerSeq, SubId};

/// One committed transaction of the op log.
///
/// It covers the contiguous positions `first..=last`. `ops` are the ops it logged, in strictly
/// increasing position inside that range; positions without an op are allowed, for example
/// positions a rolled back transaction left behind, which the next commit covers (D154).
#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub first: ServerSeq,
    pub last: ServerSeq,
    pub ops: Vec<LoggedOp>,
}

/// A subscription that lost rows through a commit.
#[derive(Clone, Debug, PartialEq)]
pub enum ScopeExit<S> {
    /// Rows of `model` that left the reader's scope or were deleted. The fence splits them
    /// into pages of at most `ids_per_page` ids.
    Evict {
        session: S,
        sub: SubId,
        model: ModelId,
        rows: Vec<RowId>,
    },
    /// The scope change is too large to page; the client drops the subscription's rows.
    Reset { session: S, sub: SubId },
}

/// Read side of the committed state, as the server engine provides it.
///
/// Both methods are called only while the fence releases a commit, in [`ServerSeq`] order,
/// and the state they read contains every commit up to and including that commit.
pub trait CommittedScope {
    /// A connected session; the order decides the order of messages within one commit.
    type Session: Ord + Clone;

    /// Re-evaluates the scope of every subscription whose `see` dependencies (5.6) `commit`
    /// wrote, and returns the rows that left it.
    fn scope_exits(&mut self, commit: &Commit) -> Vec<ScopeExit<Self::Session>>;

    /// Sessions whose read scope admits the row of `op`, evaluated against committed state that
    /// contains every commit up to `upto`. The session of the op's own replica is left out; it
    /// learns the outcome through `Ack`.
    fn readers(&self, op: &LoggedOp, upto: ServerSeq) -> Vec<Self::Session>;
}

/// One message for one session.
#[derive(Clone, Debug, PartialEq)]
pub struct Delivery<S> {
    pub session: S,
    pub msg: ServerMsg,
}

/// Why the fence refused a commit. A refused commit changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FenceError {
    /// `first` is above `last`.
    EmptyRange { first: ServerSeq, last: ServerSeq },
    /// The range starts below the next position to release: it was released already.
    Released { first: ServerSeq, next: ServerSeq },
    /// The range overlaps a commit that is held.
    Overlap { first: ServerSeq, held: ServerSeq },
    /// An op lies outside the range or does not follow the op before it.
    OpPosition { seq: ServerSeq },
    /// The range ends at the highest position; no later position could follow it.
    Exhausted,
}

impl fmt::Display for FenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FenceError::EmptyRange { first, last } => {
                write!(f, "commit range {first}..={last} is empty")
            }
            FenceError::Released { first, next } => {
                write!(f, "commit at {first} lies before the next position {next}")
            }
            FenceError::Overlap { first, held } => {
                write!(f, "commit at {first} overlaps the held commit at {held}")
            }
            FenceError::OpPosition { seq } => {
                write!(f, "op at {seq} lies outside its commit or out of order")
            }
            FenceError::Exhausted => write!(f, "commit ends at the highest log position"),
        }
    }
}

impl std::error::Error for FenceError {}

/// The single ordered broadcast queue of a server process (D126).
#[derive(Debug)]
pub struct BroadcastFence {
    next: ServerSeq,
    held: BTreeMap<ServerSeq, Commit>,
    ids_per_page: usize,
}

impl BroadcastFence {
    /// A queue whose first commit starts at `next`. `ids_per_page` is
    /// [`crate::protocol::Limits::ids_per_page`]; zero is treated as one.
    pub fn new(next: ServerSeq, ids_per_page: u32) -> Self {
        let ids_per_page = usize::try_from(ids_per_page).unwrap_or(usize::MAX).max(1);
        Self {
            next,
            held: BTreeMap::new(),
            ids_per_page,
        }
    }

    /// The position the next released commit must start at.
    pub fn next(&self) -> ServerSeq {
        self.next
    }

    /// Number of commits waiting for an earlier position.
    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// Takes one finished commit and releases every commit that is now contiguous with the
    /// released log. Returns their messages in send order: per commit first the scope exits,
    /// then the ops.
    pub fn push<C: CommittedScope>(
        &mut self,
        commit: Commit,
        scope: &mut C,
    ) -> Result<Vec<Delivery<C::Session>>, FenceError> {
        self.check(&commit)?;
        self.held.insert(commit.first, commit);
        let mut out = Vec::new();
        while let Some(commit) = self.held.remove(&self.next) {
            // `check` refused a range ending at u64::MAX, so this cannot overflow.
            self.next = ServerSeq(commit.last.0 + 1);
            self.release(&commit, scope, &mut out);
        }
        Ok(out)
    }

    fn check(&self, commit: &Commit) -> Result<(), FenceError> {
        let (first, last) = (commit.first, commit.last);
        if first > last {
            return Err(FenceError::EmptyRange { first, last });
        }
        if last.0 == u64::MAX {
            return Err(FenceError::Exhausted);
        }
        if first < self.next {
            return Err(FenceError::Released {
                first,
                next: self.next,
            });
        }
        // Held ranges never overlap, so only the held commit starting last at or before `last`
        // can reach into this range.
        let before = self.held.range(..=last).next_back().map(|(_, c)| c);
        if let Some(held) = before.filter(|held| held.last >= first) {
            return Err(FenceError::Overlap {
                first,
                held: held.first,
            });
        }
        let mut floor = first;
        for (i, logged) in commit.ops.iter().enumerate() {
            let seq = logged.seq;
            let after_previous = i == 0 || seq > floor;
            if seq < first || seq > last || !after_previous {
                return Err(FenceError::OpPosition { seq });
            }
            floor = seq;
        }
        Ok(())
    }

    fn release<C: CommittedScope>(
        &self,
        commit: &Commit,
        scope: &mut C,
        out: &mut Vec<Delivery<C::Session>>,
    ) {
        for exit in scope.scope_exits(commit) {
            match exit {
                ScopeExit::Evict {
                    session,
                    sub,
                    model,
                    rows,
                } => {
                    for page in rows.chunks(self.ids_per_page) {
                        out.push(Delivery {
                            session: session.clone(),
                            msg: ServerMsg::Evict {
                                sub,
                                model,
                                rows: page.to_vec(),
                            },
                        });
                    }
                }
                ScopeExit::Reset { session, sub } => out.push(Delivery {
                    session,
                    msg: ServerMsg::Reset { sub },
                }),
            }
        }
        let mut per_session: BTreeMap<C::Session, Vec<LoggedOp>> = BTreeMap::new();
        for logged in &commit.ops {
            let mut readers = scope.readers(logged, commit.last);
            readers.sort();
            readers.dedup();
            for session in readers {
                per_session.entry(session).or_default().push(logged.clone());
            }
        }
        out.extend(per_session.into_iter().map(|(session, ops)| Delivery {
            session,
            msg: ServerMsg::Ops(ops),
        }));
    }
}

#[cfg(test)]
mod tests {
    //! Simulator: one room whose `see` rule admits its members, messages in that room, and a
    //! committed state that holds every commit the engine finished, released or not.

    use std::collections::BTreeSet;

    use super::*;
    use crate::protocol::{Hlc, Op, OpBody, OpId, ReplicaId, Value};
    use ostrel_core::value::SafeInt;

    const ROOM: ModelId = 0;
    const MESSAGE: ModelId = 1;
    const MEMBERS: u32 = 0;

    const ALICE: u8 = 1;
    const BOB: u8 = 2;
    const CARA: u8 = 3;

    fn row(n: u64) -> RowId {
        RowId::new(Hlc::new(1_000 + n, 0, ReplicaId(9)).unwrap())
    }

    fn logged(seq: u64, author: u8, model: ModelId, row_id: RowId, body: OpBody) -> LoggedOp {
        let replica = ReplicaId(u64::from(author));
        LoggedOp {
            seq: ServerSeq(seq),
            op: Op {
                id: OpId {
                    replica,
                    seq: u32::try_from(seq).unwrap(),
                },
                hlc: Hlc::new(2_000 + seq, 0, replica).unwrap(),
                model,
                row: row_id,
                body,
            },
        }
    }

    fn member(session: u8) -> Value {
        Value::Int(SafeInt::new(i64::from(session)).unwrap())
    }

    /// `author` removes `who` from the room.
    fn revoke(seq: u64, author: u8, who: u8) -> Commit {
        let body = OpBody::SetRemove {
            field: MEMBERS,
            elem: member(who),
            tags: vec![],
        };
        single(logged(seq, author, ROOM, row(0), body))
    }

    /// `author` posts message `n` into the room.
    fn message(seq: u64, author: u8, n: u64) -> Commit {
        let body = OpBody::Make { fields: vec![] };
        single(logged(seq, author, MESSAGE, row(n), body))
    }

    fn single(op: LoggedOp) -> Commit {
        Commit {
            first: op.seq,
            last: op.seq,
            ops: vec![op],
        }
    }

    /// Committed state of the simulated server: every finished commit counts, whether the
    /// fence released it or not, as in the database.
    struct Room {
        committed: BTreeSet<u64>,
        members: BTreeSet<u8>,
        held_messages: BTreeMap<u8, Vec<RowId>>,
        calls: Vec<(&'static str, ServerSeq)>,
    }

    impl Room {
        fn new(members: &[u8]) -> Self {
            Self {
                committed: BTreeSet::new(),
                members: members.iter().copied().collect(),
                held_messages: BTreeMap::new(),
                calls: Vec::new(),
            }
        }

        /// The engine finished `commit`: it is now part of the committed state.
        fn finish(&mut self, commit: &Commit) {
            self.committed.extend(commit.first.0..=commit.last.0);
            for logged in &commit.ops {
                if let OpBody::SetRemove { elem, .. } = &logged.op.body {
                    self.members.retain(|m| member(*m) != *elem);
                }
            }
        }

        fn committed_upto(&self, upto: ServerSeq) -> bool {
            // The fence promises: every position up to `upto` is committed when it asks.
            (1..=upto.0).all(|s| self.committed.contains(&s))
        }
    }

    impl CommittedScope for Room {
        type Session = u8;

        fn scope_exits(&mut self, commit: &Commit) -> Vec<ScopeExit<u8>> {
            self.calls.push(("scope_exits", commit.last));
            assert!(self.committed_upto(commit.last));
            let gone: Vec<u8> = self
                .held_messages
                .keys()
                .copied()
                .filter(|s| !self.members.contains(s))
                .collect();
            gone.into_iter()
                .map(|session| ScopeExit::Evict {
                    session,
                    sub: 7,
                    model: MESSAGE,
                    rows: self.held_messages.remove(&session).unwrap_or_default(),
                })
                .collect()
        }

        fn readers(&self, op: &LoggedOp, upto: ServerSeq) -> Vec<u8> {
            assert!(self.committed_upto(upto));
            assert!(op.seq <= upto);
            let author = u8::try_from(op.op.id.replica.0).unwrap();
            self.members
                .iter()
                .copied()
                .filter(|s| *s != author)
                .collect()
        }
    }

    /// Runs commits through the engine and the fence in the given finish order and records
    /// what each session would receive. Sessions remember the message rows they were sent.
    fn run(members: &[u8], finish_order: Vec<Commit>) -> (Vec<Delivery<u8>>, Room) {
        let mut room = Room::new(members);
        let mut fence = BroadcastFence::new(ServerSeq(1), 1_000);
        let mut sent = Vec::new();
        for commit in finish_order {
            room.finish(&commit);
            let out = fence.push(commit, &mut room).unwrap();
            for d in &out {
                if let ServerMsg::Ops(ops) = &d.msg {
                    for l in ops.iter().filter(|l| l.op.model == MESSAGE) {
                        room.held_messages
                            .entry(d.session)
                            .or_default()
                            .push(l.op.row);
                    }
                }
            }
            sent.extend(out);
        }
        assert_eq!(fence.held(), 0, "every commit must be released");
        (sent, room)
    }

    fn seqs_to(sent: &[Delivery<u8>], session: u8) -> Vec<u64> {
        sent.iter()
            .filter(|d| d.session == session)
            .filter_map(|d| match &d.msg {
                ServerMsg::Ops(ops) => Some(ops.iter().map(|l| l.seq.0)),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn evicts_to(sent: &[Delivery<u8>], session: u8) -> Vec<usize> {
        sent.iter()
            .enumerate()
            .filter(|(_, d)| d.session == session && matches!(d.msg, ServerMsg::Evict { .. }))
            .map(|(i, _)| i)
            .collect()
    }

    /// Simulator case of D126 (finding F10): Alice removes Bob at k = 2 while Cara posts at
    /// m = 3 under another row writer, and Cara's commit finishes first. Bob must never get
    /// m, and his `Evict` comes before every op of the revoking commit.
    #[test]
    fn revoke_then_concurrent_message() {
        let finish_order = vec![
            message(1, CARA, 1),
            message(3, CARA, 3),
            revoke(2, ALICE, BOB),
        ];
        let (sent, _) = run(&[ALICE, BOB, CARA], finish_order);

        assert_eq!(seqs_to(&sent, BOB), vec![1]);
        assert_eq!(seqs_to(&sent, ALICE), vec![1, 3]);
        assert_eq!(seqs_to(&sent, CARA), vec![2]);

        let evicts = evicts_to(&sent, BOB);
        assert_eq!(evicts.len(), 1);
        let ServerMsg::Evict { sub, model, rows } = &sent[evicts[0]].msg else {
            unreachable!()
        };
        assert_eq!((*sub, *model, rows.clone()), (7, MESSAGE, vec![row(1)]));
        let first_op_of_k = sent
            .iter()
            .position(|d| matches!(&d.msg, ServerMsg::Ops(o) if o[0].seq == ServerSeq(2)))
            .unwrap();
        assert!(
            evicts[0] < first_op_of_k,
            "Evict must precede the ops of its commit"
        );
    }

    /// In every finish order the revoked reader never sees a position above the revocation,
    /// readers the revocation does not touch get the same ops, and each session gets non
    /// decreasing positions (D62). Bob may also miss position 1 when the revocation finished
    /// before it left the queue; D126 allows but does not require sending it.
    #[test]
    fn every_finish_order_keeps_the_fence() {
        let commits = [
            message(1, CARA, 1),
            revoke(2, ALICE, BOB),
            message(3, CARA, 3),
            message(4, ALICE, 4),
        ];
        let orders = permutations(commits.len());
        assert_eq!(orders.len(), 24);
        for order in orders {
            let finish: Vec<Commit> = order.iter().map(|i| commits[*i].clone()).collect();
            let (sent, _) = run(&[ALICE, BOB, CARA], finish);
            assert!(
                seqs_to(&sent, BOB).iter().all(|s| *s < 2),
                "order {order:?}"
            );
            assert_eq!(seqs_to(&sent, ALICE), vec![1, 3], "order {order:?}");
            assert_eq!(seqs_to(&sent, CARA), vec![2, 4], "order {order:?}");
            for session in [ALICE, BOB, CARA] {
                let seqs = seqs_to(&sent, session);
                assert!(seqs.windows(2).all(|w| w[0] <= w[1]), "order {order:?}");
            }
        }
    }

    /// An op committed before the revocation and released before it reaches the reader.
    #[test]
    fn op_before_revocation_is_sent() {
        let (sent, _) = run(
            &[ALICE, BOB],
            vec![message(1, ALICE, 1), revoke(2, ALICE, BOB)],
        );
        assert_eq!(seqs_to(&sent, BOB), vec![1]);
        assert_eq!(evicts_to(&sent, BOB).len(), 1);
    }

    #[test]
    fn a_gap_holds_later_commits_until_it_is_filled() {
        let mut room = Room::new(&[ALICE, BOB]);
        let mut fence = BroadcastFence::new(ServerSeq(1), 1_000);
        let later = message(3, ALICE, 3);
        room.finish(&later);
        assert_eq!(fence.push(later, &mut room).unwrap(), vec![]);
        assert_eq!((fence.held(), fence.next()), (1, ServerSeq(1)));
        assert!(
            room.calls.is_empty(),
            "nothing is evaluated before the gap closes"
        );

        // A range without ops is released like any other. Under D154 such a range only arises
        // as part of a commit that covers positions a rolled back transaction left behind; the
        // fence itself does not care why a position carries no op.
        let rolled_back = Commit {
            first: ServerSeq(1),
            last: ServerSeq(2),
            ops: vec![],
        };
        room.finish(&rolled_back);
        let sent = fence.push(rolled_back, &mut room).unwrap();
        assert_eq!(seqs_to(&sent, BOB), vec![3]);
        assert_eq!((fence.held(), fence.next()), (0, ServerSeq(4)));
        assert_eq!(
            room.calls,
            vec![("scope_exits", ServerSeq(2)), ("scope_exits", ServerSeq(3))]
        );
    }

    #[test]
    fn one_ops_message_per_session_and_commit() {
        let mut room = Room::new(&[ALICE, BOB]);
        let mut fence = BroadcastFence::new(ServerSeq(1), 1_000);
        let commit = Commit {
            first: ServerSeq(1),
            last: ServerSeq(3),
            ops: vec![
                logged(1, ALICE, MESSAGE, row(1), OpBody::Make { fields: vec![] }),
                logged(3, ALICE, MESSAGE, row(3), OpBody::Make { fields: vec![] }),
            ],
        };
        room.finish(&commit);
        let sent = fence.push(commit, &mut room).unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].session, BOB);
        assert_eq!(seqs_to(&sent, BOB), vec![1, 3]);
    }

    struct Fixed(Vec<ScopeExit<u8>>);

    impl CommittedScope for Fixed {
        type Session = u8;
        fn scope_exits(&mut self, _: &Commit) -> Vec<ScopeExit<u8>> {
            std::mem::take(&mut self.0)
        }
        fn readers(&self, _: &LoggedOp, _: ServerSeq) -> Vec<u8> {
            vec![BOB, ALICE, BOB]
        }
    }

    #[test]
    fn evictions_are_paged_and_readers_deduplicated() {
        let rows: Vec<RowId> = (1..=5).map(row).collect();
        let mut scope = Fixed(vec![
            ScopeExit::Evict {
                session: BOB,
                sub: 4,
                model: MESSAGE,
                rows: rows.clone(),
            },
            ScopeExit::Reset {
                session: CARA,
                sub: 5,
            },
        ]);
        let mut fence = BroadcastFence::new(ServerSeq(1), 2);
        let sent = fence.push(message(1, CARA, 9), &mut scope).unwrap();
        let pages: Vec<Vec<RowId>> = sent
            .iter()
            .filter_map(|d| match &d.msg {
                ServerMsg::Evict { rows, .. } => Some(rows.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            pages,
            vec![rows[0..2].to_vec(), rows[2..4].to_vec(), rows[4..].to_vec()]
        );
        assert_eq!(sent[3].msg, ServerMsg::Reset { sub: 5 });
        let sessions: Vec<u8> = sent[4..].iter().map(|d| d.session).collect();
        assert_eq!(sessions, vec![ALICE, BOB]);
    }

    #[test]
    fn refused_commits_change_nothing() {
        let mut scope = Fixed(vec![]);
        let mut fence = BroadcastFence::new(ServerSeq(5), 1_000);
        fence.push(message(7, ALICE, 7), &mut scope).unwrap();
        let cases = [
            (
                Commit {
                    first: ServerSeq(9),
                    last: ServerSeq(8),
                    ops: vec![],
                },
                FenceError::EmptyRange {
                    first: ServerSeq(9),
                    last: ServerSeq(8),
                },
            ),
            (
                message(4, ALICE, 4),
                FenceError::Released {
                    first: ServerSeq(4),
                    next: ServerSeq(5),
                },
            ),
            (
                Commit {
                    first: ServerSeq(6),
                    last: ServerSeq(7),
                    ops: vec![],
                },
                FenceError::Overlap {
                    first: ServerSeq(6),
                    held: ServerSeq(7),
                },
            ),
            (
                Commit {
                    first: ServerSeq(8),
                    last: ServerSeq(9),
                    ops: vec![message(10, ALICE, 10).ops[0].clone()],
                },
                FenceError::OpPosition { seq: ServerSeq(10) },
            ),
            (
                Commit {
                    first: ServerSeq(8),
                    last: ServerSeq(9),
                    ops: vec![
                        message(9, ALICE, 9).ops[0].clone(),
                        message(8, ALICE, 8).ops[0].clone(),
                    ],
                },
                FenceError::OpPosition { seq: ServerSeq(8) },
            ),
            (
                Commit {
                    first: ServerSeq(8),
                    last: ServerSeq(u64::MAX),
                    ops: vec![],
                },
                FenceError::Exhausted,
            ),
        ];
        for (commit, want) in cases {
            assert_eq!(fence.push(commit, &mut scope), Err(want));
            assert_eq!((fence.held(), fence.next()), (1, ServerSeq(5)));
            assert!(!want.to_string().is_empty());
        }
    }

    /// All orders of `0..n` (Heap's algorithm), without a random source.
    fn permutations(n: usize) -> Vec<Vec<usize>> {
        fn heap(k: usize, items: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
            if k <= 1 {
                out.push(items.clone());
                return;
            }
            heap(k - 1, items, out);
            for i in 0..k - 1 {
                let j = if k.is_multiple_of(2) { i } else { 0 };
                items.swap(j, k - 1);
                heap(k - 1, items, out);
            }
        }
        let mut out = Vec::new();
        heap(n, &mut (0..n).collect(), &mut out);
        out
    }
}
