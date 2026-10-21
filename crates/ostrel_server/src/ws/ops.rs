//! One `Ops` batch of a session, decided under the write lock (ARCHITECTURE 5.4, 5.5, 5.9;
//! D106, D108, D110, D116, D120, D156, D159, D182, D186).
//!
//! [`OpServer::batch`] takes the write lock of the process (the lock of
//! [`super::CallServer::write_lock`]), begins one transaction on the writer connection and
//! decides every op of the batch in batch order:
//!
//! 1. Identity: an op whose id names another replica than the session's is `Forged` and is not
//!    recorded (it is no op of this replica). A `make` whose row id names another replica is
//!    `Forged`; in an app with `auth` a session without a principal gets `Denied` (its token
//!    expired after `Hello`, 5.5 step 1).
//! 2. Order (D110): `seq = last_seq + 1` is new; `seq <= last_seq` is a replay and gets its
//!    original outcome from [`Transaction::replay_outcome`] without effect, an accepted replay
//!    always with an `Ack` of its own; `seq > last_seq + 1`, and a replay without an outcome,
//!    is `Forged` and leaves `last_seq` unchanged.
//! 3. Time: the stamp of a new op is clamped into the window from the replica's last accepted
//!    stamp (or its issue time) to `server_ms` plus the clock tolerance ([`restamp`]); a moved
//!    stamp travels back in the `Ack`.
//! 4. to 6. A batch over [`LIMITS`] `ops_per_batch` gets `Reject Limit` for every new op (D106).
//!    An op the codec could not type ([`DecodedOp::Invalid`]) is `Reject Invalid`. Then
//!    [`OpRules::check`] decides conflicts, references and values.
//! 7. Rate (D108): ops per principal and rows created per principal and model ([`OpRates`]).
//!    Over a rate the op gets `Defer`; it and every later op of the batch stay undecided.
//! 8. [`OpRules::apply`] evaluates the rules and returns the writes of the merge. The server
//!    stamps them (D186): an insert gets `made`, `changed` and, with a principal, `author`; an
//!    update gets `changed`; every stamp is the accepted stamp of the op ([`stamp`]).
//!
//! A new op that is rejected is recorded with [`Transaction::record_reject`], so its replay
//! gets the same answer; an accepted op is appended to the log after the batch, in batch order.
//!
//! # End of the batch
//!
//! While the batch still holds the write lock, [`OpServer::batch`] commits, hands the last
//! appended position to [`BatchSink::committed`] (the push to the broadcast fence) and then
//! queues every outcome with one call of [`BatchSink::outcomes`], in the order of D182: every
//! `Reject` in batch order, then the `Defer`, then the `Ack` frames, the own `Ack` of each
//! accepted replay in `seq` order and the cumulative `Ack` of the batch last ([`Outcomes`]).
//! So an `Ack` never stands before a `Reject` of an op it would cover. Only after that the lock
//! is released; the sink must queue without waiting (5.9: frames queued under the lock never
//! wait for room).
//!
//! A `commit` that fails ends the process (D159): [`BatchFault::Commit`], nothing is pushed or
//! answered and the transport closes every session and exits with code 75. Any other database
//! error rolls the batch back and answers nothing ([`BatchFault::Db`]); the transport closes the
//! session and the client resends its outbox after the reconnect.
//!
//! ASSUMPTION: the lower bound of step 3 is the stamp of the replica's op at `last_seq` when it
//! was accepted, else the issue time of the replica that the caller passes in
//! [`OpSession::floor`]. A replica whose last op was rejected can thus get a stamp below an
//! earlier accepted op of its own; the replica record of `replica_id.rs` keeps no stamp today
//! (follow up with its owner).
//!
//! ASSUMPTION: the `make` row check and the `Denied` of step 1 are recorded like every other
//! final reject of a new op, so the replica's `seq` stays contiguous and a replay gets the same
//! answer; only an op of another replica is never recorded.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use ostrel_core::ids::{Hlc, OpId, ReplicaId, RowId, ServerSeq};
use ostrel_core::value::{SafeInt, Value};
use ostrel_db::api::{Connection, DbError, FieldId, NewOp, ReplayOutcome, Transaction, Write};
use ostrel_sync::protocol::{Ack, DecodedOp, LIMITS, ModelId, Op, OpBody, RejectReason, ServerMsg};
use ostrel_sync::wire::{WireSchema, encode_op};
use ostrel_vm::host::BoxFuture;

