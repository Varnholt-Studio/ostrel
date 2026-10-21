//! Write lock, send queue accounting, waiting rows and closures of one server process
//! (ARCHITECTURE 5.4 "Snapshot consistency", 5.6, 5.9; D156 to D162).
//!
//! The module holds no I/O and no clock. It is the state that the server engine drives from its
//! tasks, so every interleaving of those tasks can be replayed in a test. It has three parts.
//!
//! * [`WriteLock`] is the single FIFO write lock of the process (D156). Every database
//!   transaction that writes takes it before `begin` and releases it after `commit` or
//!   `rollback`, the fence push and the queuing of its outcomes. A task that needs a quiet point
//!   (a closure, the registration of a subscription, M0 of a reconnect) is granted the lock
//!   only when it holds no transaction open and every commit pushed to the broadcast queue is
//!   released: then M = `next() - 1` is a quiet point, every position at or below M that is
//!   ever committed is committed and released by the broadcast fence, and none above M is
//!   allocated (5.4 "Snapshot consistency"). The lock also carries the bound of the broadcast
//!   queue (D162): while [`BROADCAST_COMMITS`] commits are not yet released, no writer is
//!   granted the lock, so a task waits before the lock and never inside it.
//! * [`SendQueue`] counts the encoded frames of one session that are not yet written (5.9, 8
//!   MiB). Frames queued under the lock or by the broadcast queue never wait: live ops,
//!   outcomes, and the `Evict` pages and `Reset` of scope re-evaluation. If they would exceed
//!   the limit the session is overloaded and closes (D158 (1), D162). Frames that a session's
//!   own task sends outside the lock (catch up, reconnect `Evict`, closure pages) wait for room
//!   instead (D159).
//! * [`WaitingRows`] keeps, per (session, subscription), the rows that must reach the session
//!   as state and not as ops, the rows already served with the mark they are live from, and the
//!   lazy candidate reading (D159). [`WaitingRows::run_closure`] is one closure: it runs under
//!   the write lock at a quiet point M, picks waiting rows round robin over sessions and their
//!   subscriptions, reads them by id until [`CLOSURE_ROWS`] rows or [`CLOSURE_BYTES`] are read
//!   (at least one row), and returns the `Snapshot` pages with mark M. No query by filter runs
//!   inside a closure, so its cost does not grow with the scope or the table.
//!
//! Why the quiet point matters: a row read at M contains exactly the ops at or below M, and a
//! served row is live from M, so the broadcast queue sends it every op above M. If the mark
//! were the highest committed position while a lower position is still in flight, that lower op
//! would be in neither the row nor the live stream (lost). If the mark were the last released
//! position while a later commit is already visible in the row, that op would arrive again live
//! (applied twice). The tests in `tests/ac_44_snapshot/` show both faults for a closure that
//! skips the lock, and that the gated closure has neither.
//!
//! Released matters as much as committed. The broadcast queue re-evaluates scopes when it
//! releases a commit: a row leaves (`Evict` or a dropped waiting row) or enters (a waiting row).
//! If a closure served a row at M while a commit k at or below M was still unreleased, the
//! release of k would then re-evaluate the scope against a row that already contains later
//! commits: it could evict the served row and let it enter again as a waiting row while the
//! client keeps the copy read at M, and a later scope exit would drop the waiting row without an
//! `Evict` (RED kira #2455). Granting quiet point tasks only with no commit unreleased rules this
//! out.
//!
//! The pages of a closure carry mark M. At a quiet point nothing up to M waits in the broadcast
//! queue, so the caller queues them into each session's stream at once, behind everything
//! already released, and each session's stream stays non decreasing in [`ServerSeq`] (D62). Splitting pages at the frame limit (D121) is the job of
//! the encoder. Mapping [`QueueError::Overloaded`] to the protocol error `Overloaded` is the
//! server's; the error code itself is part of the protocol contract (ARCHITECTURE 5.4
//! "Interfaces", D159).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use crate::protocol::{RowId, ServerSeq, SubId};

