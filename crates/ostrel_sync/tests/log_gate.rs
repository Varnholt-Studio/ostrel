//! Write lock, send queue and closure rules of `ostrel_sync::log_gate` (ARCHITECTURE 5.4, 5.6,
//! 5.9; D156 to D162).

use std::collections::BTreeMap;
use std::convert::Infallible;

use ostrel_sync::log_gate::{
    BROADCAST_COMMITS, CANDIDATE_PAGE, CLOSURE_BYTES, CLOSURE_ROWS, ClosureReader, EvictAction,
    LockError, QueueError, ReadRow, Release, Room, SEND_QUEUE_BYTES, SendQueue, WaitError,
    WaitingRows, WriteLock,
};
use ostrel_sync::protocol::{Hlc, ReplicaId, RowId, ServerSeq, SubId};

fn row(n: u64) -> RowId {
    RowId::new(Hlc::new(1_000 + n, 0, ReplicaId(9)).unwrap())
}

const MIB: u64 = 1024 * 1024;

// ---------------------------------------------------------------------------------------------
// Write lock
// ---------------------------------------------------------------------------------------------

#[test]
fn the_lock_is_granted_in_request_order_and_to_one_holder() {
    let mut lock = WriteLock::new();
    let a = lock.request("a");
    let b = lock.request("b");
    let c = lock.request("c");
    assert_eq!(lock.grant(), Some((a, "a")));
    assert_eq!(lock.grant(), None, "a second holder while a holds the lock");
    lock.release(a, Release::Nothing).unwrap();
    assert_eq!(lock.grant(), Some((b, "b")));
    lock.release(b, Release::Nothing).unwrap();
    assert_eq!(lock.grant(), Some((c, "c")));
    assert_eq!(lock.queued(), 0);
}

#[test]
fn only_the_holder_releases_the_lock() {
    let mut lock = WriteLock::new();
    let a = lock.request(1);
    let b = lock.request(2);
    lock.grant().unwrap();
    assert_eq!(
        lock.release(b, Release::Committed),
        Err(LockError::NotHolder { ticket: b })
    );
    assert_eq!(lock.holder(), Some(a));
    assert_eq!(lock.unreleased(), 0);
}

#[test]
fn writers_wait_before_the_lock_while_the_broadcast_queue_is_full() {
    let mut lock = WriteLock::with_bound(2);
    for n in 0..2 {
        let t = lock.request(n);
        assert_eq!(lock.grant().map(|g| g.0), Some(t));
        lock.release(t, Release::Committed).unwrap();
    }
    assert_eq!(lock.unreleased(), 2);
    let waiting = lock.request(9);
    assert_eq!(lock.grant(), None, "granted at the bound");
    assert_eq!(lock.holder(), None, "the waiting task holds nothing");
    lock.commit_released().unwrap();
    assert_eq!(lock.grant(), Some((waiting, 9)));
}

#[test]
fn a_quiet_point_task_waits_until_every_commit_is_released() {
    // RED kira #2455: a closure, a subscription or M0 reads at M = next() - 1, which is a quiet
    // point only when every commit up to M is released by the broadcast fence (5.4).
    let mut lock = WriteLock::with_bound(4);
    let w = lock.request("writer");
    assert_eq!(lock.grant().map(|g| g.0), Some(w));
    lock.release(w, Release::Committed).unwrap();
    let closure = lock.request_quiet("closure");
    let behind = lock.request("writer behind");
    assert_eq!(
        lock.grant(),
        None,
        "quiet point granted with a commit unreleased"
    );
    assert_eq!(
        lock.grant(),
        None,
        "a writer overtook the queued quiet point task"
    );
    lock.commit_released().unwrap();
    assert_eq!(lock.grant(), Some((closure, "closure")));
    lock.release(closure, Release::Nothing).unwrap();
    assert_eq!(lock.grant(), Some((behind, "writer behind")));
}

#[test]
fn writers_are_granted_with_unreleased_commits_below_the_bound() {
    let mut lock = WriteLock::with_bound(4);
    let w = lock.request(1);
    lock.grant().unwrap();
    lock.release(w, Release::Committed).unwrap();
    let next = lock.request(2);
    assert_eq!(lock.grant(), Some((next, 2)));
}