use super::call::CommitFailed;
use super::fifo::FifoLock;
use super::hello::PrincipalKey;

/// Field id of the implicit field `made` (D186, ARCHITECTURE 6.1).
///
/// ASSUMPTION: the constants of D186 belong to `ostrel_db::api` and arrive there with T192.
/// Until then they stand here with the values of D186; the test
/// `implicit_field_ids_are_the_ones_of_d186` pins them, and T192 replaces them by the import.
pub const FIELD_MADE: FieldId = 0xFFFF_FF00;
/// Field id of the implicit field `changed` (D186).
pub const FIELD_CHANGED: FieldId = 0xFFFF_FF01;
/// Field id of the implicit field `author`, only in an app with `auth` (D186).
pub const FIELD_AUTHOR: FieldId = 0xFFFF_FF02;
/// First field id that is reserved for implicit fields (D186).
pub const FIRST_IMPLICIT_FIELD: FieldId = 0xFFFF_FF00;

/// True for a field id reserved for an implicit field (D186).
pub fn is_implicit_field(field: FieldId) -> bool {
    field >= FIRST_IMPLICIT_FIELD
}

/// Milliseconds in one hour, the period of `rows_per_hour`.
const HOUR_MS: u64 = 3_600_000;

/// Buckets kept before the full ones are dropped.
const BUCKETS_KEPT: usize = 4096;

/// The model specific steps of 5.5 for one op; the compiled program fills them, tests write
/// them by hand.
pub trait OpRules {
    /// Steps 4 to 6 for a new op with its accepted stamp, in the batch transaction: a `make`
    /// for a row that exists or existed is `Conflict`, a reference to a row the principal
    /// cannot read is `Invalid`, a value that breaks the type table, a `check` or a limit is
    /// `Invalid`. `None` lets the op pass to the rate and the rules.
    fn check<'a, 'c>(
        &'a self,
        tx: &'a mut (dyn Transaction<'c> + 'c),
        principal: Option<RowId>,
        op: &'a Op,
    ) -> BoxFuture<'a, Result<Option<RejectReason>, DbError>>;

    /// Step 8 and the merge: evaluates the rules of the op (also the unmentioned direction of
    /// D187) and returns the writes that merge it into the row, or the reject (`Denied`, or
    /// `Limit` for a row over [`LIMITS`] `row_bytes`). The writes name only the op's row and
    /// never an implicit field: [`OpServer::batch`] stamps those (D186).
    fn apply<'a, 'c>(
        &'a self,
        tx: &'a mut (dyn Transaction<'c> + 'c),
        principal: Option<RowId>,
        op: &'a Op,
    ) -> BoxFuture<'a, Result<Result<Vec<Write>, RejectReason>, DbError>>;
}

/// Where the outcome of a batch goes, called while the batch holds the write lock.
pub trait BatchSink {
    /// The batch committed and appended ops up to `last`: push `next()..=last` to the
    /// broadcast fence. Not called when no op was appended.
    fn committed(&mut self, last: ServerSeq);

    /// The outcome frames of the batch, in the order they must enter the session stream
    /// (D182). Queued at once, never waiting for room.
    fn outcomes(&mut self, frames: Vec<ServerMsg>);
}

/// What one session brings to its batches.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OpSession {
    /// The replica of the session.
    pub replica: ReplicaId,
    /// The principal, `None` without `auth` or after the token expired.
    pub principal: Option<RowId>,
    /// True in an app with `auth`.
    pub auth: bool,
    /// Who the rates count against.
    pub key: PrincipalKey,
    /// Lowest stamp the replica may get: its issue time (5.5 step 3).
    pub floor: Hlc,
}

impl fmt::Debug for OpSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpSession")
            .field("replica", &self.replica.to_hex())
            .field("principal", &self.principal.map(RowId::to_hex))
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