/// Send queue per session: encoded frames not yet written (5.9, D156, D161).
pub const SEND_QUEUE_BYTES: u64 = 8 * 1024 * 1024;

/// Rows read by one closure at most (5.9, D158 (3); an internal portion of work).
pub const CLOSURE_ROWS: usize = 256;

/// Encoded bytes read by one closure at most, reference hops included (5.9, D159).
pub const CLOSURE_BYTES: u64 = 4 * 1024 * 1024;

/// Commits in the broadcast queue not yet released before writers wait (5.9, D162).
pub const BROADCAST_COMMITS: usize = 1024;

/// Candidate ids per read page; the next page is read only when fewer candidate or waiting
/// rows than this remain for the subscription (5.4, D159).
pub const CANDIDATE_PAGE: usize = 1000;

// ---------------------------------------------------------------------------------------------
// Write lock
// ---------------------------------------------------------------------------------------------

/// Place of a task in the write lock queue. Tickets grow with every request, so their order
/// is the FIFO order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ticket(u64);

/// What a queued task needs from the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// A write transaction: granted while the broadcast queue is below its bound.
    Write,
    /// A quiet point (closure, registration of a subscription, M0 of a reconnect): granted only
    /// when every pushed commit is released.
    QuietPoint,
}

/// What the holder did before it released the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// The transaction committed and pushed one commit to the broadcast queue.
    Committed,
    /// Nothing reached the broadcast queue: a rollback, a closure, or a task that only took
    /// the quiet point (reconnect M0, registration of a subscription).
    Nothing,
}

/// A misuse of the write lock. It changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockError {
    /// The ticket does not hold the lock.
    NotHolder { ticket: Ticket },
    /// The broadcast queue reported more released commits than were pushed.
    NothingToRelease,
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockError::NotHolder { ticket } => {
                write!(f, "ticket {} does not hold the write lock", ticket.0)
            }
            LockError::NothingToRelease => {
                write!(f, "the broadcast queue holds no unreleased commit")
            }
        }
    }
}

impl std::error::Error for LockError {}

/// The single FIFO write lock of a server process (D156) with the bound of the broadcast queue
/// (D162).
///
/// A writer calls [`WriteLock::request`], a task that reads at a quiet point calls
/// [`WriteLock::request_quiet`]; both wait until [`WriteLock::grant`] hands them the lock.
/// Grants follow request order. No writer is granted while the broadcast queue holds
/// [`BROADCAST_COMMITS`] unreleased commits, so a slow read connection of the broadcast queue
/// slows writers and never exhausts memory. No quiet point task is granted while any commit is
/// unreleased, and the tasks queued behind it wait with it (FIFO). The broadcast queue releases
/// without the lock, so that wait always ends. The holder itself is never stopped, because it
/// waits on nothing but the writer connection.
#[derive(Debug)]
pub struct WriteLock<T> {
    queue: VecDeque<(Ticket, Need, T)>,
    holder: Option<Ticket>,
    next_ticket: u64,
    unreleased: usize,
    bound: usize,
}

impl<T> WriteLock<T> {
    /// A free lock with the broadcast bound of 5.9.
    pub fn new() -> Self {
        Self::with_bound(BROADCAST_COMMITS)
    }