#[test]
fn a_quiet_point_task_is_granted_at_once_when_all_is_released() {
    let mut lock = WriteLock::new();
    let q = lock.request_quiet(());
    assert_eq!(lock.grant().map(|g| g.0), Some(q));
}

#[test]
fn a_rollback_does_not_count_against_the_broadcast_bound() {
    let mut lock = WriteLock::with_bound(1);
    let t = lock.request(());
    lock.grant().unwrap();
    lock.release(t, Release::Nothing).unwrap();
    let u = lock.request(());
    assert_eq!(lock.grant().map(|g| g.0), Some(u));
}

#[test]
fn the_broadcast_queue_cannot_release_more_than_was_pushed() {
    let mut lock: WriteLock<()> = WriteLock::new();
    assert_eq!(lock.commit_released(), Err(LockError::NothingToRelease));
}

#[test]
fn the_default_bound_is_1024_commits() {
    let mut lock = WriteLock::new();
    for _ in 0..BROADCAST_COMMITS {
        let t = lock.request(());
        lock.grant().unwrap();
        lock.release(t, Release::Committed).unwrap();
    }
    lock.request(());
    assert_eq!(lock.grant(), None);
    lock.commit_released().unwrap();
    assert!(lock.grant().is_some());
}

#[test]
fn quiet_point_tasks_queued_between_writers_share_one_emptying() {
    // RED kira #2498: each quiet point task at the head used to make every writer behind it wait
    // until the broadcast queue was empty, once per task.
    let mut lock = WriteLock::with_bound(8);
    let w = lock.request("w0");
    lock.grant().unwrap();
    lock.release(w, Release::Committed).unwrap();
    let q1 = lock.request_quiet("q1");
    let w1 = lock.request("w1");
    let q2 = lock.request_quiet("q2");
    let w2 = lock.request("w2");
    let q3 = lock.request_quiet("q3");
    assert_eq!(
        lock.grant(),
        None,
        "quiet point granted with a commit unreleased"
    );
    assert!(!lock.in_quiet_phase());
    lock.commit_released().unwrap();
    for (t, name) in [(q1, "q1"), (q2, "q2"), (q3, "q3")] {
        assert_eq!(
            lock.grant(),
            Some((t, name)),
            "quiet point tasks of one phase"
        );
        lock.release(t, Release::Nothing).unwrap();
    }
    assert!(!lock.in_quiet_phase(), "the phase ends with its last task");
    assert_eq!(lock.grant(), Some((w1, "w1")));
    lock.release(w1, Release::Committed).unwrap();
    assert_eq!(
        lock.grant(),
        Some((w2, "w2")),
        "a writer waited for a second emptying"
    );
}

#[test]
fn a_quiet_point_task_queued_during_a_phase_waits_behind_the_writers() {
    // 5.4: rows left by a closure wait for the next closure behind the writers already queued.
    let mut lock = WriteLock::with_bound(8);
    let closure = lock.request_quiet("closure");
    let w = lock.request("writer");
    assert_eq!(lock.grant(), Some((closure, "closure")));
    let again = lock.request_quiet("next closure");
    lock.release(closure, Release::Nothing).unwrap();
    assert!(!lock.in_quiet_phase());
    assert_eq!(
        lock.grant(),
        Some((w, "writer")),
        "the next closure overtook the writer"
    );
    lock.release(w, Release::Committed).unwrap();
    assert_eq!(
        lock.grant(),
        None,
        "quiet point granted with a commit unreleased"
    );
    lock.commit_released().unwrap();
    assert_eq!(lock.grant(), Some((again, "next closure")));
}

#[test]
fn a_phase_grants_nothing_while_a_commit_is_unreleased() {
    // A quiet point task pushes no commit. If a holder in a phase reported one anyway, the next
    // task of the phase must still wait for a quiet point.
    let mut lock = WriteLock::with_bound(8);
    let a = lock.request_quiet("a");
    let w = lock.request("w");
    let b = lock.request_quiet("b");
    assert_eq!(lock.grant(), Some((a, "a")));
    lock.release(a, Release::Committed).unwrap();
    assert_eq!(
        lock.grant(),
        None,
        "quiet point granted with a commit unreleased"
    );
    assert!(lock.in_quiet_phase());
    lock.commit_released().unwrap();
    assert_eq!(lock.grant(), Some((b, "b")));
    lock.release(b, Release::Nothing).unwrap();
    assert_eq!(lock.grant(), Some((w, "w")));
}

