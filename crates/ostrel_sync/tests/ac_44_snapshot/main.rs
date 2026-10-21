//! AC-44, snapshot consistency (ARCHITECTURE 5.4 "Snapshot consistency", "Offline and
//! reconnect", "Quiet phase"; D151, D154, D156 to D162, D176).
//!
//! A simulated server process: writers commit transactions of one or more ops under the write
//! lock, the broadcast queue releases commits through the [`BroadcastFence`] and re-evaluates
//! scopes (rows enter, leave, become unreadable for one session or are deleted), closures serve
//! waiting rows as `Snapshot` pages, and each session's frames pass its send queue to a
//! simulated client. The client applies a page by replacing the row, an op by appending its tag
//! and an `Evict` by dropping the row, and drops an op at or below its `applied_server_seq`
//! (D62). Send queues are small enough that closures starve and live frames overload a session,
//! which then closes (D158) and reconnects: `Hello` takes M0 at a quiet point, the held rows are
//! live from M0, and outside the lock the session reads the `Evict` pages for held rows that
//! left the scope or were deleted and then the catch up up to M0, each in its own read
//! transaction, while live frames wait behind the catch up. Closures, registrations and M0 are
//! quiet point tasks and share quiet phases with each other while writers queue between them.
//!
//! At rest every open client must hold exactly the rows in its scope, each equal to the
//! committed row: an op missing there was lost, an op present twice was applied twice, a row
//! held outside the scope or after its deletion missed its `Evict`.
//!
//! The test functions carry no `ac_44_` prefix yet: AC-44 is still on the pending list of
//! `ci/checks/70_ac_coverage.sh`, and the AC also needs the revocation and client cases of
//! its other owners. The prefix is added when QA takes AC-44 off that list (T212).
//!
//! The gated runs schedule every step at random over many seeds and count how often each path
//! was taken, so a run that never reaches a path fails. The runs without the gate read a
//! snapshot while a writer is in flight or a commit is unreleased and show the faults the quiet
//! point prevents.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::convert::Infallible;

use ostrel_sync::broadcast_fence::{BroadcastFence, Commit, CommittedScope, ScopeExit};
use ostrel_sync::log_gate::{
    ClosureReader, EvictAction, QueueError, ReadRow, Release, Room, SendQueue, Ticket, WaitingRows,
    WriteLock,
};
use ostrel_sync::protocol::{
    Hlc, LoggedOp, Op, OpBody, OpId, ReplicaId, RowId, ServerMsg, ServerSeq, SubId,
};

type Session = u32;
const SUB: SubId = 1;
/// Encoded size of one row in a `Snapshot` page, of one op in an `Ops` frame and of one id in
/// an `Evict` page.
const ROW_BYTES: u64 = 300;
const OP_BYTES: u64 = 10;
const BIG: u64 = 8 * 1024 * 1024;
/// Reconnects per session and run, so that a session that overloads again ends closed.
const RECONNECTS: u32 = 2;

fn row_id(n: u64) -> RowId {
    RowId::new(Hlc::new(1_000 + n, 0, ReplicaId(9)).unwrap())
}

/// Row contents: the tags of the ops applied to it, in order.
type Content = Vec<u64>;

/// A committed row: its content and the sessions that may not read it.
#[derive(Clone, Debug, Default)]
struct RowState {
    content: Content,
    hidden: BTreeSet<Session>,
}

// ---------------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Client {
    rows: BTreeMap<u64, Content>,
    applied: u64,
    stream: u64,
    last_page: bool,
    closed: bool,
    /// Rows the reconnect `Evict` of this session removed and no page has served since.
    gone: BTreeSet<u64>,
    /// Live ops for such rows; they were queued behind the catch up and are dropped.
    ignored: u64,
    faults: Vec<String>,
}

enum Frame {
    Page {
        rows: Vec<(u64, Content)>,
        seq: u64,
        last: bool,
        bytes: u64,
    },
    Ops(Vec<(u64, u64, u64)>),
    Evict(Vec<u64>),
    /// The `Evict` page of a reconnect for held rows that left the scope or were deleted.
    Gone(Vec<u64>),
}

impl Frame {
    fn bytes(&self) -> u64 {
        match self {
            Frame::Page { bytes, .. } => *bytes,
            Frame::Ops(ops) => OP_BYTES * ops.len() as u64,
            Frame::Evict(rows) | Frame::Gone(rows) => OP_BYTES * rows.len() as u64,
        }
    }
}

impl Client {
    fn receive(&mut self, frame: &Frame) {
        match frame {
            Frame::Page {
                rows, seq, last, ..
            } => {
                self.order(*seq);
                for (n, content) in rows {
                    self.rows.insert(*n, content.clone());
                    self.gone.remove(n);
                }
                self.applied = self.applied.max(*seq);
                self.last_page |= *last;
            }
            Frame::Ops(ops) => {
                for &(seq, n, tag) in ops {
                    self.order(seq);
                    if seq <= self.applied {
                        continue;
                    }
                    match self.rows.get_mut(&n) {
                        Some(content) => content.push(tag),
                        // ASSUMPTION (5.4 is silent): the reconnect `Evict` reads the scope after
                        // M0, so live ops above M0 for a row it removed may follow it; the
                        // client drops them, the row comes back only as a `Snapshot` row.
                        None if self.gone.contains(&n) => self.ignored += 1,
                        None => self.faults.push(format!("op {seq} for row {n} not held")),
                    }
                    self.applied = seq;
                }
            }
            Frame::Evict(rows) => {
                for n in rows {
                    self.rows.remove(n);
                }
            }
            Frame::Gone(rows) => {
                for n in rows {
                    self.rows.remove(n);
                    self.gone.insert(*n);
                }
            }
        }
    }

    fn order(&mut self, seq: u64) {
        if seq < self.stream {
            self.faults
                .push(format!("stream went back from {} to {seq}", self.stream));
        }
        self.stream = self.stream.max(seq);
    }

    /// A new session after `Hello`: the rows and `applied_server_seq` stay, the stream starts
    /// again.
    fn reconnect(&mut self) {
        self.closed = false;
        self.last_page = false;
        self.gone.clear();
        self.stream = self.applied;
    }
}

// ---------------------------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Task {
    Writer(usize),
    Subscribe(Session),
    Closure,
    /// M0 of a reconnect.
    Hello(Session),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Queued,
    Holding,
    /// The first position of the transaction.
    Begun(u64),
    Finished(u64, bool),
    Done,
}

/// What an op does to its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Writes a field; the scope does not change.
    Write,
    /// Creates the row, readable by every session.
    Make,
    /// Deletes the row: it leaves every scope.
    Delete,
    /// Changes whether `session` (every session for `None`) may read the row: a filter field
    /// for all, a `see` dependency for one (revocation and grant).
    Visible {
        session: Option<Session>,
        visible: bool,
    },
    /// Flips whether `session` (every session for `None`, after the first one) may read the
    /// row, in commit order; it is logged as the [`Kind::Visible`] it resolves to.
    Toggle { session: Option<Session> },
}

/// One write transaction: its ops in order, each on a row. An op on a row that does not exist
/// (or a `Make` of one that does) is refused and rolls the whole transaction back.
#[derive(Clone, Debug)]
struct WriterPlan {
    ops: Vec<(u64, Kind)>,
    rollback: bool,
}

struct Writer {
    plan: WriterPlan,
    stage: Stage,
    ticket: Option<Ticket>,
}

enum Item {
    Commit(Commit),
    /// Closure pages with the generation of the session they were read for.
    Pages(Vec<(Session, u32, Frame)>),
}