/// Why a batch ended without its outcomes.
#[derive(Clone, Debug, PartialEq)]
pub enum BatchFault {
    /// The commit failed: close every session and exit with code 75 (D159).
    Commit(CommitFailed),
    /// A database error before the commit; the batch rolled back and nothing is answered.
    Db(DbError),
    /// The rules returned a write the server must not apply (another row, an implicit field),
    /// or the op could not be encoded for the log: a server defect. Rolled back, nothing
    /// answered.
    Defect(&'static str),
}

impl fmt::Display for BatchFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BatchFault::Commit(failed) => failed.fmt(f),
            BatchFault::Db(error) => write!(f, "op batch rolled back: {error:?}"),
            BatchFault::Defect(why) => write!(f, "op batch rolled back, server defect: {why}"),
        }
    }
}

impl std::error::Error for BatchFault {}

/// What a batch did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchEnd {
    /// The last appended position, `None` when no op was accepted.
    pub committed: Option<ServerSeq>,
    /// The number of outcome frames handed to the sink.
    pub frames: usize,
}

/// The outcomes of one batch, kept apart by kind so that they leave in the order of D182.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcomes {
    rejects: Vec<ServerMsg>,
    defer: Option<ServerMsg>,
    replays: Vec<Ack>,
    cumulative: Option<Ack>,
}

impl Outcomes {
    /// Every `Reject` in batch order, then the `Defer`, then the own `Ack` of each accepted
    /// replay in `seq` order, the cumulative `Ack` last (D182).
    pub fn into_frames(self) -> Vec<ServerMsg> {
        let mut replays = self.replays;
        replays.sort_by_key(|ack| ack.op.seq);
        let mut frames = self.rejects;
        frames.extend(self.defer);
        frames.extend(replays.into_iter().map(ServerMsg::Ack));
        frames.extend(self.cumulative.map(ServerMsg::Ack));
        frames
    }

    fn reject(&mut self, op: OpId, reason: RejectReason) {
        self.rejects.push(ServerMsg::Reject { op, reason });
    }
}

/// The op and row rates of a server process (ARCHITECTURE 5.9, D108), per principal over all
/// its sessions, in an app without `auth` per IP address.
#[derive(Debug, Default)]
pub struct OpRates {
    ops: HashMap<PrincipalKey, Bucket>,
    rows: HashMap<(PrincipalKey, ModelId), Bucket>,
}

/// A token bucket. One token is `period_ms` units; it refills `per_period` units per ms.
#[derive(Clone, Copy, Debug)]
struct Bucket {
    units: u64,
    at_ms: u64,
}

#[derive(Clone, Copy, Debug)]
struct Rate {
    per_period: u64,
    period_ms: u64,
    burst: u64,
}

impl Rate {
    fn cap(self) -> u64 {
        self.burst.saturating_mul(self.period_ms)
    }

    /// Refills `bucket` up to `now_ms`; a clock that went back refills nothing.
    fn refill(self, bucket: &mut Bucket, now_ms: u64) {
        let elapsed = now_ms.saturating_sub(bucket.at_ms);
        let added = elapsed.saturating_mul(self.per_period);
        bucket.units = bucket.units.saturating_add(added).min(self.cap());
        bucket.at_ms = bucket.at_ms.max(now_ms);
    }

    /// Milliseconds until the bucket holds one token, at least 1.
    fn wait_ms(self, bucket: &Bucket) -> u32 {
        let missing = self.period_ms.saturating_sub(bucket.units);
        let ms = missing.div_ceil(self.per_period.max(1)).max(1);
        u32::try_from(ms).unwrap_or(u32::MAX)
    }
}

const OPS_RATE: Rate = Rate {
    per_period: LIMITS.ops_per_second as u64,
    period_ms: 1000,
    burst: LIMITS.ops_burst as u64,
};

const ROWS_RATE: Rate = Rate {
    per_period: LIMITS.rows_per_hour as u64,
    period_ms: HOUR_MS,
    burst: LIMITS.rows_per_hour as u64,
};