#[test]
fn interleaved_quiet_point_tasks_cost_writers_one_emptying_not_one_each() {
    // Measurement in the model: how often must the broadcast queue run empty before 100 writers
    // interleaved with 100 quiet point tasks are through? Strict FIFO needs 100 (writer
    // throughput = 1 / broadcast latency, kira #2498); a quiet phase needs 1.
    let mut lock = WriteLock::with_bound(BROADCAST_COMMITS);
    let w = lock.request(false);
    lock.grant().unwrap();
    lock.release(w, Release::Committed).unwrap();
    for _ in 0..100 {
        lock.request_quiet(true);
        lock.request(false);
    }
    let mut emptyings = 0;
    let mut writers = 0;
    while lock.queued() > 0 {
        match lock.grant() {
            Some((t, quiet)) => {
                let outcome = if quiet {
                    Release::Nothing
                } else {
                    writers += 1;
                    Release::Committed
                };
                lock.release(t, outcome).unwrap();
            }
            None => {
                while lock.unreleased() > 0 {
                    lock.commit_released().unwrap();
                }
                emptyings += 1;
            }
        }
    }
    assert_eq!(writers, 100);
    assert_eq!(emptyings, 1);
}

/// Small deterministic generator for the property test.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}

#[test]
fn random_schedules_keep_quiet_points_and_bound_every_wait_by_one_phase() {
    // A phase begins with a quiet point grant made outside a phase. A writer waits for at most
    // one phase begun after its request; a quiet point task is granted in exactly the first one.
    for seed in 0..500u64 {
        let mut rng = Lcg(seed);
        let bound = 1 + rng.below(4) as usize;
        let mut lock = WriteLock::with_bound(bound);
        // Per task: whether it needs a quiet point, and the phases begun at its request.
        let mut tasks: Vec<(bool, u64)> = Vec::new();
        let mut phases = 0u64;
        let mut holder = None;
        let mut step = 0;
        while step < 400 || lock.queued() > 0 || holder.is_some() {
            step += 1;
            assert!(step < 100_000, "seed {seed}: no progress");
            let pick = if step < 400 {
                rng.below(6)
            } else {
                2 + rng.below(4)
            };
            match pick {
                0 => {
                    tasks.push((false, phases));
                    lock.request(tasks.len() - 1);
                }
                1 => {
                    tasks.push((true, phases));
                    lock.request_quiet(tasks.len() - 1);
                }
                2 | 3 => {
                    let unreleased = lock.unreleased();
                    let in_phase = lock.in_quiet_phase();
                    let Some((t, i)) = lock.grant() else { continue };
                    let (quiet, at_request) = tasks[i];
                    if quiet {
                        assert_eq!(unreleased, 0, "seed {seed}: quiet point not quiet");
                        if !in_phase {
                            phases += 1;
                        }
                        assert_eq!(phases - at_request, 1, "seed {seed}: quiet task phase");
                    } else {
                        assert!(unreleased < bound, "seed {seed}: writer at the bound");
                        assert!(!in_phase, "seed {seed}: writer granted inside a phase");
                        assert!(
                            phases - at_request <= 1,
                            "seed {seed}: writer waited two phases"
                        );
                    }
                    holder = Some((t, quiet));
                }
                4 => {
                    if let Some((t, quiet)) = holder.take() {
                        let outcome = if !quiet && rng.below(4) > 0 {
                            Release::Committed
                        } else {
                            Release::Nothing
                        };
                        lock.release(t, outcome).unwrap();
                    }
                }
                _ => {
                    if lock.unreleased() > 0 {
                        lock.commit_released().unwrap();
                    }
                }
            }
        }
        assert_eq!(lock.holder(), None, "seed {seed}");
    }
}

// ---------------------------------------------------------------------------------------------
// Send queue
// ---------------------------------------------------------------------------------------------

#[test]
fn live_frames_over_the_limit_overload_and_close_the_session() {
    let mut queue = SendQueue::new();
    assert_eq!(SEND_QUEUE_BYTES, 8 * MIB);
    queue.push_now(8 * MIB - 10).unwrap();
    queue.push_now(10).unwrap();
    assert_eq!(queue.push_now(1), Err(QueueError::Overloaded));
    assert!(queue.is_closed());
    assert_eq!(queue.queued(), 0, "frames not yet written are dropped");
    assert_eq!(queue.push_now(1), Err(QueueError::Closed));
    assert_eq!(queue.push_when_room(1), Err(QueueError::Closed));
}