/// The connection of a session.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Conn {
    Open,
    Closed,
    /// `Hello` waits for its M0.
    Hello,
    /// The `Evict` pages and the catch up are read outside the lock; live frames wait behind.
    CatchUp {
        m0: u64,
        from: u64,
        held: Vec<u64>,
        evicted: Option<BTreeSet<u64>>,
    },
}

/// How often each path was taken; a gated run over many seeds must take all of them.
#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    evict_sent: u64,
    evict_dropped: u64,
    unreadable_reads: u64,
    deleted_reads: u64,
    starved: u64,
    overloaded: u64,
    disconnects: u64,
    multi_op_commits: u64,
    refused: u64,
    reconnects: u64,
    catch_up_evicts: u64,
    catch_up_ops: u64,
    /// Catch up pages queued at the front of the stream with only live frames behind them.
    catch_up_front: u64,
    /// Live ops a client dropped for a row that the reconnect `Evict` removed.
    ops_after_gone: u64,
    /// Quiet point tasks granted inside a phase that an earlier one began.
    phase_shared: u64,
    /// Quiet point tasks granted before a writer queued earlier.
    overtook_writer: u64,
}

impl Stats {
    fn add(&mut self, o: &Stats) {
        self.evict_sent += o.evict_sent;
        self.evict_dropped += o.evict_dropped;
        self.unreadable_reads += o.unreadable_reads;
        self.deleted_reads += o.deleted_reads;
        self.starved += o.starved;
        self.overloaded += o.overloaded;
        self.disconnects += o.disconnects;
        self.multi_op_commits += o.multi_op_commits;
        self.refused += o.refused;
        self.reconnects += o.reconnects;
        self.catch_up_evicts += o.catch_up_evicts;
        self.catch_up_ops += o.catch_up_ops;
        self.catch_up_front += o.catch_up_front;
        self.ops_after_gone += o.ops_after_gone;
        self.phase_shared += o.phase_shared;
        self.overtook_writer += o.overtook_writer;
    }
}

struct Server {
    gated: bool,
    sessions: Vec<Session>,
    limits: BTreeMap<Session, u64>,
    next_pos: u64,
    push_from: u64,
    committed: BTreeMap<u64, RowState>,
    /// The op log: position to row and kind.
    log: BTreeMap<u64, (u64, Kind)>,
    ids: BTreeMap<RowId, u64>,
    lock: WriteLock<Task>,
    waiting: WaitingRows<Session>,
    fence: BroadcastFence,
    pending: VecDeque<Item>,
    queues: BTreeMap<Session, SendQueue>,
    frames: BTreeMap<Session, VecDeque<Frame>>,
    /// Live frames that wait behind a catch up; they count to the send queue (D161).
    behind: BTreeMap<Session, VecDeque<Frame>>,
    clients: BTreeMap<Session, Client>,
    conns: BTreeMap<Session, Conn>,
    generation: BTreeMap<Session, u32>,
    reconnects_left: BTreeMap<Session, u32>,
    subscribed: BTreeMap<Session, Stage>,
    /// Candidate ids read by a read transaction that has begun and not yet added them.
    candidates: BTreeMap<Session, Vec<RowId>>,
    writers: Vec<Writer>,
    faults: Vec<String>,
    stats: Stats,
}

/// Scope re-evaluation at the release of a commit, from the ops the commit logged.
struct Scope<'a> {
    waiting: &'a mut WaitingRows<Session>,
    sessions: &'a [Session],
    log: &'a BTreeMap<u64, (u64, Kind)>,
    stats: &'a mut Stats,
    entered: bool,
}

impl CommittedScope for Scope<'_> {
    type Session = Session;

    fn scope_exits(&mut self, commit: &Commit) -> Vec<ScopeExit<Session>> {
        let mut exits = Vec::new();
        for op in &commit.ops {
            let row = op.op.row;
            let affected = |only: Option<Session>| {
                self.sessions
                    .iter()
                    .copied()
                    .filter(move |s| only.is_none_or(|o| o == *s))
                    .collect::<Vec<_>>()
            };
            let (sessions, visible) = match self.log[&op.seq.0].1 {
                Kind::Write => continue,
                Kind::Make => (affected(None), true),
                Kind::Delete => (affected(None), false),
                Kind::Visible { session, visible } => (affected(session), visible),
                Kind::Toggle { .. } => unreachable!("logged as Visible"),
            };
            for s in sessions {
                if visible {
                    // A session without the subscription (closed, not yet registered) has none.
                    self.entered |= self.waiting.enter(&s, SUB, row).is_ok();
                    continue;
                }
                match self.waiting.evict(&s, SUB, row) {
                    EvictAction::Send => {
                        self.stats.evict_sent += 1;
                        exits.push(ScopeExit::Evict {
                            session: s,
                            sub: SUB,
                            model: 0,
                            rows: vec![row],
                        });
                    }
                    EvictAction::Drop => self.stats.evict_dropped += 1,
                    EvictAction::Ignore => {}
                }
            }
        }
        exits
    }

    fn readers(&self, op: &LoggedOp, _upto: ServerSeq) -> Vec<Session> {
        self.sessions
            .iter()
            .copied()
            .filter(|s| self.waiting.is_live(s, SUB, op.op.row, op.seq))
            .collect()
    }
}

struct Reader<'a> {
    committed: &'a BTreeMap<u64, RowState>,
    ids: &'a BTreeMap<RowId, u64>,
    unreadable: u64,
    deleted: u64,
}

impl ClosureReader<Session> for Reader<'_> {
    type Row = (u64, Content);
    type Error = Infallible;

    fn read(
        &mut self,
        session: &Session,
        _sub: SubId,
        row: RowId,
    ) -> Result<Option<ReadRow<Self::Row>>, Infallible> {
        let n = self.ids[&row];
        let Some(state) = self.committed.get(&n) else {
            self.deleted += 1;
            return Ok(None);
        };
        let visible = !state.hidden.contains(session);
        if !visible {
            self.unreadable += 1;
        }
        Ok(Some(ReadRow {
            row: (n, state.content.clone()),
            encoded_bytes: ROW_BYTES,
            read_bytes: ROW_BYTES,
            visible,
        }))
    }
}

fn per_session<V: Clone>(sessions: &[Session], v: V) -> BTreeMap<Session, V> {
    sessions.iter().map(|s| (*s, v.clone())).collect()
}

/// An op of a transaction applied to its row: position, row, the kind it is logged as, and
/// the row after it (`None` once deleted).
type Resolved = (u64, u64, Kind, Option<RowState>);

/// Positions and rows of one simulated run.
struct Setup {
    rows: u64,
    /// Sessions and the limits of their send queues.
    sessions: Vec<(Session, u64)>,
    writers: Vec<WriterPlan>,
}

impl Server {
    fn new(setup: &Setup, gated: bool) -> Self {
        let mut ids = BTreeMap::new();
        let mut committed = BTreeMap::new();
        for n in 0..setup.rows {
            committed.insert(n, RowState::default());
        }
        let ops: usize = setup.writers.iter().map(|w| w.ops.len()).sum();
        for n in 0..setup.rows + ops as u64 + 1 {
            ids.insert(row_id(n), n);
        }
        let sessions: Vec<Session> = setup.sessions.iter().map(|s| s.0).collect();
        Self {
            gated,
            limits: setup.sessions.iter().copied().collect(),
            queues: setup
                .sessions
                .iter()
                .map(|&(s, limit)| (s, SendQueue::with_limit(limit)))
                .collect(),
            clients: sessions.iter().map(|s| (*s, Client::default())).collect(),
            frames: sessions.iter().map(|s| (*s, VecDeque::new())).collect(),
            behind: sessions.iter().map(|s| (*s, VecDeque::new())).collect(),
            conns: sessions.iter().map(|s| (*s, Conn::Open)).collect(),
            generation: per_session(&sessions, 0),
            reconnects_left: per_session(&sessions, RECONNECTS),
            subscribed: per_session(&sessions, Stage::Idle),
            candidates: BTreeMap::new(),
            sessions,
            next_pos: 1,
            push_from: 1,
            committed,
            log: BTreeMap::new(),
            ids,
            lock: WriteLock::with_bound(4),
            waiting: WaitingRows::new(),
            fence: BroadcastFence::new(ServerSeq(1), 1000),
            pending: VecDeque::new(),
            writers: setup
                .writers
                .iter()
                .map(|plan| Writer {
                    plan: plan.clone(),
                    stage: Stage::Idle,
                    ticket: None,
                })
                .collect(),
            faults: Vec::new(),
            stats: Stats::default(),
        }
    }