impl OpRates {
    /// Takes one op token of `key` and, for a `make`, one row token of (`key`, `model`) at
    /// `now_ms`. Over a rate nothing is taken and the answer is the wait in ms (`Defer`).
    pub fn take(
        &mut self,
        key: PrincipalKey,
        make: Option<ModelId>,
        now_ms: u64,
    ) -> Result<(), u32> {
        self.prune(now_ms);
        let ops = self.ops.entry(key).or_insert(Bucket {
            units: OPS_RATE.cap(),
            at_ms: now_ms,
        });
        OPS_RATE.refill(ops, now_ms);
        let ops_wait = (ops.units < OPS_RATE.period_ms).then(|| OPS_RATE.wait_ms(ops));
        let rows = match make {
            Some(model) => {
                let rows = self.rows.entry((key, model)).or_insert(Bucket {
                    units: ROWS_RATE.cap(),
                    at_ms: now_ms,
                });
                ROWS_RATE.refill(rows, now_ms);
                Some(rows)
            }
            None => None,
        };
        let rows_wait = rows
            .as_ref()
            .and_then(|b| (b.units < ROWS_RATE.period_ms).then(|| ROWS_RATE.wait_ms(b)));
        if let Some(wait) = ops_wait.max(rows_wait) {
            return Err(wait);
        }
        if let Some(rows) = rows {
            rows.units -= ROWS_RATE.period_ms;
        }
        if let Some(ops) = self.ops.get_mut(&key) {
            ops.units -= OPS_RATE.period_ms;
        }
        Ok(())
    }

    /// Drops full buckets once many are kept: a full bucket is the same as none.
    fn prune(&mut self, now_ms: u64) {
        if self.ops.len() >= BUCKETS_KEPT {
            self.ops.retain(|_, b| {
                OPS_RATE.refill(b, now_ms);
                b.units < OPS_RATE.cap()
            });
        }
        if self.rows.len() >= BUCKETS_KEPT {
            self.rows.retain(|_, b| {
                ROWS_RATE.refill(b, now_ms);
                b.units < ROWS_RATE.cap()
            });
        }
    }
}

/// The op path of one server process.
pub struct OpServer {
    rules: Box<dyn OpRules>,
    rates: RefCell<OpRates>,
}

impl fmt::Debug for OpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpServer").finish_non_exhaustive()
    }
}

/// The state of a batch while it runs.
struct Run {
    /// Highest `seq` of the replica with a final outcome, this batch included.
    last_seq: u32,
    /// Lower bound of the next stamp; exclusive once it is the stamp of an accepted op.
    floor: Hlc,
    floor_taken: bool,
    /// Position of the last appended op.
    last_position: Option<ServerSeq>,
    /// The last accepted op and the stamps moved in step 3.
    last_accepted: Option<OpId>,
    restamped: Vec<(OpId, Hlc)>,
    /// True once the transaction wrote anything (an op or a final reject).
    wrote: bool,
    out: Outcomes,
}

/// The decision about a new op.
enum Verdict {
    Accept(Op, Vec<Write>),
    Reject(RejectReason),
    Defer(u32),
}

impl OpServer {
    /// The op path with the model specific steps of the compiled program.
    pub fn new(rules: Box<dyn OpRules>) -> Self {
        OpServer {
            rules,
            rates: RefCell::new(OpRates::default()),
        }
    }

    /// Decides one batch of `session` at server time `server_ms` (module documentation).
    ///
    /// The sink is called only while the batch holds the write lock, and only after a
    /// successful commit (or the rollback of a batch that wrote nothing).
    pub async fn batch(
        &self,
        writer: &FifoLock<Box<dyn Connection>>,
        schema: &dyn WireSchema,
        session: &OpSession,
        ops: &[DecodedOp],
        server_ms: u64,
        sink: &mut dyn BatchSink,
    ) -> Result<BatchEnd, BatchFault> {
        let mut conn = writer.lock().await;
        let mut tx = conn.begin().await.map_err(BatchFault::Db)?;
        let run = match self
            .decide(tx.as_mut(), schema, session, ops, server_ms)
            .await
        {
            Ok(run) => run,
            Err(fault) => {
                // A failed rollback leaves nothing visible: the transaction never commits.
                let _ = tx.rollback().await;
                return Err(fault);
            }
        };
        if run.wrote {
            tx.commit()
                .await
                .map_err(|e| BatchFault::Commit(CommitFailed(e)))?;
        } else {
            tx.rollback().await.map_err(BatchFault::Db)?;
        }
        if let Some(last) = run.last_position {
            sink.committed(last);
        }
        let frames = run.out.into_frames();
        let count = frames.len();
        if count > 0 {
            sink.outcomes(frames);
        }
        drop(conn);
        Ok(BatchEnd {
            committed: run.last_position,
            frames: count,
        })
    }