#[test]
fn catch_up_frames_wait_for_room_and_never_overload() {
    let mut queue = SendQueue::with_limit(100);
    assert_eq!(queue.push_when_room(80), Ok(true));
    assert_eq!(queue.push_when_room(30), Ok(false), "must wait");
    assert!(!queue.is_closed());
    queue.written(40);
    assert_eq!(queue.push_when_room(30), Ok(true));
    assert_eq!(queue.queued(), 70);
}

#[test]
fn an_empty_queue_always_takes_one_waiting_frame() {
    let mut queue = SendQueue::with_limit(10);
    assert_eq!(queue.push_when_room(50), Ok(true));
    assert_eq!(queue.push_when_room(1), Ok(false));
}

#[test]
fn live_frames_count_while_they_stand_behind_a_waiting_catch_up() {
    // D161: live ops queued behind a catch up count to the limit.
    let mut queue = SendQueue::with_limit(100);
    assert_eq!(queue.push_when_room(90), Ok(true));
    assert_eq!(queue.push_when_room(20), Ok(false));
    queue.push_now(10).unwrap();
    assert_eq!(queue.push_now(1), Err(QueueError::Overloaded));
}

#[test]
fn a_starved_session_asks_again_once_below_half() {
    let mut queue = SendQueue::with_limit(100);
    queue.push_now(90).unwrap();
    queue.mark_starved();
    assert!(!queue.written(30), "60 is not below half");
    assert!(queue.written(20), "40 is below half");
    assert!(!queue.written(10), "asks only once");
}

// ---------------------------------------------------------------------------------------------
// Waiting rows and closures
// ---------------------------------------------------------------------------------------------

/// Rows of a fixed size; `hidden` rows fail the filter or `see`, `gone` rows are deleted.
#[derive(Default)]
struct Table {
    size: BTreeMap<RowId, u64>,
    hidden: Vec<RowId>,
    gone: Vec<RowId>,
    reads: Vec<(u32, SubId, RowId)>,
}

impl Table {
    fn sized(rows: impl IntoIterator<Item = (RowId, u64)>) -> Self {
        Self {
            size: rows.into_iter().collect(),
            ..Self::default()
        }
    }
}

impl ClosureReader<u32> for Table {
    type Row = RowId;
    type Error = Infallible;

    fn read(
        &mut self,
        session: &u32,
        sub: SubId,
        row: RowId,
    ) -> Result<Option<ReadRow<RowId>>, Infallible> {
        self.reads.push((*session, sub, row));
        if self.gone.contains(&row) {
            return Ok(None);
        }
        let bytes = self.size.get(&row).copied().unwrap_or(100);
        Ok(Some(ReadRow {
            row,
            encoded_bytes: bytes,
            read_bytes: bytes,
            visible: !self.hidden.contains(&row),
        }))
    }
}

/// Rooms of empty queues: the free bytes are the limit.
fn rooms(entries: &[(u32, u64)]) -> BTreeMap<u32, Room> {
    entries
        .iter()
        .map(|&(s, free)| (s, Room { free, limit: free }))
        .collect()
}

fn with_candidates(w: &mut WaitingRows<u32>, session: u32, sub: SubId, rows: &[RowId]) {
    w.subscribe(session, sub).unwrap();
    w.begin_candidates(&session, sub).unwrap();
    w.add_candidates(&session, sub, rows.iter().copied(), true)
        .unwrap();
}

#[test]
fn a_closure_serves_waiting_rows_at_its_mark_and_ends_the_snapshot() {
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 7, &[row(1), row(2)]);
    let mut table = Table::default();
    let out = w
        .run_closure(ServerSeq(40), &rooms(&[(1, MIB)]), &mut table)
        .unwrap();
    assert_eq!(out.pages.len(), 1);
    let page = &out.pages[0];
    assert_eq!((page.session, page.sub, page.seq), (1, 7, ServerSeq(40)));
    assert_eq!(page.rows, vec![row(1), row(2)]);
    assert!(page.last);
    assert!(w.is_live(&1, 7, row(1), ServerSeq(41)));
    assert!(
        !w.is_live(&1, 7, row(1), ServerSeq(40)),
        "op at the mark is in the row"
    );
    assert!(!w.has_work());
}

