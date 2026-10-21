//! AC-44, snapshot consistency (ARCHITECTURE 5.4 "Snapshot consistency", D151, D154, D156 to
//! D162).
//!
//! A simulated server process: writers commit ops on rows under the write lock, the broadcast
//! queue releases commits through the [`BroadcastFence`], closures serve waiting rows as
//! `Snapshot` pages, and each session's frames pass its send queue to a simulated client. The
//! client applies a page by replacing the row and an op by appending its tag, and drops an op at
//! or below its `applied_server_seq` (D62). At the end every client row must equal the committed
//! row: an op missing there was lost, an op present twice was applied twice.
//!
//! The test functions carry no `ac_44_` prefix yet: AC-44 is still on the pending list of
//! `ci/checks/70_ac_coverage.sh`, and the AC also needs the revocation and client cases of
//! its other owners. The prefix is added when QA takes AC-44 off that list.
//!
//! The gated runs schedule every step at random over many seeds. The runs without the gate
//! read a snapshot while a writer is in flight and show the two faults the quiet point
//! prevents.

use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;

use ostrel_sync::broadcast_fence::{BroadcastFence, Commit, CommittedScope, ScopeExit};
use ostrel_sync::log_gate::{
    ClosureReader, QueueError, ReadRow, Release, SendQueue, Ticket, WaitingRows, WriteLock,
};
use ostrel_sync::protocol::{
    Hlc, LoggedOp, Op, OpBody, OpId, ReplicaId, RowId, ServerMsg, ServerSeq, SubId,
};

type Session = u32;
const SUB: SubId = 1;
/// Encoded size of one row in a `Snapshot` page and of one op in an `Ops` frame.
const ROW_BYTES: u64 = 300;
const OP_BYTES: u64 = 10;

fn row_id(n: u64) -> RowId {
    RowId::new(Hlc::new(1_000 + n, 0, ReplicaId(9)).unwrap())
}

/// Row contents: the tags of the ops applied to it, in order.
type Content = Vec<u64>;

// ---------------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Client {
    rows: BTreeMap<u64, Content>,
    applied: u64,
    stream: u64,
    last_page: bool,
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

#[derive(Clone, Copy, Debug)]
struct WriterPlan {
    row: u64,
    /// Creates the row instead of writing an existing one.
    make: bool,
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

struct Server {
    gated: bool,
    next_pos: u64,
    push_from: u64,
    committed: BTreeMap<u64, Content>,
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
}

struct Scope<'a> {
    waiting: &'a WaitingRows<Session>,
    sessions: Vec<Session>,
}

impl CommittedScope for Scope<'_> {
    type Session = Session;

    fn scope_exits(&mut self, _commit: &Commit) -> Vec<ScopeExit<Session>> {
        Vec::new()
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
    committed: &'a BTreeMap<u64, Content>,
    ids: &'a BTreeMap<RowId, u64>,
}

impl ClosureReader<Session> for Reader<'_> {
    type Row = (u64, Content);
    type Error = Infallible;

    fn read(
        &mut self,
        _session: &Session,
        _sub: SubId,
        row: RowId,
    ) -> Result<Option<ReadRow<Self::Row>>, Infallible> {
        let n = self.ids[&row];
        Ok(self.committed.get(&n).map(|content| ReadRow {
            row: (n, content.clone()),
            encoded_bytes: ROW_BYTES,
            read_bytes: ROW_BYTES,
            visible: true,
        }))
    }
}

/// Positions and rows of one simulated run.
struct Setup {
    rows: u64,
    sessions: Vec<(Session, u64)>,
    writers: Vec<WriterPlan>,
}

impl Server {
    fn new(setup: &Setup, gated: bool) -> Self {
        let mut ids = BTreeMap::new();
        let mut committed = BTreeMap::new();
        for n in 0..setup.rows {
            committed.insert(n, Vec::new());
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
            next_pos: 1,
            push_from: 1,
            committed,
            ids,
            lock: WriteLock::with_bound(2),
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
        }
    }