    async fn decide<'c>(
        &self,
        tx: &mut (dyn Transaction<'c> + 'c),
        schema: &dyn WireSchema,
        session: &OpSession,
        ops: &[DecodedOp],
        server_ms: u64,
    ) -> Result<Run, BatchFault> {
        let replica = session.replica;
        let last_seq = tx.last_seq(replica).await.map_err(BatchFault::Db)?;
        let mut run = Run {
            last_seq,
            floor: session.floor,
            floor_taken: false,
            last_position: None,
            last_accepted: None,
            restamped: Vec::new(),
            wrote: false,
            out: Outcomes::default(),
        };
        if last_seq > 0 {
            let last = OpId {
                replica,
                seq: last_seq,
            };
            let outcome = tx.replay_outcome(last).await.map_err(BatchFault::Db)?;
            if let Some(ReplayOutcome::Accepted(stored)) = outcome
                && stored.op.hlc >= run.floor
            {
                run.floor = stored.op.hlc;
                run.floor_taken = true;
            }
        }
        let over = ops.len() > LIMITS.ops_per_batch as usize;
        for decoded in ops {
            let id = head(decoded);
            if id.replica != replica {
                run.out.reject(id, RejectReason::Forged);
                continue;
            }
            if id.seq <= run.last_seq {
                replay(tx, &mut run, id).await?;
                continue;
            }
            if Some(id.seq) != run.last_seq.checked_add(1) {
                run.out.reject(id, RejectReason::Forged);
                continue;
            }
            match self
                .judge(tx, session, decoded, over, server_ms, &run)
                .await?
            {
                Verdict::Defer(retry_after_ms) => {
                    run.out.defer = Some(ServerMsg::Defer {
                        op: id,
                        retry_after_ms,
                    });
                    break;
                }
                Verdict::Reject(reason) => {
                    tx.record_reject(id, reason.name())
                        .await
                        .map_err(BatchFault::Db)?;
                    run.wrote = true;
                    run.last_seq = id.seq;
                    run.out.reject(id, reason);
                }
                Verdict::Accept(op, writes) => {
                    let writes = stamp(writes, &op, session.principal)?;
                    tx.apply(&writes).await.map_err(BatchFault::Db)?;
                    let body = encode_op(&op, schema)
                        .map_err(|_| BatchFault::Defect("accepted op not in the wire schema"))?;
                    let logged = NewOp {
                        id: op.id,
                        hlc: op.hlc,
                        model: op.model,
                        row: op.row,
                        body: body.into_bytes(),
                    };
                    let position = tx
                        .append_ops(std::slice::from_ref(&logged))
                        .await
                        .map_err(BatchFault::Db)?;
                    if let DecodedOp::Valid(sent) = decoded
                        && sent.hlc != op.hlc
                    {
                        run.restamped.push((op.id, op.hlc));
                    }
                    run.wrote = true;
                    run.last_seq = id.seq;
                    run.floor = op.hlc;
                    run.floor_taken = true;
                    run.last_position = Some(position);
                    run.last_accepted = Some(op.id);
                }
            }
        }
        if let (Some(op), Some(seq)) = (run.last_accepted, run.last_position) {
            run.out.cumulative = Some(Ack {
                op,
                seq,
                restamped: std::mem::take(&mut run.restamped),
            });
        }
        Ok(run)
    }

    /// Steps 1 and 3 to 8 for a new op (`seq = last_seq + 1`).
    async fn judge<'c>(
        &self,
        tx: &mut (dyn Transaction<'c> + 'c),
        session: &OpSession,
        decoded: &DecodedOp,
        over: bool,
        server_ms: u64,
        run: &Run,
    ) -> Result<Verdict, BatchFault> {
        if session.auth && session.principal.is_none() {
            return Ok(Verdict::Reject(RejectReason::Denied));
        }
        let op = match decoded {
            DecodedOp::Valid(op) => op,
            DecodedOp::Invalid { .. } => return Ok(Verdict::Reject(RejectReason::Invalid)),
        };
        let make = matches!(op.body, OpBody::Make { .. });
        if make && op.row.replica() != session.replica {
            return Ok(Verdict::Reject(RejectReason::Forged));
        }
        if over {
            return Ok(Verdict::Reject(RejectReason::Limit));
        }
        let hlc = restamp(
            op.hlc,
            session.replica,
            run.floor,
            run.floor_taken,
            server_ms.saturating_add(u64::from(LIMITS.clock_tolerance_ms)),
        )
        .ok_or(BatchFault::Defect("no stamp left in the clock window"))?;
        let op = Op { hlc, ..op.clone() };
        let principal = session.principal;
        if let Some(reason) = self
            .rules
            .check(tx, principal, &op)
            .await
            .map_err(BatchFault::Db)?
        {
            return Ok(Verdict::Reject(reason));
        }
        let created = make.then_some(op.model);
        if let Err(wait) = self
            .rates
            .borrow_mut()
            .take(session.key, created, server_ms)
        {
            return Ok(Verdict::Defer(wait));
        }
        match self
            .rules
            .apply(tx, principal, &op)
            .await
            .map_err(BatchFault::Db)?
        {
            Ok(writes) => Ok(Verdict::Accept(op, writes)),
            Err(reason) => Ok(Verdict::Reject(reason)),
        }
    }
}