#[test]
fn a_waiting_row_gets_no_op() {
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 7, &[row(1)]);
    assert!(!w.is_live(&1, 7, row(1), ServerSeq(99)));
}

#[test]
fn a_closure_reads_at_most_256_rows() {
    let mut w = WaitingRows::new();
    let rows: Vec<RowId> = (0..300).map(row).collect();
    with_candidates(&mut w, 1, 1, &rows);
    let mut table = Table::default();
    let out = w
        .run_closure(ServerSeq(1), &rooms(&[(1, 8 * MIB)]), &mut table)
        .unwrap();
    assert_eq!(out.rows_read, CLOSURE_ROWS);
    assert_eq!(out.pages[0].rows.len(), CLOSURE_ROWS);
    assert!(!out.pages[0].last);
    assert_eq!(w.waiting(&1, 1), 300 - CLOSURE_ROWS);
    let out = w
        .run_closure(ServerSeq(2), &rooms(&[(1, 8 * MIB)]), &mut table)
        .unwrap();
    assert_eq!(out.pages[0].rows.len(), 300 - CLOSURE_ROWS);
    assert!(out.pages[0].last);
}

#[test]
fn a_closure_stops_after_4_mib_but_reads_at_least_one_row() {
    let mut w = WaitingRows::new();
    let rows: Vec<RowId> = (0..10).map(row).collect();
    with_candidates(&mut w, 1, 1, &rows);
    // Six rows of 700 KiB pass 4 MiB.
    let mut table = Table::sized(rows.iter().map(|r| (*r, 700 * 1024)));
    let out = w
        .run_closure(ServerSeq(1), &rooms(&[(1, 8 * MIB)]), &mut table)
        .unwrap();
    assert_eq!(out.rows_read, 6);
    assert!(out.bytes_read >= CLOSURE_BYTES);

    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 1, &[row(1), row(2)]);
    let mut table = Table::sized([(row(1), 5 * MIB), (row(2), 1)]);
    let out = w
        .run_closure(ServerSeq(1), &rooms(&[(1, 8 * MIB)]), &mut table)
        .unwrap();
    assert_eq!(out.rows_read, 1, "one row over 4 MiB is still read");
}

#[test]
fn rows_are_picked_round_robin_over_sessions_and_subscriptions() {
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 1, &[row(1), row(2), row(3)]);
    with_candidates(&mut w, 1, 2, &[row(11), row(12)]);
    with_candidates(&mut w, 2, 1, &[row(21), row(22), row(23)]);
    let mut table = Table::default();
    w.run_closure(ServerSeq(1), &rooms(&[(1, MIB), (2, MIB)]), &mut table)
        .unwrap();
    let order: Vec<(u32, SubId)> = table.reads.iter().map(|r| (r.0, r.1)).collect();
    assert_eq!(
        order[..6],
        [(1, 1), (2, 1), (1, 2), (2, 1), (1, 1), (2, 1)],
        "sessions alternate, a session alternates its subscriptions"
    );
}

#[test]
fn invisible_and_deleted_rows_leave_the_waiting_rows_unsent() {
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 1, &[row(1), row(2), row(3)]);
    let mut table = Table {
        hidden: vec![row(2)],
        gone: vec![row(3)],
        ..Table::default()
    };
    let out = w
        .run_closure(ServerSeq(5), &rooms(&[(1, MIB)]), &mut table)
        .unwrap();
    assert_eq!(out.pages[0].rows, vec![row(1)]);
    assert!(out.pages[0].last);
    assert!(!w.is_live(&1, 1, row(2), ServerSeq(6)));
}

#[test]
fn a_row_that_does_not_fit_the_room_waits_and_its_session_starves() {
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 1, &[row(1), row(2)]);
    with_candidates(&mut w, 2, 1, &[row(3)]);
    let mut table = Table::sized([(row(1), 60), (row(2), 60), (row(3), 60)]);
    let out = w
        .run_closure(ServerSeq(3), &rooms(&[(1, 100), (2, 100)]), &mut table)
        .unwrap();
    assert_eq!(out.starved, vec![1]);
    let one: Vec<_> = out.pages.iter().filter(|p| p.session == 1).collect();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].rows, vec![row(1)]);
    assert!(!one[0].last, "a row still waits");
    assert_eq!(w.waiting(&1, 1), 1);
    assert!(out.pages.iter().any(|p| p.session == 2 && p.last));
}