    fn want_closure(&mut self) {
        if self.waiting.request_closure() {
            self.lock.request_quiet(Task::Closure);
        }
    }

    /// The session's queue overflowed: it gets `Error Overloaded` and closes (D158). Frames not
    /// yet written are dropped; the client may reconnect.
    fn overload(&mut self, s: Session) {
        self.stats.overloaded += 1;
        self.close(s);
    }

    /// The session closes; frames not yet written are lost and the client keeps its rows.
    fn close(&mut self, s: Session) {
        // A test may close the session directly; an overflowing frame closes its queue.
        let _ = self.queues.get_mut(&s).unwrap().push_now(u64::MAX);
        self.waiting.close_session(&s);
        self.frames.get_mut(&s).unwrap().clear();
        self.behind.get_mut(&s).unwrap().clear();
        self.clients.get_mut(&s).unwrap().closed = true;
        self.conns.insert(s, Conn::Closed);
    }

    fn is_closed(&self, s: Session) -> bool {
        self.clients[&s].closed
    }

    /// Queues a frame into the session stream, behind a running catch up if there is one.
    fn deliver(&mut self, s: Session, frame: Frame) {
        if matches!(self.conns[&s], Conn::CatchUp { .. }) {
            self.behind.get_mut(&s).unwrap().push_back(frame);
        } else {
            self.frames.get_mut(&s).unwrap().push_back(frame);
        }
    }

    // ---- writers ----

    fn begin(&mut self, i: usize) -> u64 {
        let pos = self.next_pos;
        self.next_pos += self.writers[i].plan.ops.len() as u64;
        self.writers[i].stage = Stage::Begun(pos);
        pos
    }

    /// Applies the ops of writer `i` to a copy of the touched rows; None if one is refused.
    fn resolve(&self, i: usize, first: u64) -> Option<Vec<Resolved>> {
        let mut rows: BTreeMap<u64, Option<RowState>> = BTreeMap::new();
        let mut out = Vec::new();
        for (k, &(row, kind)) in self.writers[i].plan.ops.iter().enumerate() {
            let pos = first + k as u64;
            let state = rows
                .entry(row)
                .or_insert_with(|| self.committed.get(&row).cloned());
            let kind = match (kind, state.as_mut()) {
                (Kind::Make, None) => {
                    *state = Some(RowState {
                        content: vec![pos],
                        hidden: BTreeSet::new(),
                    });
                    Kind::Make
                }
                (Kind::Make, Some(_)) | (_, None) => return None,
                (Kind::Delete, Some(_)) => {
                    *state = None;
                    Kind::Delete
                }
                (Kind::Write, Some(st)) => {
                    st.content.push(pos);
                    Kind::Write
                }
                (Kind::Toggle { session }, Some(st)) => {
                    let probe = session.unwrap_or(self.sessions[0]);
                    let visible = st.hidden.contains(&probe);
                    self.set_visible(st, pos, session, visible);
                    Kind::Visible { session, visible }
                }
                (Kind::Visible { session, visible }, Some(st)) => {
                    self.set_visible(st, pos, session, visible);
                    Kind::Visible { session, visible }
                }
            };
            out.push((pos, row, kind, state.clone()));
        }
        Some(out)
    }

    fn set_visible(&self, st: &mut RowState, pos: u64, only: Option<Session>, visible: bool) {
        st.content.push(pos);
        for s in self
            .sessions
            .iter()
            .filter(|s| only.is_none_or(|o| o == **s))
        {
            if visible {
                st.hidden.remove(s);
            } else {
                st.hidden.insert(*s);
            }
        }
    }

    fn finish(&mut self, i: usize) {
        let Stage::Begun(first) = self.writers[i].stage else {
            panic!("writer {i} has not begun")
        };
        let resolved = if self.writers[i].plan.rollback {
            None
        } else {
            self.resolve(i, first)
        };
        let Some(ops) = resolved else {
            if !self.writers[i].plan.rollback {
                self.stats.refused += 1;
            }
            // memory and SQLite give the positions again (D154).
            let len = self.writers[i].plan.ops.len() as u64;
            assert_eq!(
                first + len,
                self.next_pos,
                "only the last positions are given again"
            );
            self.next_pos = first;
            self.writers[i].stage = Stage::Finished(first, false);
            return;
        };
        if ops.len() > 1 {
            self.stats.multi_op_commits += 1;
        }
        for (pos, row, kind, state) in ops {
            self.log.insert(pos, (row, kind));
            match state {
                Some(state) => self.committed.insert(row, state),
                None => self.committed.remove(&row),
            };
        }
        self.writers[i].stage = Stage::Finished(first, true);
    }

    fn release(&mut self, i: usize) {
        let Stage::Finished(pos, committed) = self.writers[i].stage else {
            panic!("writer {i} has not finished")
        };
        if committed {
            let len = self.writers[i].plan.ops.len() as u64;
            let last = pos + len - 1;
            let first = if self.gated { self.push_from } else { pos };
            self.push_from = last + 1;
            let ops = (pos..=last)
                .map(|seq| {
                    let (row, kind) = self.log[&seq];
                    LoggedOp {
                        seq: ServerSeq(seq),
                        op: Op {
                            id: OpId {
                                replica: ReplicaId(1),
                                seq: u32::try_from(seq).unwrap(),
                            },
                            hlc: Hlc::new(2_000 + seq, 0, ReplicaId(1)).unwrap(),
                            model: 0,
                            row: row_id(row),
                            body: match kind {
                                Kind::Make => OpBody::Make { fields: vec![] },
                                // The body is not read; the kind of the op is kept by position.
                                _ => OpBody::Drop,
                            },
                        },
                    }
                })
                .collect();
            self.pending.push_back(Item::Commit(Commit {
                first: ServerSeq(first),
                last: ServerSeq(last),
                ops,
            }));
        }
        if let Some(ticket) = self.writers[i].ticket {
            let outcome = if committed {
                Release::Committed
            } else {
                Release::Nothing
            };
            self.lock.release(ticket, outcome).unwrap();
        }
        self.writers[i].stage = Stage::Done;
    }

    // ---- lock holders that do not await anything ----

    /// The lock decides whether a queued task gets it; the simulation never checks for it.
    fn grant(&mut self) -> bool {
        let in_phase = self.lock.in_quiet_phase();
        let Some((ticket, task)) = self.lock.grant() else {
            return false;
        };
        if !matches!(task, Task::Writer(_)) {
            self.quiet_granted(ticket, in_phase);
        }
        match task {
            Task::Writer(i) => self.writers[i].stage = Stage::Holding,
            Task::Subscribe(s) => {
                if !self.is_closed(s) {
                    self.waiting.subscribe(s, SUB).unwrap();
                }
                self.subscribed.insert(s, Stage::Holding);
                self.lock.release(ticket, Release::Nothing).unwrap();
            }
            Task::Closure => {
                // Quiet point: the holder has no transaction open (5.4).
                let mark = self.next_pos - 1;
                let starved = self.closure(mark, false);
                self.lock.release(ticket, Release::Nothing).unwrap();
                // A starved session asks again when its queue drains.
                let again = self
                    .queues
                    .keys()
                    .any(|s| self.waiting.has_work_for(s) && !starved.contains(s));
                if again {
                    self.want_closure();
                }
            }
            Task::Hello(s) => {
                self.hello(s);
                self.lock.release(ticket, Release::Nothing).unwrap();
            }
        }
        true
    }