/// The id of an op, also of one the codec could not type.
fn head(op: &DecodedOp) -> OpId {
    match op {
        DecodedOp::Valid(op) => op.id,
        DecodedOp::Invalid { id, .. } => *id,
    }
}

/// The original outcome of a replayed op (D110, D120): an accepted op gets an `Ack` of its own
/// with its logged position and accepted stamp; a rejected one the same reason; an op without
/// an outcome is `Forged`.
async fn replay<'c>(
    tx: &mut (dyn Transaction<'c> + 'c),
    run: &mut Run,
    id: OpId,
) -> Result<(), BatchFault> {
    match tx.replay_outcome(id).await.map_err(BatchFault::Db)? {
        Some(ReplayOutcome::Accepted(stored)) => run.out.replays.push(Ack {
            op: id,
            seq: stored.seq,
            restamped: vec![(id, stored.op.hlc)],
        }),
        Some(ReplayOutcome::Rejected(reason)) => {
            let reason = RejectReason::ALL
                .into_iter()
                .find(|r| r.name() == reason)
                .ok_or(BatchFault::Defect(
                    "recorded reject reason is not a wire reason",
                ))?;
            run.out.reject(id, reason);
        }
        None => run.out.reject(id, RejectReason::Forged),
    }
    Ok(())
}

/// The accepted stamp of a new op (5.5 step 3): `hlc` clamped into the window from `floor`
/// (inclusive, the issue time; exclusive when `taken`, the stamp of the replica's last accepted
/// op) to `upper_ms`. A stamp above the window moves to its upper end, one below it just above
/// the floor through the counter, so the replica's stamps keep increasing. The stamp always
/// carries `replica`. `None` only when the clock range of [`Hlc`] is exhausted.
pub fn restamp(
    hlc: Hlc,
    replica: ReplicaId,
    floor: Hlc,
    taken: bool,
    upper_ms: u64,
) -> Option<Hlc> {
    let mut stamp = Hlc::new(hlc.wall_ms(), hlc.counter(), replica).ok()?;
    if stamp.wall_ms() > upper_ms {
        stamp = Hlc::new(upper_ms, 0, replica).ok()?;
    }
    let floor = Hlc::new(floor.wall_ms(), floor.counter(), replica).ok()?;
    if stamp < floor || (taken && stamp <= floor) {
        stamp = if taken { next_after(floor)? } else { floor };
    }
    Some(stamp)
}