#[test]
fn a_session_without_room_is_skipped_and_others_proceed() {
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 1, &[row(1)]);
    with_candidates(&mut w, 2, 1, &[row(2)]);
    let mut table = Table::default();
    let out = w
        .run_closure(ServerSeq(3), &rooms(&[(2, MIB)]), &mut table)
        .unwrap();
    assert_eq!(out.starved, vec![1]);
    assert_eq!(out.pages.len(), 1);
    assert_eq!(out.pages[0].session, 2);
    assert_eq!(w.waiting(&1, 1), 1);
}

#[test]
fn only_one_closure_is_queued_and_requests_join_it() {
    let mut w: WaitingRows<u32> = WaitingRows::new();
    assert!(w.request_closure());
    assert!(!w.request_closure());
    assert!(w.closure_queued());
    w.run_closure(ServerSeq(0), &BTreeMap::new(), &mut Table::default())
        .unwrap();
    assert!(!w.closure_queued());
    assert!(
        w.request_closure(),
        "a request after the start queues the next one"
    );
}

#[test]
fn an_evicted_waiting_row_is_dropped_without_a_message() {
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 1, &[row(1), row(2)]);
    w.run_closure(ServerSeq(1), &rooms(&[(1, 150)]), &mut Table::default())
        .unwrap();
    // row(1) served, row(2) still waiting (room for one row of 100 bytes).
    assert_eq!(w.evict(&1, 1, row(2)), EvictAction::Drop);
    assert_eq!(w.waiting(&1, 1), 0, "the evicted row no longer waits");
    assert_eq!(w.evict(&1, 1, row(1)), EvictAction::Send);
    assert_eq!(w.evict(&1, 1, row(1)), EvictAction::Ignore);
    assert!(!w.is_live(&1, 1, row(1), ServerSeq(9)));
}

#[test]
fn an_entering_row_waits_unless_served() {
    let mut w = WaitingRows::new();
    w.resubscribe(1, 1, [row(1)], ServerSeq(10)).unwrap();
    w.enter(&1, 1, row(1)).unwrap();
    w.enter(&1, 1, row(2)).unwrap();
    assert_eq!(w.waiting(&1, 1), 1);
    assert!(
        w.is_live(&1, 1, row(1), ServerSeq(11)),
        "held rows are live from M0"
    );
    assert_eq!(w.enter(&1, 2, row(3)), Err(WaitError::Unknown { sub: 2 }));
}

#[test]
fn candidates_are_read_lazily_one_page_at_a_time() {
    let mut w = WaitingRows::new();
    w.subscribe(1, 1).unwrap();
    assert!(w.wants_candidates(&1, 1));
    w.begin_candidates(&1, 1).unwrap();
    assert!(!w.wants_candidates(&1, 1), "a page is being read");
    let page: Vec<RowId> = (0..CANDIDATE_PAGE as u64).map(row).collect();
    w.add_candidates(&1, 1, page, false).unwrap();
    assert!(!w.wants_candidates(&1, 1), "1 000 rows wait");
    w.run_closure(ServerSeq(1), &rooms(&[(1, 8 * MIB)]), &mut Table::default())
        .unwrap();
    assert!(w.wants_candidates(&1, 1), "fewer than 1 000 rows wait");
    assert_eq!(
        w.add_candidates(&1, 1, [row(5000)], true),
        Err(WaitError::NoCandidateRead { sub: 1 })
    );
}

#[test]
fn served_candidates_are_skipped() {
    let mut w = WaitingRows::new();
    w.resubscribe(1, 1, [row(1)], ServerSeq(4)).unwrap();
    w.begin_candidates(&1, 1).unwrap();
    w.add_candidates(&1, 1, [row(1), row(2)], true).unwrap();
    assert_eq!(w.waiting(&1, 1), 1);
}

#[test]
fn an_empty_subscription_gets_one_empty_last_page() {
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 3, &[]);
    assert!(w.has_work());
    let out = w
        .run_closure(ServerSeq(8), &rooms(&[(1, MIB)]), &mut Table::default())
        .unwrap();
    assert_eq!(out.pages.len(), 1);
    assert!(out.pages[0].rows.is_empty() && out.pages[0].last);
    let out = w
        .run_closure(ServerSeq(9), &rooms(&[(1, MIB)]), &mut Table::default())
        .unwrap();
    assert!(out.pages.is_empty(), "the last page is sent once");
}