    /// Checks the quiet point of a granted quiet point task and counts the phase paths.
    fn quiet_granted(&mut self, ticket: Ticket, in_phase: bool) {
        let in_flight = self.writers.iter().any(|w| {
            matches!(
                w.stage,
                Stage::Holding | Stage::Begun(_) | Stage::Finished(..)
            )
        });
        if self.lock.unreleased() > 0 || in_flight {
            self.faults.push(format!(
                "quiet point task {ticket:?} granted with {} commits unreleased, writer in flight: \
                 {in_flight}",
                self.lock.unreleased()
            ));
        }
        if in_phase {
            self.stats.phase_shared += 1;
        }
        let overtaken = self
            .writers
            .iter()
            .any(|w| w.stage == Stage::Queued && w.ticket.is_some_and(|t| t < ticket));
        if overtaken {
            self.stats.overtook_writer += 1;
        }
    }

    /// M0 of a reconnect (5.4 "Offline and reconnect"): the held rows are live from M0, the
    /// subscription reads its candidates again, and the session reads its `Evict` pages and
    /// catch up outside the lock.
    fn hello(&mut self, s: Session) {
        let m0 = self.next_pos - 1;
        let held: Vec<u64> = self.clients[&s].rows.keys().copied().collect();
        self.waiting
            .resubscribe(s, SUB, held.iter().map(|n| row_id(*n)), ServerSeq(m0))
            .unwrap();
        self.queues
            .insert(s, SendQueue::with_limit(self.limits[&s]));
        *self.generation.get_mut(&s).unwrap() += 1;
        let client = self.clients.get_mut(&s).unwrap();
        let from = client.applied;
        client.reconnect();
        self.conns.insert(
            s,
            Conn::CatchUp {
                m0,
                from,
                held,
                evicted: None,
            },
        );
        self.subscribed.insert(s, Stage::Holding);
    }

    /// One closure at `mark`. Gated pages go through the broadcast queue behind everything
    /// released up to the mark; `send_now` sends them at once (the faulty variants).
    fn closure(&mut self, mark: u64, send_now: bool) -> Vec<Session> {
        let rooms: BTreeMap<Session, Room> =
            self.queues.iter().map(|(s, q)| (*s, q.space())).collect();
        let mut reader = Reader {
            committed: &self.committed,
            ids: &self.ids,
            unreadable: 0,
            deleted: 0,
        };
        let out = self
            .waiting
            .run_closure(ServerSeq(mark), &rooms, &mut reader)
            .unwrap();
        self.stats.unreadable_reads += reader.unreadable;
        self.stats.deleted_reads += reader.deleted;
        assert!(out.oversized.is_empty(), "rows fit every queue here");
        for s in &out.starved {
            self.stats.starved += 1;
            self.queues.get_mut(s).unwrap().mark_starved();
        }
        let starved = out.starved;
        let mut frames = Vec::new();
        for page in out.pages {
            match self
                .queues
                .get_mut(&page.session)
                .unwrap()
                .push_closure(page.bytes)
            {
                Ok(()) => {}
                Err(e) => panic!("closure page refused for session {}: {e}", page.session),
            }
            frames.push((
                page.session,
                self.generation[&page.session],
                Frame::Page {
                    rows: page.rows,
                    seq: page.seq.0,
                    last: page.last,
                    bytes: page.bytes,
                },
            ));
        }
        if send_now {
            for (s, _, frame) in frames {
                self.deliver(s, frame);
            }
        } else {
            self.pending.push_back(Item::Pages(frames));
        }
        starved
    }

    // ---- read connection ----

    /// Candidates are read by the filter only, without `see`, in a read transaction: the ids
    /// are those of the state when the read begins, and they are added later, after commits
    /// in between may have deleted, hidden or shown rows. Unreadable and deleted rows are
    /// dropped by the closure.
    fn read_candidates(&mut self, s: Session) {
        if self.is_closed(s) {
            self.candidates.remove(&s);
        } else if let Some(rows) = self.candidates.remove(&s) {
            self.waiting.add_candidates(&s, SUB, rows, true).unwrap();
        } else {
            self.waiting.begin_candidates(&s, SUB).unwrap();
            let rows: Vec<RowId> = self.committed.keys().map(|n| row_id(*n)).collect();
            self.candidates.insert(s, rows);
            return;
        }
        self.subscribed.insert(s, Stage::Done);
        self.want_closure();
    }

    /// Whether the session may read the row in the committed state now.
    fn readable(&self, s: Session, n: u64) -> bool {
        self.committed
            .get(&n)
            .is_some_and(|state| !state.hidden.contains(&s))
    }

    /// One read transaction of a reconnect: first the `Evict` page for held rows that left the
    /// scope or were deleted, then the catch up `(from, m0]` of the held rows still in scope. The
    /// state is read when the step runs, so each step may see later commits than M0 and than the
    /// step before it. When the catch up is queued, the live frames behind it follow.
    fn catch_up(&mut self, s: Session) {
        let Conn::CatchUp {
            m0,
            from,
            held,
            evicted,
        } = self.conns[&s].clone()
        else {
            panic!("session {s} runs no catch up")
        };
        let frame = match evicted {
            None => {
                let gone: Vec<u64> = held
                    .iter()
                    .copied()
                    .filter(|n| !self.readable(s, *n))
                    .collect();
                for n in &gone {
                    // The row is no longer live from M0 (5.4).
                    self.waiting.evict(&s, SUB, row_id(*n));
                }
                self.stats.catch_up_evicts += gone.len() as u64;
                let frame = (!gone.is_empty()).then(|| Frame::Gone(gone.clone()));
                self.conns.insert(
                    s,
                    Conn::CatchUp {
                        m0,
                        from,
                        held,
                        evicted: Some(gone.into_iter().collect()),
                    },
                );
                frame
            }
            Some(gone) => {
                let ops: Vec<(u64, u64, u64)> = self
                    .log
                    .range(from.min(m0) + 1..m0 + 1)
                    .filter(|(_, (n, _))| {
                        held.contains(n) && !gone.contains(n) && self.readable(s, *n)
                    })
                    .map(|(seq, (n, _))| (*seq, *n, *seq))
                    .collect();
                self.stats.catch_up_ops += ops.len() as u64;
                self.conns.insert(s, Conn::Open);
                (!ops.is_empty()).then_some(Frame::Ops(ops))
            }
        };
        if let Some(frame) = frame {
            self.queue_catch_up(s, frame);
        }
        if self.conns[&s] == Conn::Open {
            let behind: Vec<Frame> = self.behind.get_mut(&s).unwrap().drain(..).collect();
            self.frames.get_mut(&s).unwrap().extend(behind);
        }
    }

    /// A catch up frame waits for room (D159). Live frames behind it count to the queue but
    /// cannot be written before it; with nothing ahead of it unwritten, waiting would never end,
    /// so it is queued at the front of the stream like a frame on an empty queue.
    fn queue_catch_up(&mut self, s: Session, frame: Frame) {
        let queue = self.queues.get_mut(&s).unwrap();
        let fits = queue.push_when_room(frame.bytes()).unwrap();
        if !fits {
            assert!(
                self.frames[&s].is_empty(),
                "the simulation runs a catch up step only on a drained stream"
            );
            self.stats.catch_up_front += 1;
            queue.push_at_front(frame.bytes()).unwrap();
        }
        self.frames.get_mut(&s).unwrap().push_back(frame);
    }

