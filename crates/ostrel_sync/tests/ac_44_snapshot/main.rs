//! AC-44, snapshot consistency (ARCHITECTURE 5.4 "Snapshot consistency", D151, D154, D156 to
//! D162).
//!
//! A simulated server process: writers commit ops on rows under the write lock, the broadcast
//! queue releases commits through the [`BroadcastFence`] and re-evaluates scopes (rows enter,
//! leave or become unreadable for one session), closures serve waiting rows as `Snapshot` pages,
//! and each session's frames pass its send queue to a simulated client. The client applies a
//! page by replacing the row, an op by appending its tag and an `Evict` by dropping the row, and
//! drops an op at or below its `applied_server_seq` (D62). Send queues are small enough that
//! closures starve and live frames overload a session, which then closes (D158).
//!
//! At rest every open client must hold exactly the rows in its scope, each equal to the
//! committed row: an op missing there was lost, an op present twice was applied twice, a row
//! held outside the scope missed its `Evict`.
//!
//! The test functions carry no `ac_44_` prefix yet: AC-44 is still on the pending list of
//! `ci/checks/70_ac_coverage.sh`, and the AC also needs the revocation and client cases of
//! its other owners. The prefix is added when QA takes AC-44 off that list (T212).
//!
//! The gated runs schedule every step at random over many seeds and count how often each path
//! (served and waiting rows leaving the scope, unreadable rows, starved and overloaded sessions)
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
}

impl Frame {
    fn bytes(&self) -> u64 {
        match self {
            Frame::Page { bytes, .. } => *bytes,
            Frame::Ops(ops) => OP_BYTES * ops.len() as u64,
            Frame::Evict(rows) => OP_BYTES * rows.len() as u64,
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
        }
    }

    fn order(&mut self, seq: u64) {
        if seq < self.stream {
            self.faults
                .push(format!("stream went back from {} to {seq}", self.stream));
        }
        self.stream = self.stream.max(seq);
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Queued,
    Holding,
    Begun(u64),
    Finished(u64, bool),
    Done,
}

/// What a writer's op does to its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Writes a field; the scope does not change.
    Write,
    /// Creates the row, readable by every session.
    Make,
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

#[derive(Clone, Copy, Debug)]
struct WriterPlan {
    row: u64,
    kind: Kind,
    rollback: bool,
}

struct Writer {
    plan: WriterPlan,
    stage: Stage,
    ticket: Option<Ticket>,
}

enum Item {
    Commit(Commit),
    Pages(Vec<(Session, Frame)>),
}

/// How often each path was taken; a gated run over many seeds must take all of them.
#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    evict_sent: u64,
    evict_dropped: u64,
    unreadable_reads: u64,
    starved: u64,
    overloaded: u64,
}

struct Server {
    gated: bool,
    sessions: Vec<Session>,
    next_pos: u64,
    push_from: u64,
    committed: BTreeMap<u64, RowState>,
    kinds: BTreeMap<u64, Kind>,
    ids: BTreeMap<RowId, u64>,
    lock: WriteLock<Task>,
    waiting: WaitingRows<Session>,
    fence: BroadcastFence,
    pending: VecDeque<Item>,
    queues: BTreeMap<Session, SendQueue>,
    frames: BTreeMap<Session, VecDeque<Frame>>,
    clients: BTreeMap<Session, Client>,
    subscribed: BTreeMap<Session, Stage>,
    writers: Vec<Writer>,
    stats: Stats,
}