fn next_after(hlc: Hlc) -> Option<Hlc> {
    match hlc.counter().checked_add(1) {
        Some(counter) => Hlc::new(hlc.wall_ms(), counter, hlc.replica()).ok(),
        None => Hlc::new(hlc.wall_ms().checked_add(1)?, 0, hlc.replica()).ok(),
    }
}

/// Adds the implicit fields of D186 to the writes of an accepted op: an insert gets `made`,
/// `changed` and, with a principal, `author`; an update gets `changed`; all from the accepted
/// stamp. Writes for another row, or that name an implicit field themselves, are a defect of
/// the rules: no op ever writes a stamp or the author (D116).
pub fn stamp(
    writes: Vec<Write>,
    op: &Op,
    principal: Option<RowId>,
) -> Result<Vec<Write>, BatchFault> {
    let wall = i64::try_from(op.hlc.wall_ms())
        .ok()
        .and_then(|ms| SafeInt::new(ms).ok());
    let time = Value::Time(wall.ok_or(BatchFault::Defect("stamp outside the Time range"))?);
    let mut out = Vec::with_capacity(writes.len());
    for mut write in writes {
        let (model, row) = match &write {
            Write::Insert { model, row, .. }
            | Write::Update { model, row, .. }
            | Write::Delete { model, row, .. } => (*model, *row),
        };
        if model != op.model || row != op.row {
            return Err(BatchFault::Defect("rules wrote another row than the op's"));
        }
        match &mut write {
            Write::Insert {
                fields,
                collections,
                ..
            } => {
                refuse_implicit(fields, collections)?;
                fields.push((FIELD_MADE, time.clone()));
                fields.push((FIELD_CHANGED, time.clone()));
                if let Some(user) = principal {
                    fields.push((FIELD_AUTHOR, Value::Ref(user)));
                }
            }
            Write::Update {
                fields,
                collections,
                ..
            } => {
                refuse_implicit(fields, collections)?;
                fields.push((FIELD_CHANGED, time.clone()));
            }
            Write::Delete { .. } => {}
        }
        out.push(write);
    }
    Ok(out)
}