    // ---- broadcast queue ----

    fn broadcast(&mut self) {
        match self.pending.pop_front().expect("broadcast enabled") {
            Item::Pages(frames) => {
                for (s, generation, frame) in frames {
                    // Pages of a closed session die with it; a new session reads its own.
                    if !self.is_closed(s) && self.generation[&s] == generation {
                        self.deliver(s, frame);
                    }
                }
            }
            Item::Commit(commit) => {
                let mut scope = Scope {
                    waiting: &mut self.waiting,
                    sessions: &self.sessions,
                    log: &self.log,
                    stats: &mut self.stats,
                    entered: false,
                };
                let out = self.fence.push(commit, &mut scope).unwrap();
                let entered = scope.entered;
                for delivery in out {
                    let s = delivery.session;
                    let frame = match delivery.msg {
                        ServerMsg::Ops(ops) => Frame::Ops(
                            ops.iter()
                                .map(|o| (o.seq.0, self.ids[&o.op.row], o.op.id.seq.into()))
                                .collect(),
                        ),
                        ServerMsg::Evict { rows, .. } => {
                            Frame::Evict(rows.iter().map(|r| self.ids[r]).collect())
                        }
                        other => panic!("unexpected delivery {other:?}"),
                    };
                    // Scope exits and live ops never wait (D162).
                    match self.queues.get_mut(&s).unwrap().push_now(frame.bytes()) {
                        Ok(()) => self.deliver(s, frame),
                        Err(QueueError::Overloaded) => self.overload(s),
                        Err(QueueError::Closed) => {}
                    }
                }
                if self.gated {
                    self.lock.commit_released().unwrap();
                }
                if entered {
                    self.want_closure();
                }
            }
        }
    }

    fn drain(&mut self, s: Session) {
        let frame = self.frames.get_mut(&s).unwrap().pop_front().unwrap();
        self.clients.get_mut(&s).unwrap().receive(&frame);
        if self.queues.get_mut(&s).unwrap().written(frame.bytes()) {
            self.want_closure();
        }
    }

    // ---- scheduler ----

    fn enabled(&self) -> Vec<Action> {
        let mut out = Vec::new();
        if self.lock.queued() > 0 {
            out.push(Action::Grant);
        }
        for (i, w) in self.writers.iter().enumerate() {
            match w.stage {
                Stage::Idle => out.push(Action::Request(i)),
                Stage::Holding => out.push(Action::Begin(i)),
                Stage::Begun(_) => out.push(Action::Finish(i)),
                Stage::Finished(..) => out.push(Action::Release(i)),
                Stage::Queued | Stage::Done => {}
            }
        }
        for (s, stage) in &self.subscribed {
            match stage {
                Stage::Idle if !self.is_closed(*s) => out.push(Action::Subscribe(*s)),
                Stage::Holding
                    if self.is_closed(*s)
                        || self.candidates.contains_key(s)
                        || self.waiting.wants_candidates(s, SUB) =>
                {
                    out.push(Action::Candidates(*s));
                }
                _ => {}
            }
        }
        for (s, conn) in &self.conns {
            match conn {
                Conn::Closed
                    if self.reconnects_left[s] > 0 && self.subscribed[s] == Stage::Done =>
                {
                    out.push(Action::Reconnect(*s));
                }
                // The network may drop an open session at any time.
                Conn::Open
                    if self.reconnects_left[s] > 0
                        && self.subscribed[s] == Stage::Done
                        && !self.is_closed(*s) =>
                {
                    out.push(Action::Disconnect(*s));
                }
                // One read transaction at a time, once the frames ahead of it are written.
                Conn::CatchUp { .. } if self.frames[s].is_empty() => {
                    out.push(Action::CatchUp(*s));
                }
                _ => {}
            }
        }
        if !self.pending.is_empty() {
            out.push(Action::Broadcast);
        }
        for (s, frames) in &self.frames {
            if !frames.is_empty() {
                out.push(Action::Drain(*s));
            }
        }
        out
    }

    /// Applies `action`; false when it was a grant the lock refused.
    fn apply(&mut self, action: Action) -> bool {
        match action {
            Action::Grant => return self.grant(),
            Action::Request(i) => {
                self.writers[i].ticket = Some(self.lock.request(Task::Writer(i)));
                self.writers[i].stage = Stage::Queued;
            }
            Action::Begin(i) => {
                self.begin(i);
            }
            Action::Finish(i) => self.finish(i),
            Action::Release(i) => self.release(i),
            Action::Subscribe(s) => {
                self.lock.request_quiet(Task::Subscribe(s));
                self.subscribed.insert(s, Stage::Queued);
            }
            Action::Candidates(s) => self.read_candidates(s),
            Action::Reconnect(s) => {
                self.stats.reconnects += 1;
                let left = self.reconnects_left.get_mut(&s).unwrap();
                *left = left.saturating_sub(1);
                self.conns.insert(s, Conn::Hello);
                self.lock.request_quiet(Task::Hello(s));
            }
            Action::CatchUp(s) => self.catch_up(s),
            Action::Disconnect(s) => {
                self.stats.disconnects += 1;
                self.close(s);
            }
            Action::Broadcast => self.broadcast(),
            Action::Drain(s) => self.drain(s),
        }
        true
    }

    /// Applies the first enabled action that makes progress until none does. It requests no
    /// writer and reconnects no session; the test does that.
    fn settle(&mut self) {
        'outer: for _ in 0..100_000 {
            for action in self.enabled() {
                if matches!(
                    action,
                    Action::Request(_) | Action::Reconnect(_) | Action::Disconnect(_)
                ) {
                    continue;
                }
                if self.apply(action) {
                    continue 'outer;
                }
            }
            return;
        }
        panic!("no quiescence");
    }

    /// The paths taken, with the live ops clients dropped after a reconnect `Evict`.
    fn stats(&self) -> Stats {
        Stats {
            ops_after_gone: self.clients.values().map(|c| c.ignored).sum(),
            ..self.stats
        }
    }

    /// Faults of the server and of every open client against its scope at rest.
    fn faults(&self) -> Vec<String> {
        let mut faults = self.faults.clone();
        for (s, client) in &self.clients {
            faults.extend(client.faults.iter().map(|f| format!("session {s}: {f}")));
            if client.closed {
                continue;
            }
            if !client.last_page {
                faults.push(format!("session {s}: no last page"));
            }
            for n in client.rows.keys() {
                if !self.committed.contains_key(n) {
                    faults.push(format!("session {s}: row {n} held after its deletion"));
                }
            }
            for (n, state) in &self.committed {
                let in_scope = !state.hidden.contains(s);
                match (in_scope, client.rows.get(n)) {
                    (true, None) => faults.push(format!("session {s}: row {n} never served")),
                    (false, Some(_)) => {
                        faults.push(format!("session {s}: row {n} held outside its scope"));
                    }
                    (false, None) => {}
                    (true, Some(got)) => {
                        for tag in &state.content {
                            match got.iter().filter(|t| *t == tag).count() {
                                0 => faults.push(format!("session {s}: row {n} lost op {tag}")),
                                1 => {}
                                k => faults.push(format!(
                                    "session {s}: row {n} applied op {tag} {k} times"
                                )),
                            }
                        }
                        if got != &state.content {
                            faults.push(format!(
                                "session {s}: row {n} is {got:?}, committed {:?}",
                                state.content
                            ));
                        }
                    }
                }
            }
        }
        faults
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Grant,
    Request(usize),
    Begin(usize),
    Finish(usize),
    Release(usize),
    Subscribe(Session),
    Candidates(Session),
    Reconnect(Session),
    Disconnect(Session),
    CatchUp(Session),
    Broadcast,
    Drain(Session),
}