    /// A free lock whose grants stop at `bound` unreleased commits; zero is treated as one.
    pub fn with_bound(bound: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            holder: None,
            next_ticket: 0,
            unreleased: 0,
            bound: bound.max(1),
        }
    }

    /// A writer joins the end of the queue.
    pub fn request(&mut self, task: T) -> Ticket {
        self.enqueue(Need::Write, task)
    }

    /// A task that reads at a quiet point joins the end of the queue.
    pub fn request_quiet(&mut self, task: T) -> Ticket {
        self.enqueue(Need::QuietPoint, task)
    }

    fn enqueue(&mut self, need: Need, task: T) -> Ticket {
        let ticket = Ticket(self.next_ticket);
        self.next_ticket += 1;
        self.queue.push_back((ticket, need, task));
        ticket
    }

    /// Hands the lock to the first queued task, if the lock is free and the task may start: a
    /// writer while the broadcast queue is below its bound, a quiet point task when no commit
    /// is unreleased.
    pub fn grant(&mut self) -> Option<(Ticket, T)> {
        if self.holder.is_some() {
            return None;
        }
        let need = self.queue.front()?.1;
        let ready = match need {
            Need::Write => self.unreleased < self.bound,
            Need::QuietPoint => self.unreleased == 0,
        };
        if !ready {
            return None;
        }
        let (ticket, _, task) = self.queue.pop_front()?;
        self.holder = Some(ticket);
        Some((ticket, task))
    }

    /// The ticket that holds the lock.
    pub fn holder(&self) -> Option<Ticket> {
        self.holder
    }

    /// Tasks queued and not yet granted.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Commits pushed to the broadcast queue and not yet released by it.
    pub fn unreleased(&self) -> usize {
        self.unreleased
    }

    /// The holder releases the lock. A commit counts against the broadcast bound until the
    /// broadcast queue reports it with [`WriteLock::commit_released`].
    pub fn release(&mut self, ticket: Ticket, outcome: Release) -> Result<(), LockError> {
        if self.holder != Some(ticket) {
            return Err(LockError::NotHolder { ticket });
        }
        self.holder = None;
        if outcome == Release::Committed {
            self.unreleased += 1;
        }
        Ok(())
    }

    /// The broadcast queue released one commit (all its pages and ops are queued into the
    /// session streams).
    pub fn commit_released(&mut self) -> Result<(), LockError> {
        if self.unreleased == 0 {
            return Err(LockError::NothingToRelease);
        }
        self.unreleased -= 1;
        Ok(())
    }
}

impl<T> Default for WriteLock<T> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------------
// Send queue
// ---------------------------------------------------------------------------------------------

/// Why a frame was not queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueError {
    /// A frame that never waits would push the queue over its limit. The frames not yet
    /// written are dropped, the session gets the fatal `Error Overloaded` and closes (D158 (1)).
    Overloaded,
    /// The session closed earlier; nothing more is queued.
    Closed,
}

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueueError::Overloaded => write!(f, "send queue limit exceeded"),
            QueueError::Closed => write!(f, "session closed"),
        }
    }
}

impl std::error::Error for QueueError {}

/// Room of one session's send queue as a closure sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Room {
    /// Bytes that still fit before the limit.
    pub free: u64,
    /// The limit of the queue; a row larger than it never fits.
    pub limit: u64,
}

/// Bytes of encoded frames of one session that are not yet written (5.9).
#[derive(Debug)]
pub struct SendQueue {
    queued: u64,
    limit: u64,
    starved: bool,
    closed: bool,
}

impl SendQueue {
    /// An empty queue with the limit of 5.9.
    pub fn new() -> Self {
        Self::with_limit(SEND_QUEUE_BYTES)
    }

    /// An empty queue with `limit` bytes.
    pub fn with_limit(limit: u64) -> Self {
        Self {
            queued: 0,
            limit,
            starved: false,
            closed: false,
        }
    }

    /// Bytes queued and not yet written.
    pub fn queued(&self) -> u64 {
        self.queued
    }

    /// Bytes that still fit before the limit.
    pub fn room(&self) -> u64 {
        self.limit.saturating_sub(self.queued)
    }

    /// Free bytes and limit, for [`WaitingRows::run_closure`].
    pub fn space(&self) -> Room {
        Room {
            free: self.room(),
            limit: self.limit,
        }
    }

    /// Whether the session was closed by an overflow.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Queues a frame that never waits: a live op, an outcome, or an `Evict` page or `Reset` of
    /// scope re-evaluation (D162). Over the limit the session closes with
    /// [`QueueError::Overloaded`]; the queue then holds nothing, because the frames not yet
    /// written are dropped.
    pub fn push_now(&mut self, bytes: u64) -> Result<(), QueueError> {
        if self.closed {
            return Err(QueueError::Closed);
        }
        match self.queued.checked_add(bytes) {
            Some(total) if total <= self.limit => {
                self.queued = total;
                Ok(())
            }
            _ => {
                self.closed = true;
                self.queued = 0;
                Err(QueueError::Overloaded)
            }
        }
    }