#[test]
fn a_read_error_changes_nothing() {
    struct Failing;
    impl ClosureReader<u32> for Failing {
        type Row = RowId;
        type Error = &'static str;
        fn read(
            &mut self,
            _: &u32,
            _: SubId,
            _: RowId,
        ) -> Result<Option<ReadRow<RowId>>, &'static str> {
            Err("connection lost")
        }
    }
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 1, &[row(1)]);
    assert_eq!(
        w.run_closure(ServerSeq(1), &rooms(&[(1, MIB)]), &mut Failing),
        Err("connection lost")
    );
    assert_eq!(w.waiting(&1, 1), 1);
    assert!(!w.is_live(&1, 1, row(1), ServerSeq(2)));
}

#[test]
fn a_failed_closure_stays_queued() {
    // RED kira #2456 F3: the request is not consumed by a closure that read nothing.
    struct Failing;
    impl ClosureReader<u32> for Failing {
        type Row = RowId;
        type Error = &'static str;
        fn read(
            &mut self,
            _: &u32,
            _: SubId,
            _: RowId,
        ) -> Result<Option<ReadRow<RowId>>, &'static str> {
            Err("connection lost")
        }
    }
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 1, &[row(1)]);
    assert!(w.request_closure());
    assert!(
        w.run_closure(ServerSeq(1), &rooms(&[(1, MIB)]), &mut Failing)
            .is_err()
    );
    assert!(w.closure_queued(), "the waiting row lost its closure");
    assert!(
        !w.request_closure(),
        "a request joins the closure still owed"
    );
    w.run_closure(ServerSeq(1), &rooms(&[(1, MIB)]), &mut Table::default())
        .unwrap();
    assert!(!w.closure_queued());
}

#[test]
fn a_row_larger_than_the_whole_queue_is_reported_instead_of_waiting() {
    // RED kira #2456 F4: a row that never fits must not leave its subscription waiting.
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 1, &[row(1), row(2)]);
    with_candidates(&mut w, 2, 1, &[row(3)]);
    let mut table = Table::sized([(row(1), 500), (row(2), 10), (row(3), 10)]);
    let rooms: BTreeMap<u32, Room> = [
        (
            1,
            Room {
                free: 400,
                limit: 400,
            },
        ),
        (
            2,
            Room {
                free: 400,
                limit: 400,
            },
        ),
    ]
    .into();
    let out = w.run_closure(ServerSeq(2), &rooms, &mut table).unwrap();
    assert_eq!(out.oversized, vec![(1, 1, row(1))]);
    assert!(
        out.starved.is_empty(),
        "an oversized row is not a starved session"
    );
    assert_eq!(
        w.waiting(&1, 1),
        1,
        "only the oversized row left the waiting rows"
    );
    assert!(!w.is_live(&1, 1, row(1), ServerSeq(3)));
    assert!(
        out.pages
            .iter()
            .any(|p| p.session == 2 && p.rows == vec![row(3)])
    );

    // A row that fits the limit but not the free bytes still waits for room.
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 1, &[row(1)]);
    let mut table = Table::sized([(row(1), 300)]);
    let rooms: BTreeMap<u32, Room> = [(
        1,
        Room {
            free: 100,
            limit: 400,
        },
    )]
    .into();
    let out = w.run_closure(ServerSeq(2), &rooms, &mut table).unwrap();
    assert!(out.oversized.is_empty());
    assert_eq!(out.starved, vec![1]);
}

#[test]
fn the_space_of_a_queue_is_its_free_bytes_and_limit() {
    let mut q = SendQueue::with_limit(1_000);
    q.push_now(300).unwrap();
    assert_eq!(
        q.space(),
        Room {
            free: 700,
            limit: 1_000
        }
    );
}

#[test]
fn closing_a_session_forgets_its_rows() {
    let mut w = WaitingRows::new();
    with_candidates(&mut w, 1, 1, &[row(1)]);
    w.close_session(&1);
    assert!(!w.has_work());
    assert_eq!(w.evict(&1, 1, row(1)), EvictAction::Ignore);
}