/// xorshift64*, enough to pick among enabled steps reproducibly.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let x = self.0.wrapping_mul(0x2545_f491_4f6c_dd1d);
        (x % n as u64) as usize
    }
}

/// Runs `server` with random scheduling until nothing is enabled. Returns the faults and the
/// paths taken.
///
/// Every other seed slows the broadcast queue down (it releases only one in eight of the times
/// it is picked), so commits pile up unreleased in front of closures and subscriptions. Every
/// third seed slows the readers of all sessions but the first down the same way once they hold
/// a row, so their frames pile up until they overload, reconnect with the rows they hold and
/// catch up. On one seed in five the network drops sessions now and then; they reconnect too.
/// A run ends when nothing but dropping a session is left to do.
fn run_random(mut server: Server, seed: u64) -> (Vec<String>, Stats) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let slow_broadcast = seed.is_multiple_of(2);
    let slow_readers = seed.is_multiple_of(3);
    let drops = seed % 5 == 1;
    let first = server.sessions[0];
    for _ in 0..100_000 {
        let mut enabled = server.enabled();
        if !drops || rng.below(16) != 0 {
            enabled.retain(|a| !matches!(a, Action::Disconnect(_)));
        }
        if enabled.is_empty() {
            return (server.faults(), server.stats());
        }
        let mut action = enabled[rng.below(enabled.len())];
        let slow_drain = |s: Session| s != first && !server.clients[&s].rows.is_empty();
        let slow = (slow_broadcast && action == Action::Broadcast)
            || (slow_readers && matches!(action, Action::Drain(s) if slow_drain(s)));
        if slow && enabled.len() > 1 && rng.below(8) != 0 {
            action = enabled[rng.below(enabled.len())];
        }
        server.apply(action);
    }
    (vec!["no quiescence".to_string()], server.stats())
}

fn run_gated(setup: &Setup, seed: u64) -> (Vec<String>, Stats) {
    run_random(Server::new(setup, true), seed)
}

/// Eight transactions of one to three ops on four rows: field writes, new rows (sometimes
/// written again in the same transaction), deletes, rows leaving and entering the scope of
/// every session (filter), and revocations and grants for one session (`see`). Scope changes
/// flip in commit order, so a row leaves and enters its scope again and again.
fn plans(seed: u64) -> Vec<WriterPlan> {
    let mut rng = Rng(seed | 1);
    let mut next_new = 4;
    (0..8)
        .map(|_| {
            let len = 1 + usize::from(rng.below(3) == 0) + usize::from(rng.below(4) == 0);
            let mut ops = Vec::new();
            for _ in 0..len {
                let row = rng.below(2) as u64;
                let op = match rng.below(12) {
                    0 => {
                        next_new += 1;
                        (next_new - 1, Kind::Make)
                    }
                    1 => (rng.below(4) as u64, Kind::Delete),
                    2..=3 => (row, Kind::Write),
                    4..=7 => (row, Kind::Toggle { session: None }),
                    _ => (
                        row,
                        Kind::Toggle {
                            session: Some(1 + rng.below(3) as u32),
                        },
                    ),
                };
                ops.push(op);
                if op.1 == Kind::Make && rng.below(2) == 0 {
                    ops.push((op.0, Kind::Write));
                }
            }
            WriterPlan {
                ops,
                rollback: rng.below(6) == 0,
            }
        })
        .collect()
}

fn one(row: u64, kind: Kind) -> WriterPlan {
    WriterPlan {
        ops: vec![(row, kind)],
        rollback: false,
    }
}

fn write(row: u64) -> WriterPlan {
    one(row, Kind::Write)
}

fn visible(row: u64, visible: bool) -> WriterPlan {
    one(
        row,
        Kind::Visible {
            session: None,
            visible,
        },
    )
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn snapshot_gated_closures_keep_every_client_equal_to_its_scope() {
    // Session 2 has room for three rows, so its closures starve and resume on drain. Session 3
    // has room for one row and one op, session 4 for one row only, so live frames overload
    // them; they reconnect with the rows they hold, catch up, and may overload again.
    let mut total = Stats::default();
    for seed in 1..=3_000u64 {
        let setup = Setup {
            rows: 4,
            sessions: vec![
                (1, BIG),
                (2, 1_000),
                (3, ROW_BYTES + OP_BYTES + OP_BYTES / 2),
                (4, ROW_BYTES + OP_BYTES / 2),
            ],
            writers: plans(seed),
        };
        let (faults, stats) = run_gated(&setup, seed);
        assert!(faults.is_empty(), "seed {seed}: {faults:?}");
        total.add(&stats);
    }
    // Every path the gate must survive was taken.
    let paths = [
        ("evict_sent", total.evict_sent),
        ("evict_dropped", total.evict_dropped),
        ("unreadable_reads", total.unreadable_reads),
        ("deleted_reads", total.deleted_reads),
        ("starved", total.starved),
        ("overloaded", total.overloaded),
        ("disconnects", total.disconnects),
        ("multi_op_commits", total.multi_op_commits),
        ("refused", total.refused),
        ("reconnects", total.reconnects),
        ("catch_up_evicts", total.catch_up_evicts),
        ("catch_up_ops", total.catch_up_ops),
        ("catch_up_front", total.catch_up_front),
        ("phase_shared", total.phase_shared),
        ("overtook_writer", total.overtook_writer),
    ];
    for (name, n) in paths {
        assert!(n > 0, "path {name} never taken: {total:?}");
    }
}

#[test]
fn snapshot_quiet_phases_among_many_writers_keep_every_client_equal_to_its_scope() {
    // Six sessions subscribe, so registrations, closures and M0 of reconnects stand in the lock
    // queue between sixteen writers and share phases; the broadcast queue is slow on every
    // other seed, so phases begin only after an emptying.
    let mut total = Stats::default();
    for seed in 1..=600u64 {
        let mut writers = plans(seed);
        writers.extend(plans(seed.wrapping_mul(31) + 7));
        let setup = Setup {
            rows: 4,
            sessions: (1..=6)
                .map(|s| {
                    (
                        s,
                        if s % 2 == 0 {
                            ROW_BYTES + OP_BYTES / 2
                        } else {
                            BIG
                        },
                    )
                })
                .collect(),
            writers,
        };
        let (faults, stats) = run_gated(&setup, seed);
        assert!(faults.is_empty(), "seed {seed}: {faults:?}");
        total.add(&stats);
    }
    assert!(total.phase_shared > 0, "{total:?}");
    assert!(total.overtook_writer > 0, "{total:?}");
    assert!(total.reconnects > 0, "{total:?}");
}

#[test]
fn snapshot_a_closure_waits_for_unreleased_commits_at_or_below_its_mark() {
    // RED kira #2455: row 0 waits; commit 1 takes it out of the scope and commit 2 back in,
    // both committed and unreleased when the closure asks for the lock. A closure at M = 2
    // would serve the row, the release of 1 would evict it again and the release of 2 would
    // let it wait; commit 3 then takes it out of the scope and only drops the waiting row, so
    // the client keeps a row outside its scope. Gated, the closure waits for both releases.
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, BIG)],
        writers: vec![visible(0, false), visible(0, true), visible(0, false)],
    };
    let mut server = Server::new(&setup, true);
    for action in [
        Action::Subscribe(1),
        Action::Grant,
        Action::Request(0),
        Action::Request(1),
        Action::Candidates(1),
        Action::Candidates(1),
        Action::Grant,
        Action::Begin(0),
        Action::Finish(0),
        Action::Release(0),
        Action::Grant,
        Action::Begin(1),
        Action::Finish(1),
        Action::Release(1),
        Action::Request(2),
    ] {
        assert!(server.apply(action), "{action:?}");
    }
    assert_eq!(server.lock.unreleased(), 2);
    // The closure is next in the queue; gated, the lock refuses it until both are released.
    server.apply(Action::Grant);
    server.settle();
    assert_eq!(server.faults(), Vec::<String>::new());
    assert!(!server.clients[&1].rows.contains_key(&0));
}