    /// Queues a frame that waits for room: catch up, reconnect `Evict`, `Reset` on reconnect
    /// (D159). Returns `Ok(false)` when it does not fit; the task then waits until frames are
    /// written. An empty queue always takes one frame, so a frame never waits forever.
    pub fn push_when_room(&mut self, bytes: u64) -> Result<bool, QueueError> {
        if self.closed {
            return Err(QueueError::Closed);
        }
        let fits = self
            .queued
            .checked_add(bytes)
            .is_some_and(|total| total <= self.limit);
        if fits || self.queued == 0 {
            self.queued = self.queued.saturating_add(bytes);
            return Ok(true);
        }
        Ok(false)
    }

    /// Queues the closure pages that [`WaitingRows::run_closure`] admitted against
    /// [`SendQueue::room`]. They were measured against the room under the write lock, so they
    /// fit unless the caller passed a larger room.
    pub fn push_closure(&mut self, bytes: u64) -> Result<(), QueueError> {
        match self.push_when_room(bytes)? {
            true => Ok(()),
            false => Err(QueueError::Overloaded),
        }
    }

    /// A closure skipped this session because its rows did not fit; it asks for a closure again
    /// once the queue drains below half (D159).
    pub fn mark_starved(&mut self) {
        self.starved = true;
    }

    /// `bytes` were written to the socket. Returns true once when a starved session drained
    /// below half of its limit; the caller then requests a closure.
    pub fn written(&mut self, bytes: u64) -> bool {
        self.queued = self.queued.saturating_sub(bytes);
        if self.starved && !self.closed && self.queued < self.limit / 2 {
            self.starved = false;
            return true;
        }
        false
    }
}

impl Default for SendQueue {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------------
// Waiting rows and closures
// ---------------------------------------------------------------------------------------------

/// A row read by id under the write lock for one subscription.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadRow<R> {
    /// The row as a `Snapshot` would carry it (only fields the reader may read).
    pub row: R,
    /// Encoded size of the row in a `Snapshot` page; it must fit the session's room.
    pub encoded_bytes: u64,
    /// Bytes read for the row, reference hops included; they count to [`CLOSURE_BYTES`].
    pub read_bytes: u64,
    /// The subscription's filter and `see` admit the row at M (`ostrel_db::eval`).
    pub visible: bool,
}

/// Point reads of a closure on the writer connection, at the quiet point M.
pub trait ClosureReader<S> {
    type Row;
    type Error;

    /// Reads `row` by id and evaluates the filter and `see` of `(session, sub)` on it. `None`
    /// means the row does not exist (deleted).
    fn read(
        &mut self,
        session: &S,
        sub: SubId,
        row: RowId,
    ) -> Result<Option<ReadRow<Self::Row>>, Self::Error>;
}

/// One `Snapshot` page for one subscription of one session.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotPage<S, R> {
    pub session: S,
    pub sub: SubId,
    pub rows: Vec<R>,
    /// The quiet point the rows were read at.
    pub seq: ServerSeq,
    /// The subscription's candidates and waiting rows are exhausted.
    pub last: bool,
    /// Encoded bytes of the rows, as admitted against the session's room.
    pub bytes: u64,
}

/// Result of one closure.
#[derive(Clone, Debug, PartialEq)]
pub struct ClosureOutput<S, R> {
    /// Pages in session order, then subscription order.
    pub pages: Vec<SnapshotPage<S, R>>,
    /// Sessions skipped because a row did not fit their room; each marks its queue starved.
    pub starved: Vec<S>,
    /// Rows larger than the whole send queue of their session: they can never be queued. Each
    /// left the waiting rows, and the caller ends its session with the fatal `Error Overloaded`
    /// (D158) instead of letting the subscription wait for room forever. Under the limits of
    /// 5.9 (`row_bytes` 768 KiB, send queue 8 MiB) no row reaches this.
    pub oversized: Vec<(S, SubId, RowId)>,
    /// Rows read, the portion counted against [`CLOSURE_ROWS`].
    pub rows_read: usize,
    /// Bytes read, the portion counted against [`CLOSURE_BYTES`].
    pub bytes_read: u64,
}