fn refuse_implicit<C>(
    fields: &[(FieldId, Value)],
    collections: &[(FieldId, C)],
) -> Result<(), BatchFault> {
    let named = fields.iter().map(|(f, _)| *f);
    if named
        .chain(collections.iter().map(|(f, _)| *f))
        .any(is_implicit_field)
    {
        return Err(BatchFault::Defect("rules wrote an implicit field"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replica(n: u64) -> ReplicaId {
        ReplicaId(n)
    }

    fn hlc(ms: u64, counter: u16, r: u64) -> Hlc {
        Hlc::new(ms, counter, replica(r)).unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn implicit_field_ids_are_the_ones_of_d186() {
        assert_eq!(FIELD_MADE, 0xFFFF_FF00);
        assert_eq!(FIELD_CHANGED, 0xFFFF_FF01);
        assert_eq!(FIELD_AUTHOR, 0xFFFF_FF02);
        assert_eq!(FIRST_IMPLICIT_FIELD, FIELD_MADE);
        assert!(is_implicit_field(FIELD_AUTHOR));
        assert!(!is_implicit_field(FIRST_IMPLICIT_FIELD - 1));
    }

    #[test]
    fn a_stamp_inside_the_window_stays() {
        let got = restamp(hlc(5_000, 3, 7), replica(7), hlc(1_000, 0, 7), false, 9_000);
        assert_eq!(got, Some(hlc(5_000, 3, 7)));
    }

    #[test]
    fn a_future_stamp_moves_to_the_upper_end() {
        let got = restamp(
            hlc(50_000, 9, 7),
            replica(7),
            hlc(1_000, 0, 7),
            false,
            9_000,
        );
        assert_eq!(got, Some(hlc(9_000, 0, 7)));
    }

    #[test]
    fn a_stamp_at_or_below_the_last_accepted_one_moves_just_above_it() {
        let floor = hlc(4_000, 2, 7);
        assert_eq!(
            restamp(hlc(4_000, 2, 7), replica(7), floor, true, 9_000),
            Some(hlc(4_000, 3, 7))
        );
        assert_eq!(
            restamp(hlc(10, 0, 7), replica(7), floor, true, 9_000),
            Some(hlc(4_000, 3, 7))
        );
        // Below the issue time (inclusive bound) the stamp becomes the issue time.
        assert_eq!(
            restamp(hlc(10, 0, 7), replica(7), hlc(1_000, 0, 7), false, 9_000),
            Some(hlc(1_000, 0, 7))
        );
    }

    #[test]
    fn the_floor_wins_over_the_upper_end_and_the_counter_carries() {
        let floor = hlc(9_000, u16::MAX, 7);
        assert_eq!(
            restamp(hlc(20_000, 0, 7), replica(7), floor, true, 9_000),
            Some(hlc(9_001, 0, 7))
        );
    }

    #[test]
    fn a_stamp_always_carries_the_replica_of_the_op() {
        let got = restamp(
            hlc(5_000, 0, 99),
            replica(7),
            hlc(1_000, 0, 7),
            false,
            9_000,
        );
        assert_eq!(got, Some(hlc(5_000, 0, 7)));
    }

    #[test]
    fn outcomes_leave_as_rejects_then_defer_then_acks_with_the_cumulative_last() {
        let id = |seq| OpId {
            replica: replica(7),
            seq,
        };
        let ack = |seq: u32| Ack {
            op: id(seq),
            seq: ServerSeq(u64::from(seq)),
            restamped: Vec::new(),
        };
        let mut out = Outcomes {
            cumulative: Some(ack(9)),
            ..Outcomes::default()
        };
        out.replays.push(ack(3));
        out.replays.push(ack(2));
        out.reject(id(5), RejectReason::Denied);
        out.defer = Some(ServerMsg::Defer {
            op: id(8),
            retry_after_ms: 20,
        });
        out.reject(id(6), RejectReason::Invalid);
        assert_eq!(
            out.into_frames(),
            vec![
                ServerMsg::Reject {
                    op: id(5),
                    reason: RejectReason::Denied
                },
                ServerMsg::Reject {
                    op: id(6),
                    reason: RejectReason::Invalid
                },
                ServerMsg::Defer {
                    op: id(8),
                    retry_after_ms: 20
                },
                ServerMsg::Ack(ack(2)),
                ServerMsg::Ack(ack(3)),
                ServerMsg::Ack(ack(9)),
            ]
        );
    }

    #[test]
    fn the_op_rate_defers_after_the_burst_and_names_the_wait() {
        let mut rates = OpRates::default();
        let key = PrincipalKey::Ip(std::net::IpAddr::from([127, 0, 0, 1]));
        for _ in 0..LIMITS.ops_burst {
            assert_eq!(rates.take(key, None, 1_000), Ok(()));
        }
        // 50 per s: one token every 20 ms.
        assert_eq!(rates.take(key, None, 1_000), Err(20));
        assert_eq!(rates.take(key, None, 1_019), Err(1));
        assert_eq!(rates.take(key, None, 1_020), Ok(()));
    }

    #[test]
    fn the_row_rate_counts_per_model_and_takes_nothing_when_it_defers() {
        let mut rates = OpRates::default();
        let key = PrincipalKey::Ip(std::net::IpAddr::from([127, 0, 0, 1]));
        // Spread the makes so the op rate never binds: 25 ms apart refills more than one op.
        // The row bucket refills about 7 rows over these 25 s.
        let mut now = 0;
        let mut taken = 0;
        while rates.take(key, Some(1), now).is_ok() && taken < 2 * LIMITS.rows_per_hour {
            taken += 1;
            now += 25;
        }
        assert!(
            (LIMITS.rows_per_hour..LIMITS.rows_per_hour + 10).contains(&taken),
            "{taken}"
        );
        let wait = rates.take(key, Some(1), now);
        // 1 000 rows per hour: one row every 3.6 s.
        assert!(
            matches!(wait, Err(ms) if (1..=3_600).contains(&ms)),
            "{wait:?}"
        );
        // Another model of the same principal is not limited, and the deferred make took no
        // op token: the op bucket is unchanged by the refusal.
        assert_eq!(rates.take(key, Some(2), now), Ok(()));
        assert_eq!(rates.take(key, None, now), Ok(()));
    }
}