#[test]
fn snapshot_subscribe_while_a_lower_position_is_in_flight() {
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, 1 << 20)],
        writers: vec![write(0)],
    };
    let mut server = Server::new(&setup, true);
    server.apply(Action::Request(0));
    server.apply(Action::Grant);
    server.apply(Action::Begin(0));
    // The subscription and its closure queue behind the writer that holds position 1.
    server.apply(Action::Subscribe(1));
    assert!(!server.grant(), "granted while position 1 is in flight");
    server.apply(Action::Finish(0));
    server.apply(Action::Release(0));
    assert!(
        !server.grant(),
        "granted a quiet point while position 1 is unreleased"
    );
    server.settle();
    assert_eq!(server.faults(), Vec::<String>::new());
    assert_eq!(server.clients[&1].rows[&0], vec![1]);
}

#[test]
fn snapshot_rolled_back_position_is_covered_by_the_next_commit() {
    let plan = |rollback| WriterPlan {
        rollback,
        ops: vec![(0, Kind::Write), (0, Kind::Write)],
    };
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, 1 << 20)],
        writers: vec![plan(true), plan(false), plan(false)],
    };
    for seed in 1..=300u64 {
        let (faults, _) = run_gated(&setup, seed);
        assert!(faults.is_empty(), "seed {seed}: {faults:?}");
    }
}

#[test]
fn snapshot_row_entering_the_scope_is_served_once() {
    let setup = Setup {
        rows: 0,
        sessions: vec![(1, 1 << 20), (2, 1 << 20)],
        writers: vec![one(0, Kind::Make), write(0)],
    };
    // The second writer only writes row 0 once it exists.
    for seed in 1..=500u64 {
        let mut server = Server::new(&setup, true);
        let mut rng = Rng(seed | 1);
        loop {
            let enabled: Vec<Action> = server
                .enabled()
                .into_iter()
                .filter(|a| *a != Action::Request(1) || server.writers[0].stage == Stage::Done)
                .collect();
            if enabled.is_empty() {
                break;
            }
            server.apply(enabled[rng.below(enabled.len())]);
        }
        assert_eq!(server.faults(), Vec::<String>::new(), "seed {seed}");
    }
}

#[test]
fn snapshot_a_multi_op_commit_that_makes_writes_and_deletes_reaches_no_client() {
    // One transaction makes row 1, writes it and deletes row 0; another makes row 2 and hides
    // it in the same commit. Every interleaving ends with row 0 gone everywhere, row 1 served
    // once with both ops, and row 2 nowhere.
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, 1 << 20), (2, 1 << 20)],
        writers: vec![
            write(0),
            WriterPlan {
                ops: vec![(1, Kind::Make), (1, Kind::Write), (0, Kind::Delete)],
                rollback: false,
            },
            WriterPlan {
                ops: vec![
                    (2, Kind::Make),
                    (
                        2,
                        Kind::Visible {
                            session: None,
                            visible: false,
                        },
                    ),
                ],
                rollback: false,
            },
        ],
    };
    for seed in 1..=500u64 {
        let mut server = Server::new(&setup, true);
        let (faults, stats) = {
            let mut rng = Rng(seed | 1);
            loop {
                let enabled = server.enabled();
                if enabled.is_empty() {
                    break;
                }
                server.apply(enabled[rng.below(enabled.len())]);
            }
            (server.faults(), server.stats)
        };
        assert!(faults.is_empty(), "seed {seed}: {faults:?}");
        assert_eq!(stats.multi_op_commits, 2, "seed {seed}");
        for client in server.clients.values() {
            assert_eq!(client.rows.keys().copied().collect::<Vec<_>>(), vec![1]);
        }
    }
}

#[test]
fn snapshot_an_op_on_a_deleted_row_is_refused_and_nothing_reaches_the_client() {
    // The write of row 0 commits after its delete: it is refused, rolls back and gives its
    // position again (D154), so the next commit covers it.
    let setup = Setup {
        rows: 2,
        sessions: vec![(1, 1 << 20)],
        writers: vec![one(0, Kind::Delete), write(0), write(1)],
    };
    let mut server = Server::new(&setup, true);
    for action in [
        Action::Subscribe(1),
        Action::Grant,
        Action::Candidates(1),
        Action::Candidates(1),
        Action::Grant,
        Action::Request(0),
        Action::Request(1),
        Action::Request(2),
    ] {
        assert!(server.apply(action), "{action:?}");
    }
    server.settle();
    assert_eq!(server.faults(), Vec::<String>::new());
    assert_eq!(server.stats.refused, 1);
    assert_eq!(
        server.clients[&1].rows.iter().collect::<Vec<_>>(),
        vec![(&1, &vec![2])]
    );
}

/// Session 1 overloads after it holds rows 0 to 3; while it is away row 0 is written, row 1 is
/// deleted, row 2 is hidden from it, and row 4 is made. Writers in `during` commit between
/// M0 and the catch up.
fn reconnect_case(during: Vec<WriterPlan>) -> Server {
    let mut writers = vec![
        write(0),
        one(1, Kind::Delete),
        one(
            2,
            Kind::Visible {
                session: Some(1),
                visible: false,
            },
        ),
        one(4, Kind::Make),
    ];
    let first_during = writers.len();
    writers.extend(during);
    let setup = Setup {
        rows: 4,
        sessions: vec![(1, BIG), (2, BIG)],
        writers,
    };
    let mut server = Server::new(&setup, true);
    for s in [1, 2] {
        server.apply(Action::Subscribe(s));
    }
    server.settle();
    assert_eq!(server.clients[&1].rows.len(), 4);
    server.overload(1);
    for i in 0..first_during {
        server.apply(Action::Request(i));
        server.settle();
    }
    server.apply(Action::Reconnect(1));
    assert!(server.grant(), "M0 at a quiet point");
    assert!(matches!(server.conns[&1], Conn::CatchUp { m0: 4, .. }));
    for i in first_during..server.writers.len() {
        server.apply(Action::Request(i));
        // A closure queued by a row entering the scope goes first and needs the releases.
        while server.writers[i].stage != Stage::Holding {
            if !server.grant() {
                server.broadcast();
            }
        }
        server.apply(Action::Begin(i));
        server.apply(Action::Finish(i));
        server.apply(Action::Release(i));
    }
    server
}

#[test]
fn snapshot_reconnect_evicts_gone_rows_catches_up_and_serves_new_ones() {
    let mut server = reconnect_case(vec![]);
    server.settle();
    assert_eq!(server.faults(), Vec::<String>::new());
    let rows = &server.clients[&1].rows;
    assert_eq!(rows.keys().copied().collect::<Vec<_>>(), vec![0, 3, 4]);
    assert_eq!(rows[&0], vec![1], "the catch up brought op 1");
    assert_eq!(server.stats.catch_up_evicts, 2, "rows 1 and 2");
    assert_eq!(server.stats.catch_up_ops, 1);
}