/// What the caller does with a row that left a subscription's scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvictAction {
    /// The row was served: send its `Evict`.
    Send,
    /// The row was only waiting: it is dropped without a message.
    Drop,
    /// The row is unknown to the subscription: nothing to do.
    Ignore,
}

/// A misuse of the waiting row registry. It changes nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitError {
    /// The subscription is registered already.
    Exists { sub: SubId },
    /// The subscription is not registered.
    Unknown { sub: SubId },
    /// Candidates were added while no candidate page was being read.
    NoCandidateRead { sub: SubId },
}

impl fmt::Display for WaitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WaitError::Exists { sub } => write!(f, "subscription {sub} is registered already"),
            WaitError::Unknown { sub } => write!(f, "subscription {sub} is not registered"),
            WaitError::NoCandidateRead { sub } => {
                write!(f, "subscription {sub} reads no candidate page")
            }
        }
    }
}

impl std::error::Error for WaitError {}

/// Rows a closure admitted per subscription: id, row and encoded bytes.
type Taken<S, R> = BTreeMap<(S, SubId), Vec<(RowId, R, u64)>>;

/// Rows of a closure per subscription and their encoded bytes.
type Served<S, R> = BTreeMap<(S, SubId), (Vec<R>, u64)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Candidates {
    /// More pages may follow; none is being read.
    Open,
    /// A page is being read on the read connection.
    Reading,
    /// Every candidate was read.
    Exhausted,
}

#[derive(Debug)]
struct SubState {
    waiting: BTreeSet<RowId>,
    /// Served rows and the mark they are live from.
    served: BTreeMap<RowId, ServerSeq>,
    candidates: Candidates,
    last_sent: bool,
}

impl SubState {
    fn new(candidates: Candidates) -> Self {
        Self {
            waiting: BTreeSet::new(),
            served: BTreeMap::new(),
            candidates,
            last_sent: false,
        }
    }

    fn complete(&self) -> bool {
        self.candidates == Candidates::Exhausted && self.waiting.is_empty()
    }

    fn has_work(&self) -> bool {
        !self.waiting.is_empty() || (self.complete() && !self.last_sent)
    }
}

/// Waiting rows, served rows and candidate reading of every subscription of the process, and
/// the single queued closure (5.4, 5.9).
#[derive(Debug)]
pub struct WaitingRows<S> {
    subs: BTreeMap<S, BTreeMap<SubId, SubState>>,
    closure_queued: bool,
    /// Round robin position: the session served last, and per session the subscription.
    last_session: Option<S>,
    last_sub: BTreeMap<S, SubId>,
}

impl<S: Ord + Clone> WaitingRows<S> {
    pub fn new() -> Self {
        Self {
            subs: BTreeMap::new(),
            closure_queued: false,
            last_session: None,
            last_sub: BTreeMap::new(),
        }
    }

    /// Registers a new subscription. It runs under the write lock at a quiet point and reads
    /// nothing; candidates are read later on the read connection (5.4).
    pub fn subscribe(&mut self, session: S, sub: SubId) -> Result<(), WaitError> {
        let subs = self.subs.entry(session).or_default();
        if subs.contains_key(&sub) {
            return Err(WaitError::Exists { sub });
        }
        subs.insert(sub, SubState::new(Candidates::Open));
        Ok(())
    }

    /// Registers a resubscribed subscription on reconnect: the rows the client named in `Held`
    /// are live from M0 (5.4). Rows in scope that it did not name arrive through candidates as
    /// waiting rows.
    pub fn resubscribe(
        &mut self,
        session: S,
        sub: SubId,
        held: impl IntoIterator<Item = RowId>,
        m0: ServerSeq,
    ) -> Result<(), WaitError> {
        self.subscribe(session.clone(), sub)?;
        let state = self.state_mut(&session, sub)?;
        state.served.extend(held.into_iter().map(|row| (row, m0)));
        Ok(())
    }