/// Scope re-evaluation at the release of a commit, from the op the commit logged.
struct Scope<'a> {
    waiting: &'a mut WaitingRows<Session>,
    sessions: &'a [Session],
    kinds: &'a BTreeMap<u64, Kind>,
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
            let (sessions, visible) = match self.kinds[&op.seq.0] {
                Kind::Write => continue,
                Kind::Make => (affected(None), true),
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
        for n in 0..setup.rows + setup.writers.len() as u64 + 1 {
            ids.insert(row_id(n), n);
        }
        let mut queues = BTreeMap::new();
        let mut clients = BTreeMap::new();
        let mut frames = BTreeMap::new();
        let mut subscribed = BTreeMap::new();
        for &(s, limit) in &setup.sessions {
            queues.insert(s, SendQueue::with_limit(limit));
            clients.insert(s, Client::default());
            frames.insert(s, VecDeque::new());
            subscribed.insert(s, Stage::Idle);
        }
        Self {
            gated,
            sessions: setup.sessions.iter().map(|s| s.0).collect(),
            next_pos: 1,
            push_from: 1,
            committed,
            kinds: BTreeMap::new(),
            ids,
            lock: WriteLock::with_bound(4),
            waiting: WaitingRows::new(),
            fence: BroadcastFence::new(ServerSeq(1), 1000),
            pending: VecDeque::new(),
            queues,
            frames,
            clients,
            subscribed,
            writers: setup
                .writers
                .iter()
                .map(|&plan| Writer {
                    plan,
                    stage: Stage::Idle,
                    ticket: None,
                })
                .collect(),
            stats: Stats::default(),
        }
    }

    fn want_closure(&mut self) {
        if self.waiting.request_closure() {
            self.lock.request_quiet(Task::Closure);
        }
    }

    /// The session's queue overflowed: it gets `Error Overloaded` and closes (D158). Frames not
    /// yet written are dropped; the client would reconnect, which this simulation leaves out.
    fn overload(&mut self, s: Session) {
        self.stats.overloaded += 1;
        self.waiting.close_session(&s);
        self.frames.get_mut(&s).unwrap().clear();
        self.clients.get_mut(&s).unwrap().closed = true;
    }

    fn is_closed(&self, s: Session) -> bool {
        self.clients[&s].closed
    }

    // ---- writers ----

    fn begin(&mut self, i: usize) -> u64 {
        let pos = self.next_pos;
        self.next_pos += 1;
        self.writers[i].stage = Stage::Begun(pos);
        pos
    }

    fn finish(&mut self, i: usize) {
        let Stage::Begun(pos) = self.writers[i].stage else {
            panic!("writer {i} has not begun")
        };
        let plan = self.writers[i].plan;
        if plan.rollback {
            // memory and SQLite give the position again (D154).
            assert_eq!(
                pos + 1,
                self.next_pos,
                "only the last position is given again"
            );
            self.next_pos = pos;
        } else {
            let kind = match plan.kind {
                Kind::Toggle { session } => {
                    let probe = session.unwrap_or(self.sessions[0]);
                    Kind::Visible {
                        session,
                        visible: self.committed[&plan.row].hidden.contains(&probe),
                    }
                }
                kind => kind,
            };
            self.kinds.insert(pos, kind);
            let sessions = self.sessions.clone();
            match kind {
                Kind::Make => {
                    self.committed.insert(
                        plan.row,
                        RowState {
                            content: vec![pos],
                            hidden: BTreeSet::new(),
                        },
                    );
                }
                Kind::Write => self.committed.get_mut(&plan.row).unwrap().content.push(pos),
                Kind::Visible { session, visible } => {
                    let state = self.committed.get_mut(&plan.row).unwrap();
                    state.content.push(pos);
                    for s in sessions
                        .into_iter()
                        .filter(|s| session.is_none_or(|o| o == *s))
                    {
                        if visible {
                            state.hidden.remove(&s);
                        } else {
                            state.hidden.insert(s);
                        }
                    }
                }
                Kind::Toggle { .. } => unreachable!("resolved above"),
            }
        }
        self.writers[i].stage = Stage::Finished(pos, !plan.rollback);
    }

    fn release(&mut self, i: usize) {
        let Stage::Finished(pos, committed) = self.writers[i].stage else {
            panic!("writer {i} has not finished")
        };
        if committed {
            let first = if self.gated { self.push_from } else { pos };
            self.push_from = pos + 1;
            let plan = self.writers[i].plan;
            let op = LoggedOp {
                seq: ServerSeq(pos),
                op: Op {
                    id: OpId {
                        replica: ReplicaId(1),
                        seq: u32::try_from(pos).unwrap(),
                    },
                    hlc: Hlc::new(2_000 + pos, 0, ReplicaId(1)).unwrap(),
                    model: 0,
                    row: row_id(plan.row),
                    body: match plan.kind {
                        Kind::Make => OpBody::Make { fields: vec![] },
                        // The body is not read; the kind of the op is kept by position.
                        _ => OpBody::Drop,
                    },
                },
            };
            self.pending.push_back(Item::Commit(Commit {
                first: ServerSeq(first),
                last: ServerSeq(pos),
                ops: vec![op],
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
        let Some((ticket, task)) = self.lock.grant() else {
            return false;
        };
        match task {
            Task::Writer(i) => {
                self.writers[i].stage = Stage::Holding;
                self.writers[i].ticket = Some(ticket);
            }
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
        }
        true
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
        };
        let out = self
            .waiting
            .run_closure(ServerSeq(mark), &rooms, &mut reader)
            .unwrap();
        self.stats.unreadable_reads += reader.unreadable;
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
                Frame::Page {
                    rows: page.rows,
                    seq: page.seq.0,
                    last: page.last,
                    bytes: page.bytes,
                },
            ));
        }
        if send_now {
            for (s, frame) in frames {
                self.frames.get_mut(&s).unwrap().push_back(frame);
            }
        } else {
            self.pending.push_back(Item::Pages(frames));
        }
        starved
    }

    // ---- read connection ----

    /// Candidates are read by the filter only, without `see`; unreadable rows are dropped by
    /// the closure.
    fn read_candidates(&mut self, s: Session) {
        if !self.is_closed(s) {
            self.waiting.begin_candidates(&s, SUB).unwrap();
            let rows: Vec<RowId> = self.committed.keys().map(|n| row_id(*n)).collect();
            self.waiting.add_candidates(&s, SUB, rows, true).unwrap();
        }
        self.subscribed.insert(s, Stage::Done);
        self.want_closure();
    }

    // ---- broadcast queue ----

    fn broadcast(&mut self) {
        match self.pending.pop_front().expect("broadcast enabled") {
            Item::Pages(frames) => {
                for (s, frame) in frames {
                    if !self.is_closed(s) {
                        self.frames.get_mut(&s).unwrap().push_back(frame);
                    }
                }
            }
            Item::Commit(commit) => {
                let mut scope = Scope {
                    waiting: &mut self.waiting,
                    sessions: &self.sessions,
                    kinds: &self.kinds,
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
                        Ok(()) => self.frames.get_mut(&s).unwrap().push_back(frame),
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
                Stage::Holding if self.is_closed(*s) || self.waiting.wants_candidates(s, SUB) => {
                    out.push(Action::Candidates(*s));
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
                self.lock.request(Task::Writer(i));
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
            Action::Broadcast => self.broadcast(),
            Action::Drain(s) => self.drain(s),
        }
        true
    }

    /// Applies the first enabled action that makes progress until none does.
    fn settle(&mut self) {
        'outer: for _ in 0..100_000 {
            for action in self.enabled() {
                if self.apply(action) {
                    continue 'outer;
                }
            }
            return;
        }
        panic!("no quiescence");
    }

    /// Faults of every open client against its scope at rest.
    fn faults(&self) -> Vec<String> {
        let mut faults = Vec::new();
        for (s, client) in &self.clients {
            faults.extend(client.faults.iter().map(|f| format!("session {s}: {f}")));
            if client.closed {
                continue;
            }
            if !client.last_page {
                faults.push(format!("session {s}: no last page"));
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

/// Runs `setup` gated with random scheduling until nothing is enabled. Returns the faults and
/// the paths taken.
///
/// Every other seed slows the broadcast queue down (it releases only one in eight of the times
/// it is picked), so commits pile up unreleased in front of closures and subscriptions.
fn run_gated(setup: &Setup, seed: u64) -> (Vec<String>, Stats) {
    let mut server = Server::new(setup, true);
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let slow_broadcast = seed.is_multiple_of(2);
    for _ in 0..100_000 {
        let enabled = server.enabled();
        if enabled.is_empty() {
            return (server.faults(), server.stats);
        }
        let mut action = enabled[rng.below(enabled.len())];
        if slow_broadcast && action == Action::Broadcast && enabled.len() > 1 && rng.below(8) != 0 {
            action = enabled[rng.below(enabled.len())];
        }
        server.apply(action);
    }
    (vec!["no quiescence".to_string()], server.stats)
}

/// Eight writers on two rows of four: field writes, new rows, rows leaving and entering the
/// scope of every session (filter), and revocations and grants for one session (`see`). Scope
/// changes flip in commit order, so a row leaves and enters its scope again and again.
fn plans(seed: u64) -> Vec<WriterPlan> {
    let mut rng = Rng(seed | 1);
    let mut next_new = 4;
    (0..8)
        .map(|_| {
            let row = rng.below(2) as u64;
            let kind = match rng.below(10) {
                0 => Kind::Make,
                1..=2 => Kind::Write,
                3..=6 => Kind::Toggle { session: None },
                _ => Kind::Toggle {
                    session: Some(1 + rng.below(3) as u32),
                },
            };
            let row = if kind == Kind::Make {
                next_new += 1;
                next_new - 1
            } else {
                row
            };
            WriterPlan {
                row,
                kind,
                rollback: rng.below(6) == 0,
            }
        })
        .collect()
}

fn write(row: u64) -> WriterPlan {
    WriterPlan {
        row,
        kind: Kind::Write,
        rollback: false,
    }
}

fn visible(row: u64, visible: bool) -> WriterPlan {
    WriterPlan {
        row,
        kind: Kind::Visible {
            session: None,
            visible,
        },
        rollback: false,
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn snapshot_gated_closures_keep_every_client_equal_to_its_scope() {
    // Session 2 has room for three rows, so its closures starve and resume on drain. Session 3
    // has room for one row and no live frame behind it, so live frames overload and close it.
    let mut total = Stats::default();
    for seed in 1..=2_000u64 {
        let setup = Setup {
            rows: 4,
            sessions: vec![(1, BIG), (2, 1_000), (3, ROW_BYTES + OP_BYTES / 2)],
            writers: plans(seed),
        };
        let (faults, stats) = run_gated(&setup, seed);
        assert!(faults.is_empty(), "seed {seed}: {faults:?}");
        total.evict_sent += stats.evict_sent;
        total.evict_dropped += stats.evict_dropped;
        total.unreadable_reads += stats.unreadable_reads;
        total.starved += stats.starved;
        total.overloaded += stats.overloaded;
    }
    // Every path the gate must survive was taken.
    assert!(total.evict_sent > 0, "{total:?}");
    assert!(total.evict_dropped > 0, "{total:?}");
    assert!(total.unreadable_reads > 0, "{total:?}");
    assert!(total.starved > 0, "{total:?}");
    assert!(total.overloaded > 0, "{total:?}");
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
        ..write(0)
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
        writers: vec![
            WriterPlan {
                row: 0,
                kind: Kind::Make,
                rollback: false,
            },
            write(0),
        ],
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