#[test]
fn snapshot_reconnect_with_commits_between_m0_and_the_catch_up() {
    // Commits above M0 are read by the catch up steps (they read the state when they run), but
    // their ops reach the client only live, behind the catch up. Row 2 is shown again, row 0
    // hidden and shown, row 3 written and deleted.
    let during = vec![
        one(
            2,
            Kind::Visible {
                session: Some(1),
                visible: true,
            },
        ),
        visible(0, false),
        write(3),
        visible(0, true),
        one(3, Kind::Delete),
    ];
    let mut total = Stats::default();
    for seed in 1..=500u64 {
        let server = reconnect_case(during.clone());
        let (faults, stats) = run_random(server, seed);
        assert!(faults.is_empty(), "seed {seed}: {faults:?}");
        total.add(&stats);
    }
    // The `Evict` step read a delete or hide above M0 while live ops of the row waited behind
    // the catch up; the client dropped them (ASSUMPTION in `Client::receive`).
    assert!(total.ops_after_gone > 0, "{total:?}");
}

#[test]
fn snapshot_a_catch_up_never_waits_for_room_held_by_live_frames_behind_it() {
    // D161: live frames above M0 wait behind the catch up and count to the queue. With nothing
    // ahead of it, a catch up page that waited for room would wait forever, because only the
    // frames behind it hold the room; it is queued at the front of the stream instead.
    let mut queue = SendQueue::with_limit(ROW_BYTES + OP_BYTES / 2);
    queue.push_now(ROW_BYTES).unwrap();
    assert_eq!(queue.push_when_room(OP_BYTES), Ok(false));
    assert_eq!(queue.push_at_front(OP_BYTES), Ok(()));
    assert_eq!(queue.queued(), ROW_BYTES + OP_BYTES);

    // Session 1 holds rows 0 and 1 and closes. While it is away row 0 is written (1) and row 1
    // hidden from it (2). After M0 = 2 the `Evict` step drops row 1, then row 1 is shown again
    // (3) and its closure page waits behind the catch up with 300 of the 305 bytes; the catch
    // up of op 1 does not fit behind it.
    let setup = Setup {
        rows: 2,
        sessions: vec![(1, ROW_BYTES + OP_BYTES / 2)],
        writers: vec![
            write(0),
            one(
                1,
                Kind::Visible {
                    session: Some(1),
                    visible: false,
                },
            ),
            one(
                1,
                Kind::Visible {
                    session: Some(1),
                    visible: true,
                },
            ),
        ],
    };
    let mut server = Server::new(&setup, true);
    server.queues.insert(1, SendQueue::with_limit(BIG));
    server.apply(Action::Subscribe(1));
    server.settle();
    assert_eq!(server.clients[&1].rows.len(), 2);
    server.overload(1);
    for i in 0..2 {
        server.apply(Action::Request(i));
        server.settle();
    }
    server.apply(Action::Reconnect(1));
    assert!(server.grant(), "M0 at a quiet point");
    for action in [
        Action::CatchUp(1),
        Action::Drain(1),
        Action::Request(2),
        Action::Grant,
        Action::Begin(2),
        Action::Finish(2),
        Action::Release(2),
        Action::Broadcast,
        Action::Grant,
        Action::Broadcast,
    ] {
        assert!(server.apply(action), "{action:?}");
    }
    assert_eq!(
        server.behind[&1].len(),
        1,
        "the page waits behind the catch up"
    );
    assert_eq!(server.queues[&1].queued(), ROW_BYTES);
    server.apply(Action::CatchUp(1));
    assert_eq!(server.stats.catch_up_front, 1);
    server.settle();
    assert_eq!(server.faults(), Vec::<String>::new());
    assert_eq!(server.clients[&1].rows[&0], vec![1]);
    assert_eq!(server.clients[&1].rows[&1], vec![2, 3]);
}

#[test]
fn snapshot_without_gate_a_mark_at_the_highest_commit_loses_an_op() {
    // Two writers without the lock: position 1 is in flight while position 2 commits, and a
    // snapshot is read with the highest committed position as its mark.
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, 1 << 20)],
        writers: vec![write(0), write(0)],
    };
    let mut server = Server::new(&setup, false);
    server.waiting.subscribe(1, SUB).unwrap();
    // Begin and add the candidates in one go.
    server.read_candidates(1);
    server.read_candidates(1);
    server.begin(0);
    server.begin(1);
    server.finish(1);
    server.release(1);
    server.closure(2, true);
    server.finish(0);
    server.release(0);
    while !server.pending.is_empty() {
        server.broadcast();
    }
    while !server.frames[&1].is_empty() {
        server.drain(1);
    }
    let faults = server.faults();
    assert!(
        faults.contains(&"session 1: row 0 lost op 1".to_string()),
        "{faults:?}"
    );
}

#[test]
fn snapshot_without_gate_a_mark_at_the_last_release_repeats_an_op() {
    // Position 1 is committed and visible in the row but not yet released; a snapshot read
    // with the last released position as its mark contains it, and the op arrives again live.
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, 1 << 20)],
        writers: vec![write(0)],
    };
    let mut server = Server::new(&setup, false);
    server.waiting.subscribe(1, SUB).unwrap();
    // Begin and add the candidates in one go.
    server.read_candidates(1);
    server.read_candidates(1);
    server.begin(0);
    server.finish(0);
    server.release(0);
    server.closure(0, true);
    while !server.pending.is_empty() {
        server.broadcast();
    }
    while !server.frames[&1].is_empty() {
        server.drain(1);
    }
    let faults = server.faults();
    assert!(
        faults.contains(&"session 1: row 0 applied op 1 2 times".to_string()),
        "{faults:?}"
    );
}

#[test]
fn snapshot_without_gate_a_mark_above_an_unreleased_scope_exit_keeps_a_stale_row() {
    // kira's sequence without the release condition: the closure reads at the quiet point of
    // the writers (M = 2) while commits 1 and 2 are unreleased.
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, BIG)],
        writers: vec![visible(0, false), visible(0, true), visible(0, false)],
    };
    let mut server = Server::new(&setup, false);
    server.waiting.subscribe(1, SUB).unwrap();
    // Begin and add the candidates in one go.
    server.read_candidates(1);
    server.read_candidates(1);
    for i in 0..2 {
        server.begin(i);
        server.finish(i);
        server.release(i);
    }
    server.closure(2, false);
    server.begin(2);
    server.finish(2);
    server.release(2);
    while !server.pending.is_empty() {
        server.broadcast();
    }
    while !server.frames[&1].is_empty() {
        server.drain(1);
    }
    assert_eq!(
        server.faults(),
        vec!["session 1: row 0 held outside its scope".to_string()]
    );
}

#[test]
fn snapshot_the_gated_closure_waits_for_the_writer_in_flight() {
    // The same interleaving as the lost op case, under the lock: the closure is queued behind
    // the writer, so its mark is a quiet point.
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, 1 << 20)],
        writers: vec![write(0), write(0)],
    };
    let mut server = Server::new(&setup, true);
    server.apply(Action::Subscribe(1));
    server.apply(Action::Grant);
    server.apply(Action::Request(0));
    server.apply(Action::Candidates(1));
    server.apply(Action::Candidates(1));
    server.apply(Action::Request(1));
    server.apply(Action::Grant);
    server.apply(Action::Begin(0));
    assert!(!server.grant(), "the closure must wait");
    let mut rng = Rng(7);
    loop {
        let enabled = server.enabled();
        if enabled.is_empty() {
            break;
        }
        server.apply(enabled[rng.below(enabled.len())]);
    }
    assert_eq!(server.faults(), Vec::<String>::new());
    assert_eq!(server.clients[&1].rows[&0], vec![1, 2]);
}