    /// Ends a subscription and forgets its rows.
    pub fn unsubscribe(&mut self, session: &S, sub: SubId) {
        if let Some(subs) = self.subs.get_mut(session) {
            subs.remove(&sub);
            if subs.is_empty() {
                self.subs.remove(session);
                self.last_sub.remove(session);
            }
        }
    }

    /// Ends every subscription of a closed session.
    pub fn close_session(&mut self, session: &S) {
        self.subs.remove(session);
        self.last_sub.remove(session);
    }

    /// Whether the read connection should read the next candidate page of `(session, sub)`:
    /// more may follow, none is being read, and fewer than [`CANDIDATE_PAGE`] rows wait.
    pub fn wants_candidates(&self, session: &S, sub: SubId) -> bool {
        self.state(session, sub).is_some_and(|state| {
            state.candidates == Candidates::Open && state.waiting.len() < CANDIDATE_PAGE
        })
    }

    /// The read connection starts reading a candidate page.
    pub fn begin_candidates(&mut self, session: &S, sub: SubId) -> Result<(), WaitError> {
        let state = self.state_mut(session, sub)?;
        if state.candidates == Candidates::Open {
            state.candidates = Candidates::Reading;
        }
        Ok(())
    }

    /// One page of candidate ids (filter only, no rule) read outside the lock. Served rows are
    /// skipped; every other id becomes a waiting row. `last` ends the candidates.
    pub fn add_candidates(
        &mut self,
        session: &S,
        sub: SubId,
        rows: impl IntoIterator<Item = RowId>,
        last: bool,
    ) -> Result<(), WaitError> {
        let state = self.state_mut(session, sub)?;
        if state.candidates != Candidates::Reading {
            return Err(WaitError::NoCandidateRead { sub });
        }
        for row in rows {
            if !state.served.contains_key(&row) {
                state.waiting.insert(row);
            }
        }
        state.candidates = if last {
            Candidates::Exhausted
        } else {
            Candidates::Open
        };
        Ok(())
    }

    /// A row entered the scope of `(session, sub)` at the release of a commit (5.6). Unless it
    /// is served already it waits for the next closure.
    pub fn enter(&mut self, session: &S, sub: SubId, row: RowId) -> Result<(), WaitError> {
        let state = self.state_mut(session, sub)?;
        if !state.served.contains_key(&row) {
            state.waiting.insert(row);
        }
        Ok(())
    }

    /// A row left the scope of `(session, sub)` or was deleted. A waiting row is removed
    /// without a message; a served row is no longer live and its `Evict` is sent.
    pub fn evict(&mut self, session: &S, sub: SubId, row: RowId) -> EvictAction {
        let Some(state) = self.state_mut(session, sub).ok() else {
            return EvictAction::Ignore;
        };
        if state.waiting.remove(&row) {
            EvictAction::Drop
        } else if state.served.remove(&row).is_some() {
            EvictAction::Send
        } else {
            EvictAction::Ignore
        }
    }

    /// Whether the op at `seq` on `row` goes to `(session, sub)` live: the row is served and
    /// `seq` lies above the mark it was served at. A waiting row gets no op.
    pub fn is_live(&self, session: &S, sub: SubId, row: RowId, seq: ServerSeq) -> bool {
        self.state(session, sub)
            .and_then(|state| state.served.get(&row))
            .is_some_and(|mark| seq > *mark)
    }

    /// Rows waiting for `(session, sub)`.
    pub fn waiting(&self, session: &S, sub: SubId) -> usize {
        self.state(session, sub)
            .map_or(0, |state| state.waiting.len())
    }

    /// Whether any subscription has waiting rows or owes its last page.
    pub fn has_work(&self) -> bool {
        self.subs
            .values()
            .flat_map(BTreeMap::values)
            .any(SubState::has_work)
    }

    /// Whether a subscription of `session` has waiting rows or owes its last page. After a
    /// closure the caller requests the next one only for sessions that were not starved; a
    /// starved session asks again when its queue drains ([`SendQueue::written`]).
    pub fn has_work_for(&self, session: &S) -> bool {
        self.subs
            .get(session)
            .is_some_and(|subs| subs.values().any(SubState::has_work))
    }