    fn want_closure(&mut self) {
        if self.waiting.request_closure() {
            self.lock.request(Task::Closure);
        }
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
        } else if plan.make {
            self.committed.insert(plan.row, vec![pos]);
        } else {
            self.committed.get_mut(&plan.row).unwrap().push(pos);
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
                    body: if plan.make {
                        OpBody::Make { fields: vec![] }
                    } else {
                        OpBody::Drop
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
                self.waiting.subscribe(s, SUB).unwrap();
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
        let rooms: BTreeMap<Session, u64> =
            self.queues.iter().map(|(s, q)| (*s, q.room())).collect();
        let mut reader = Reader {
            committed: &self.committed,
            ids: &self.ids,
        };
        let out = self
            .waiting
            .run_closure(ServerSeq(mark), &rooms, &mut reader)
            .unwrap();
        for s in &out.starved {
            self.queues.get_mut(s).unwrap().mark_starved();
        }
        let starved = out.starved;
        let mut frames = Vec::new();
        for page in out.pages {
            self.queues
                .get_mut(&page.session)
                .unwrap()
                .push_closure(page.bytes)
                .unwrap();
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

    fn read_candidates(&mut self, s: Session) {
        self.waiting.begin_candidates(&s, SUB).unwrap();
        let rows: Vec<RowId> = self.committed.keys().map(|n| row_id(*n)).collect();
        self.waiting.add_candidates(&s, SUB, rows, true).unwrap();
        self.subscribed.insert(s, Stage::Done);
        self.want_closure();
    }

    // ---- broadcast queue ----

    fn broadcast(&mut self) {
        match self.pending.pop_front().expect("broadcast enabled") {
            Item::Pages(frames) => {
                for (s, frame) in frames {
                    self.frames.get_mut(&s).unwrap().push_back(frame);
                }
            }
            Item::Commit(commit) => {
                // Scope re-evaluation: a created row enters the scope of every subscription.
                let mut entered = false;
                for op in &commit.ops {
                    if matches!(op.op.body, OpBody::Make { .. }) {
                        for s in self.queues.keys() {
                            entered |= self.waiting.enter(s, SUB, op.op.row).is_ok();
                        }
                    }
                }
                let mut scope = Scope {
                    waiting: &self.waiting,
                    sessions: self.queues.keys().copied().collect(),
                };
                let out = self.fence.push(commit, &mut scope).unwrap();
                for delivery in out {
                    let ServerMsg::Ops(ops) = delivery.msg else {
                        panic!("no scope exits in this simulation")
                    };
                    let bytes = OP_BYTES * ops.len() as u64;
                    let frame = Frame::Ops(
                        ops.iter()
                            .map(|o| (o.seq.0, self.ids[&o.op.row], o.op.id.seq.into()))
                            .collect(),
                    );
                    match self
                        .queues
                        .get_mut(&delivery.session)
                        .unwrap()
                        .push_now(bytes)
                    {
                        Ok(()) => self
                            .frames
                            .get_mut(&delivery.session)
                            .unwrap()
                            .push_back(frame),
                        Err(QueueError::Overloaded | QueueError::Closed) => {
                            panic!("no session overflows in this simulation")
                        }
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
        let bytes = match &frame {
            Frame::Page { bytes, .. } => *bytes,
            Frame::Ops(ops) => OP_BYTES * ops.len() as u64,
        };
        self.clients.get_mut(&s).unwrap().receive(&frame);
        if self.queues.get_mut(&s).unwrap().written(bytes) {
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
                Stage::Idle => out.push(Action::Subscribe(*s)),
                Stage::Holding if self.waiting.wants_candidates(s, SUB) => {
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

    fn apply(&mut self, action: Action) {
        match action {
            Action::Grant => {
                self.grant();
            }
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
                self.lock.request(Task::Subscribe(s));
                self.subscribed.insert(s, Stage::Queued);
            }
            Action::Candidates(s) => self.read_candidates(s),
            Action::Broadcast => self.broadcast(),
            Action::Drain(s) => self.drain(s),
        }
    }

    /// Faults of every client against the committed rows.
    fn faults(&self) -> Vec<String> {
        let mut faults = Vec::new();
        for (s, client) in &self.clients {
            faults.extend(client.faults.iter().map(|f| format!("session {s}: {f}")));
            if !client.last_page {
                faults.push(format!("session {s}: no last page"));
            }
            for (n, content) in &self.committed {
                let got = client.rows.get(n).cloned().unwrap_or_default();
                for tag in content {
                    match got.iter().filter(|t| *t == tag).count() {
                        0 => faults.push(format!("session {s}: row {n} lost op {tag}")),
                        1 => {}
                        k => {
                            faults.push(format!("session {s}: row {n} applied op {tag} {k} times"))
                        }
                    }
                }
                if !client.rows.contains_key(n) {
                    faults.push(format!("session {s}: row {n} never served"));
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

/// Runs `setup` gated with random scheduling until nothing is enabled. Returns the faults.
fn run_gated(setup: &Setup, seed: u64) -> Vec<String> {
    let mut server = Server::new(setup, true);
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    for _ in 0..100_000 {
        let enabled = server.enabled();
        if enabled.is_empty() {
            return server.faults();
        }
        let action = enabled[rng.below(enabled.len())];
        server.apply(action);
    }
    vec!["no quiescence".to_string()]
}

fn plans(seed: u64) -> Vec<WriterPlan> {
    let mut rng = Rng(seed | 1);
    let mut next_new = 4;
    (0..6)
        .map(|_| {
            let make = rng.below(4) == 0;
            let row = if make {
                next_new += 1;
                next_new - 1
            } else {
                rng.below(4) as u64
            };
            WriterPlan {
                row,
                make,
                rollback: rng.below(5) == 0,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn snapshot_gated_closures_lose_and_repeat_no_op() {
    // Session 2 has room for three rows only, so its closures starve and resume on drain.
    for seed in 1..=1_500u64 {
        let setup = Setup {
            rows: 4,
            sessions: vec![(1, 8 * 1024 * 1024), (2, 1_000)],
            writers: plans(seed),
        };
        let faults = run_gated(&setup, seed);
        assert!(faults.is_empty(), "seed {seed}: {faults:?}");
    }
}

#[test]
fn snapshot_subscribe_while_a_lower_position_is_in_flight() {
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, 1 << 20)],
        writers: vec![WriterPlan {
            row: 0,
            make: false,
            rollback: false,
        }],
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
    server.apply(Action::Grant);
    server.apply(Action::Candidates(1));
    server.apply(Action::Grant);
    while let Some(&action) = server.enabled().first() {
        server.apply(action);
    }
    assert_eq!(server.faults(), Vec::<String>::new());
    assert_eq!(server.clients[&1].rows[&0], vec![1]);
}

#[test]
fn snapshot_rolled_back_position_is_covered_by_the_next_commit() {
    let plan = |rollback| WriterPlan {
        row: 0,
        make: false,
        rollback,
    };
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, 1 << 20)],
        writers: vec![plan(true), plan(false), plan(false)],
    };
    for seed in 1..=300u64 {
        let faults = run_gated(&setup, seed);
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
                make: true,
                rollback: false,
            },
            WriterPlan {
                row: 0,
                make: false,
                rollback: false,
            },
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
    let plan = WriterPlan {
        row: 0,
        make: false,
        rollback: false,
    };
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, 1 << 20)],
        writers: vec![plan, plan],
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
    assert_eq!(
        server.faults(),
        vec!["session 1: row 0 lost op 1".to_string()]
    );
}

#[test]
fn snapshot_without_gate_a_mark_at_the_last_release_repeats_an_op() {
    // Position 1 is committed and visible in the row but not yet released; a snapshot read
    // with the last released position as its mark contains it, and the op arrives again live.
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, 1 << 20)],
        writers: vec![WriterPlan {
            row: 0,
            make: false,
            rollback: false,
        }],
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
    assert_eq!(
        server.faults(),
        vec!["session 1: row 0 applied op 1 2 times".to_string()]
    );
}

#[test]
fn snapshot_the_gated_closure_waits_for_the_writer_in_flight() {
    // The same interleaving as the lost op case, under the lock: the closure is queued behind
    // the writer, so its mark is a quiet point.
    let plan = WriterPlan {
        row: 0,
        make: false,
        rollback: false,
    };
    let setup = Setup {
        rows: 1,
        sessions: vec![(1, 1 << 20)],
        writers: vec![plan, plan],
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