    /// A task asks for a closure. Returns true when the caller must queue a closure task at the
    /// write lock; false when one is queued already and the request joins it (5.9: at most one).
    pub fn request_closure(&mut self) -> bool {
        if self.closure_queued {
            return false;
        }
        self.closure_queued = true;
        true
    }

    /// Whether a closure is queued and not yet running.
    pub fn closure_queued(&self) -> bool {
        self.closure_queued
    }

    /// One closure. The caller holds the write lock, granted through
    /// [`WriteLock::request_quiet`], at the quiet point `mark` with no transaction open.
    /// `rooms` is the room of each session's send queue ([`SendQueue::space`]); a session
    /// missing from it has no room.
    ///
    /// Picks waiting rows round robin over sessions and, inside a session, over its
    /// subscriptions, and reads them by id until [`CLOSURE_ROWS`] rows or [`CLOSURE_BYTES`]
    /// are read (at least one row). A visible row that fits the room left in its session is
    /// served from `mark`; an invisible or deleted row is dropped from the waiting rows; a row
    /// that does not fit goes back and its session is skipped for the rest of the closure; a
    /// row larger than its session's whole queue is reported in
    /// [`ClosureOutput::oversized`].
    ///
    /// On a read error nothing changes and the error is returned. The closure then stays
    /// queued ([`WaitingRows::closure_queued`] is true and further requests join it), so the
    /// caller must queue the closure task at the write lock again, or end the process; the
    /// waiting rows are never left without a queued closure.
    pub fn run_closure<C: ClosureReader<S>>(
        &mut self,
        mark: ServerSeq,
        rooms: &BTreeMap<S, Room>,
        reader: &mut C,
    ) -> Result<ClosureOutput<S, C::Row>, C::Error> {
        let mut room: BTreeMap<S, u64> = rooms.iter().map(|(s, r)| (s.clone(), r.free)).collect();
        let mut skipped: BTreeSet<S> = BTreeSet::new();
        let mut picked: BTreeSet<(S, SubId, RowId)> = BTreeSet::new();
        let mut taken: Taken<S, C::Row> = BTreeMap::new();
        let mut dropped: Vec<(S, SubId, RowId)> = Vec::new();
        let mut starved: Vec<S> = Vec::new();
        let mut oversized: Vec<(S, SubId, RowId)> = Vec::new();
        let mut rows_read = 0usize;
        let mut bytes_read = 0u64;
        let mut last_session = self.last_session.clone();
        let mut last_sub = self.last_sub.clone();

        while rows_read < CLOSURE_ROWS && bytes_read < CLOSURE_BYTES {
            let Some((session, sub, row)) =
                self.next_pick(&last_session, &last_sub, &skipped, &picked)
            else {
                break;
            };
            last_session = Some(session.clone());
            last_sub.insert(session.clone(), sub);
            picked.insert((session.clone(), sub, row));
            let read = reader.read(&session, sub, row)?;
            rows_read += 1;
            let Some(read) = read else {
                dropped.push((session, sub, row));
                continue;
            };
            bytes_read = bytes_read.saturating_add(read.read_bytes);
            if !read.visible {
                dropped.push((session, sub, row));
                continue;
            }
            let limit = rooms.get(&session).map_or(0, |r| r.limit);
            if rooms.contains_key(&session) && read.encoded_bytes > limit {
                // It never fits: report it instead of waiting for room forever (RED kira F4).
                dropped.push((session.clone(), sub, row));
                skipped.insert(session.clone());
                oversized.push((session, sub, row));
                continue;
            }
            let left = room.get(&session).copied().unwrap_or(0);
            if read.encoded_bytes > left {
                // The row stays waiting; the session drains first.
                picked.remove(&(session.clone(), sub, row));
                skipped.insert(session.clone());
                starved.push(session);
                continue;
            }
            room.insert(session.clone(), left - read.encoded_bytes);
            taken
                .entry((session, sub))
                .or_default()
                .push((row, read.row, read.encoded_bytes));
        }

        // Every read succeeded: apply the outcome. Only now the queued closure counts as run.
        self.closure_queued = false;
        self.last_session = last_session;
        self.last_sub = last_sub;
        for (session, sub, row) in dropped {
            if let Ok(state) = self.state_mut(&session, sub) {
                state.waiting.remove(&row);
            }
        }
        let mut served: Served<S, C::Row> = BTreeMap::new();
        for ((session, sub), rows) in taken {
            if let Ok(state) = self.state_mut(&session, sub) {
                let entry = served.entry((session, sub)).or_default();
                for (row, data, bytes) in rows {
                    state.waiting.remove(&row);
                    state.served.insert(row, mark);
                    entry.0.push(data);
                    entry.1 += bytes;
                }
            }
        }
        let mut pages = Vec::new();
        for (session, subs) in self.subs.iter_mut() {
            if skipped.contains(session) {
                continue;
            }
            for (sub, state) in subs.iter_mut() {
                let rows = served.remove(&(session.clone(), *sub));
                let last = state.complete() && !state.last_sent;
                if rows.is_none() && !last {
                    continue;
                }
                let (rows, bytes) = rows.unwrap_or_default();
                state.last_sent |= last;
                pages.push(SnapshotPage {
                    session: session.clone(),
                    sub: *sub,
                    rows,
                    seq: mark,
                    last,
                    bytes,
                });
            }
        }
        // Rows served to a skipped session before it was skipped.
        for ((session, sub), (rows, bytes)) in served {
            let last = self
                .state(&session, sub)
                .is_some_and(|state| state.complete() && !state.last_sent);
            if let Ok(state) = self.state_mut(&session, sub) {
                state.last_sent |= last;
            }
            pages.push(SnapshotPage {
                session,
                sub,
                rows,
                seq: mark,
                last,
                bytes,
            });
        }
        pages.sort_by(|a, b| (&a.session, a.sub).cmp(&(&b.session, b.sub)));
        Ok(ClosureOutput {
            pages,
            starved,
            oversized,
            rows_read,
            bytes_read,
        })
    }

    /// The next waiting row in round robin order: the first session after `last_session` that
    /// has an unpicked waiting row and was not skipped, and inside it the first subscription
    /// after the one served last.
    fn next_pick(
        &self,
        last_session: &Option<S>,
        last_sub: &BTreeMap<S, SubId>,
        skipped: &BTreeSet<S>,
        picked: &BTreeSet<(S, SubId, RowId)>,
    ) -> Option<(S, SubId, RowId)> {
        let after: Vec<&S> = match last_session {
            Some(last) => self
                .subs
                .range::<S, _>((std::ops::Bound::Excluded(last), std::ops::Bound::Unbounded))
                .chain(self.subs.range::<S, _>(..=last))
                .map(|(s, _)| s)
                .collect(),
            None => self.subs.keys().collect(),
        };
        for session in after {
            if skipped.contains(session) {
                continue;
            }
            let subs = &self.subs[session];
            let ordered: Vec<(&SubId, &SubState)> = match last_sub.get(session) {
                Some(last) => subs
                    .range((std::ops::Bound::Excluded(*last), std::ops::Bound::Unbounded))
                    .chain(subs.range(..=*last))
                    .collect(),
                None => subs.iter().collect(),
            };
            for (sub, state) in ordered {
                let row = state
                    .waiting
                    .iter()
                    .find(|row| !picked.contains(&(session.clone(), *sub, **row)));
                if let Some(row) = row {
                    return Some((session.clone(), *sub, *row));
                }
            }
        }
        None
    }

    fn state(&self, session: &S, sub: SubId) -> Option<&SubState> {
        self.subs.get(session).and_then(|subs| subs.get(&sub))
    }

    fn state_mut(&mut self, session: &S, sub: SubId) -> Result<&mut SubState, WaitError> {
        self.subs
            .get_mut(session)
            .and_then(|subs| subs.get_mut(&sub))
            .ok_or(WaitError::Unknown { sub })
    }
}

impl<S: Ord + Clone> Default for WaitingRows<S> {
    fn default() -> Self {
        Self::new()
    }
}
