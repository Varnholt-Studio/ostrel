// Tests for the confirmed state of the local replica: server ops, own ops confirmed by `Ack`,
// snapshot pages, rejected ops and the optimistic view (ARCHITECTURE 5.4, D62, D151, D154,
// D190). The data store runs on the in-memory IndexedDB stand in.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { encode } from '../../crdt/canon/canon.mjs';
import { openOutbox } from '../outbox/outbox.mjs';
import { MemoryIndexedDB, MemoryKeyRange } from '../outbox/testing/memory_indexeddb.mjs';
import { EventQueue } from './events.mjs';
import { ApplyError, applyOp, openLocalReplica, viewRow } from './apply.mjs';
import { runSession } from './session.mjs';
import { FakeSocket, memoryIdentity, until, welcome } from './testing/fake_server.mjs';

const ALICE = 'user-alice';
const BOB = 'user-bob';
const A = '00000000000000a1';
const B = '00000000000000b2';
const C = '00000000000000c3';
const KINDS = { Issue: { labels: 'set', votes: 'map' } };
const ISSUE = '0000000003e80000' + B;

const seq = (n) => n.toString(16).padStart(16, '0');
const opId = (replica, n) => replica + n.toString(16).padStart(8, '0');
const hlc = (wallMs, replica, counter = 0) =>
  wallMs.toString(16).padStart(12, '0') + counter.toString(16).padStart(4, '0') + replica;

function wireOp(replica, n, wallMs, body, row = ISSUE) {
  return { id: opId(replica, n), hlc: hlc(wallMs, replica), model: 'Issue', row, body };
}

// The outbox entry of a wire op, as `outbox.append` stores it.
function entryOf(op, user = ALICE) {
  const replica = op.id.slice(0, 16);
  const n = Number.parseInt(op.id.slice(16), 16);
  return {
    opId: op.id,
    user,
    replica,
    seq: n,
    hlc: op.hlc,
    op: { model: op.model, row: op.row, body: op.body },
  };
}

const makeIssue = (wallMs = 1000) =>
  wireOp(B, 1, wallMs, { make: { title: 'first', labels: ['bug'], votes: [['ann', 1]] } });

async function store(indexedDB = new MemoryIndexedDB()) {
  return openLocalReplica({ indexedDB, IDBKeyRange: MemoryKeyRange, kinds: KINDS });
}

async function confirmedRow(local, user, replica, row = ISSUE) {
  return (await local.row({ user, replica, model: 'Issue', row }))?.row ?? null;
}

test('ac_22_late_old_stamp_wins: the op last in log order wins on every client (D190)', async () => {
  // Replica A writes the title offline at wall time 1500, replica B writes it at 3000 and syncs
  // first (log position 3), then A syncs (position 4). The server applies its log in order, so
  // it holds A's value; every client must hold it too, whatever the stamps say. The same holds
  // for a `Rank`, a whole `List` and each key of a `Map`.
  const make = makeIssue();
  const fromA = [
    wireOp(A, 1, 1500, { set: { field: 'title', value: 'offline A' } }),
    wireOp(A, 2, 1500, { set: { field: 'rank', value: 'a0' } }),
    wireOp(A, 3, 1500, { put: { field: 'votes', key: 'ann', value: 7 } }),
    wireOp(A, 4, 1500, { set: { field: 'steps', value: ['a'] } }),
  ];
  const fromB = [
    wireOp(B, 2, 3000, { set: { field: 'title', value: 'online B' } }),
    wireOp(B, 3, 3000, { set: { field: 'rank', value: 'b0' } }),
    wireOp(B, 4, 3000, { remove_key: { field: 'votes', key: 'ann' } }),
    wireOp(B, 5, 3000, { set: { field: 'steps', value: ['b', 'c'] } }),
  ];
  // Log: make at 1, B's batch at 2 to 5, A's batch at 6 to 9.
  const logged = [make, ...fromB, ...fromA].map((op, i) => ({ seq: seq(i + 1), op }));

  // An observer that receives every op through the session stream.
  const observer = await store();
  await observer.applyOps({ user: BOB, replica: C, ops: logged });

  // Replica A: the make and B's ops arrive as ops, its own ops are confirmed by one `Ack` at 9.
  const clientA = await store();
  await clientA.applyOps({ user: ALICE, replica: A, ops: logged.slice(0, 5) });
  await clientA.applyAck({ user: ALICE, replica: A, seq: seq(9), entries: fromA.map((op) => entryOf(op)) });

  // Replica B: its own make and ops are confirmed by `Ack`, then A's ops arrive.
  const clientB = await store();
  await clientB.applyAck({ user: BOB, replica: B, seq: seq(1), entries: [entryOf(make, BOB)] });
  await clientB.applyAck({ user: BOB, replica: B, seq: seq(5), entries: fromB.map((op) => entryOf(op, BOB)) });
  await clientB.applyOps({ user: BOB, replica: B, ops: logged.slice(5) });

  // A client that got a `Snapshot` at position 5 (B's values) in between.
  const late = await store();
  await late.applySnapshot({
    user: BOB,
    replica: C,
    model: 'Issue',
    seq: seq(5),
    rows: [snapshotAt5(logged)],
  });
  await late.applyOps({ user: BOB, replica: C, ops: logged });

  const expected = {
    id: ISSUE,
    fields: { title: 'offline A', rank: 'a0', steps: ['a'], made: 1000, changed: 1500 },
    sets: { labels: [['bug', [make.id]]] },
    maps: { votes: [['ann', { hlc: fromA[2].hlc, value: 7 }]] },
  };
  for (const [name, local, user, replica] of [
    ['observer', observer, BOB, C],
    ['replica A', clientA, ALICE, A],
    ['replica B', clientB, BOB, B],
    ['after a snapshot', late, BOB, C],
  ]) {
    const row = await confirmedRow(local, user, replica);
    assert.equal(encode(row), encode(expected), name);
    assert.equal(await local.appliedServerSeq({ user, replica }), seq(9), name);
  }
  // Documented limit (SPEC 12.16): `changed` went down from 3000 to 1500.
});

// The server row after position 5, as a `Snapshot` page carries it.
function snapshotAt5(logged) {
  let row = null;
  for (const { op } of logged.slice(0, 5)) row = applyOp(row, op, KINDS);
  return row;
}

test('an op at or below the mark is dropped; a redelivered add after its remove stays removed (D62)', async () => {
  const local = await store();
  const make = makeIssue();
  const add = wireOp(A, 1, 2000, { add: { field: 'labels', elem: 'ui' } });
  const remove = wireOp(C, 1, 2500, { remove: { field: 'labels', elem: 'ui', tags: [add.id] } });
  await local.applyOps({
    user: ALICE,
    replica: B,
    ops: [
      { seq: seq(1), op: make },
      { seq: seq(2), op: add },
      { seq: seq(3), op: remove },
    ],
  });
  // A reconnect overlap delivers the add again, below the mark.
  const result = await local.applyOps({ user: ALICE, replica: B, ops: [{ seq: seq(2), op: add }] });
  assert.deepEqual(result, { applied: [], dropped: [add.id], own: [] });
  assert.deepEqual((await confirmedRow(local, ALICE, B)).sets.labels, [['bug', [make.id]]]);
  assert.equal(await local.appliedServerSeq({ user: ALICE, replica: B }), seq(3));
  // Ops of the own replica in the catch up are reported, so they leave the outbox.
  const own = await local.applyOps({ user: ALICE, replica: B, ops: [{ seq: seq(1), op: make }] });
  assert.deepEqual(own, { applied: [], dropped: [make.id], own: [make.id] });
});

test('an ack applies own ops at its position with the accepted stamps', async () => {
  const local = await store();
  const make = wireOp(A, 1, 1000, { make: { title: 't', labels: [], votes: [] } }, '0000000003e80000' + A);
  const title = wireOp(A, 2, 1200, { set: { field: 'title', value: 'u' } }, make.row);
  const accepted = hlc(4000, A);
  const result = await local.applyAck({
    user: ALICE,
    replica: A,
    seq: seq(8),
    entries: [entryOf(make), entryOf(title)],
    restamped: [[title.id, accepted]],
  });
  assert.deepEqual(result, { applied: [make.id, title.id], skipped: [] });
  const stored = await local.row({ user: ALICE, replica: A, model: 'Issue', row: make.row });
  assert.equal(stored.mark, seq(8));
  assert.deepEqual(stored.row.fields, { title: 'u', made: 1000, changed: 4000 });
  assert.equal(await local.appliedServerSeq({ user: ALICE, replica: A }), seq(8));
});

test('an own op at or below the mark is not applied again; it only leaves the pending ops (AC-44)', async () => {
  const local = await store();
  const make = makeIssue();
  const page = applyOp(null, make, KINDS);
  page.fields.title = 'from the snapshot';
  await local.applySnapshot({ user: ALICE, replica: A, model: 'Issue', seq: seq(6), rows: [page] });
  const mine = wireOp(A, 1, 2000, { set: { field: 'title', value: 'mine' } });
  // The snapshot at 6 already holds the op logged at 6 (and the one at 4, a replayed ack).
  for (const at of [6, 4]) {
    const result = await local.applyAck({ user: ALICE, replica: A, seq: seq(at), entries: [entryOf(mine)] });
    assert.deepEqual(result, { applied: [], skipped: [mine.id] });
  }
  assert.equal((await confirmedRow(local, ALICE, A)).fields.title, 'from the snapshot');
  assert.equal(await local.appliedServerSeq({ user: ALICE, replica: A }), seq(6));
});

test('a snapshot row replaces the confirmed row without a merge and keeps its mark per row', async () => {
  const local = await store();
  const make = makeIssue();
  const other = wireOp(B, 9, 1000, { make: { title: 'x', labels: [], votes: [] } }, '0000000003e90000' + B);
  const add = wireOp(C, 1, 2000, { add: { field: 'labels', elem: 'ui' } });
  await local.applyOps({
    user: ALICE,
    replica: A,
    ops: [
      { seq: seq(1), op: make },
      { seq: seq(2), op: other },
      { seq: seq(3), op: add },
    ],
  });
  // The page holds the row without `ui` and without `votes`: no merge brings them back.
  const page = { id: ISSUE, fields: { title: 'server', made: 1000, changed: 5000 }, sets: { labels: [] } };
  const result = await local.applySnapshot({ user: ALICE, replica: A, model: 'Issue', seq: seq(7), rows: [page] });
  assert.deepEqual(result, { replaced: 1 });
  const stored = await local.row({ user: ALICE, replica: A, model: 'Issue', row: ISSUE });
  assert.equal(stored.mark, seq(7));
  assert.deepEqual(stored.row, { ...page, maps: {} });
  assert.equal((await local.row({ user: ALICE, replica: A, model: 'Issue', row: other.row })).mark, seq(2));
  assert.equal(await local.appliedServerSeq({ user: ALICE, replica: A }), seq(7));
  // A page below the mark changes nothing (marks never decrease within a session).
  const stale = await local.applySnapshot({
    user: ALICE,
    replica: A,
    model: 'Issue',
    seq: seq(5),
    rows: [{ ...page, fields: { title: 'old' } }],
  });
  assert.deepEqual(stale, { replaced: 0 });
  assert.equal((await confirmedRow(local, ALICE, A)).fields.title, 'server');
});

test('set adds keep one tag per replica and ignore replays; removes ignore unknown tags', () => {
  let row = applyOp(null, makeIssue(), KINDS);
  const first = wireOp(A, 3, 2000, { add: { field: 'labels', elem: 'ui' } });
  const again = wireOp(A, 5, 2100, { add: { field: 'labels', elem: 'ui' } });
  const replay = wireOp(A, 4, 2200, { add: { field: 'labels', elem: 'ui' } });
  const other = wireOp(C, 1, 2300, { add: { field: 'labels', elem: 'ui' } });
  for (const op of [first, again, replay, other]) row = applyOp(row, op, KINDS);
  assert.deepEqual(row.sets.labels, [
    ['bug', [opId(B, 1)]],
    ['ui', [again.id, other.id]],
  ]);
  // The remove saw only A's tag: C's concurrent add survives (add wins).
  row = applyOp(row, wireOp(B, 7, 2400, { remove: { field: 'labels', elem: 'ui', tags: [again.id, first.id] } }), KINDS);
  assert.deepEqual(row.sets.labels[1], ['ui', [other.id]]);
  row = applyOp(row, wireOp(B, 8, 2500, { remove: { field: 'labels', elem: 'ui', tags: [other.id] } }), KINDS);
  assert.deepEqual(row.sets.labels, [['bug', [opId(B, 1)]]]);
});

test('ops on a dropped or unknown row leave no row; a drop deletes the confirmed row', async () => {
  const local = await store();
  const make = makeIssue();
  const drop = wireOp(B, 2, 2000, { drop: {} });
  const late = wireOp(C, 1, 2100, { set: { field: 'title', value: 'z' } });
  await local.applyOps({
    user: ALICE,
    replica: A,
    ops: [
      { seq: seq(1), op: make },
      { seq: seq(2), op: drop },
      { seq: seq(3), op: late },
    ],
  });
  assert.equal(await local.row({ user: ALICE, replica: A, model: 'Issue', row: ISSUE }), null);
  assert.deepEqual(await local.rows({ user: ALICE, replica: A, model: 'Issue' }), []);
  assert.equal(await local.appliedServerSeq({ user: ALICE, replica: A }), seq(3));
});

test('the row and the mark change in one transaction or not at all', async () => {
  const indexedDB = new MemoryIndexedDB();
  const local = await store(indexedDB);
  await local.applyOps({ user: ALICE, replica: A, ops: [{ seq: seq(1), op: makeIssue() }] });
  indexedDB.failNext(({ store: name, method }) => name === 'marks' && method === 'put');
  const title = wireOp(B, 2, 2000, { set: { field: 'title', value: 'lost' } });
  await assert.rejects(local.applyOps({ user: ALICE, replica: A, ops: [{ seq: seq(2), op: title }] }));
  assert.equal((await confirmedRow(local, ALICE, A)).fields.title, 'first');
  assert.equal(await local.appliedServerSeq({ user: ALICE, replica: A }), seq(1));
  // The same op applies once the store works again.
  await local.applyOps({ user: ALICE, replica: A, ops: [{ seq: seq(2), op: title }] });
  assert.equal((await confirmedRow(local, ALICE, A)).fields.title, 'lost');
});

test('the mark and the rows are kept per user and replica', async () => {
  const local = await store();
  await local.applyOps({ user: ALICE, replica: A, ops: [{ seq: seq(4), op: makeIssue() }] });
  assert.equal(await local.appliedServerSeq({ user: BOB, replica: A }), null);
  assert.equal(await local.appliedServerSeq({ user: ALICE, replica: C }), null);
  assert.equal(await confirmedRow(local, BOB, A), null);
  assert.equal((await local.rows({ user: ALICE, replica: A, model: 'Issue' })).length, 1);
});

test('evict removes held rows and keeps the mark', async () => {
  const local = await store();
  await local.applyOps({ user: ALICE, replica: A, ops: [{ seq: seq(2), op: makeIssue() }] });
  await local.evict({ user: ALICE, replica: A, model: 'Issue', rows: [ISSUE] });
  assert.equal(await confirmedRow(local, ALICE, A), null);
  assert.equal(await local.appliedServerSeq({ user: ALICE, replica: A }), seq(2));
});

test('the view applies pending ops on top in seq order; their stamps decide nothing', () => {
  const confirmed = applyOp(null, makeIssue(), KINDS);
  // Pending ops with stamps below the confirmed write still show on top, in seq order.
  const pending = [
    entryOf(wireOp(A, 2, 10, { set: { field: 'title', value: 'second' } })),
    entryOf(wireOp(A, 1, 20, { set: { field: 'title', value: 'first' } })),
    entryOf(wireOp(A, 3, 30, { set: { field: 'title', value: 'other row' } }, '0000000003e90000' + A)),
  ];
  const view = viewRow({ model: 'Issue', row: ISSUE, confirmed, pending, kinds: KINDS });
  assert.equal(view.fields.title, 'second');
  assert.equal(view.pending, true);
  assert.equal(view.rejected, false);
  const idle = viewRow({ model: 'Issue', row: ISSUE, confirmed, kinds: KINDS });
  assert.equal(idle.pending, false);
  assert.equal(idle.fields.title, 'first');
});

test('a rejected op leaves the view and marks its row rejected until dismissed (B2-15)', async () => {
  const local = await store();
  const room = '0000000003e80000' + A;
  const make = wireOp(A, 1, 1000, { make: { title: 'room', labels: [], votes: [] } }, room);
  const edit = wireOp(A, 2, 1100, { set: { field: 'title', value: 'renamed' } }, ISSUE);
  await local.applyOps({ user: ALICE, replica: A, ops: [{ seq: seq(1), op: makeIssue() }] });
  await local.reject({ user: ALICE, replica: A, entry: entryOf(make), reason: 'Invalid' });
  await local.reject({ user: ALICE, replica: A, entry: entryOf(edit), reason: 'Denied' });
  const rejected = await local.rejected({ user: ALICE, replica: A });
  assert.deepEqual(rejected.map((r) => [r.opId, r.reason]), [
    [make.id, 'Invalid'],
    [edit.id, 'Denied'],
  ]);
  // The edit was rolled back: the confirmed value shows, marked rejected.
  const issue = viewRow({
    model: 'Issue',
    row: ISSUE,
    confirmed: await confirmedRow(local, ALICE, A),
    rejected,
    kinds: KINDS,
  });
  assert.equal(issue.fields.title, 'first');
  assert.equal(issue.rejected, true);
  // The rejected make never reached the confirmed state; it shows marked rejected.
  const shown = viewRow({ model: 'Issue', row: room, confirmed: null, rejected, kinds: KINDS });
  assert.equal(shown.fields.title, 'room');
  assert.equal(shown.rejected, true);
  assert.equal(await confirmedRow(local, ALICE, A, room), null);
  await local.dismissRejected({ user: ALICE, replica: A, opIds: [make.id] });
  const left = await local.rejected({ user: ALICE, replica: A });
  assert.deepEqual(left.map((r) => r.opId), [edit.id]);
  assert.equal(viewRow({ model: 'Issue', row: room, confirmed: null, rejected: left, kinds: KINDS }), null);
});

test('malformed input is refused before anything is written', async () => {
  const local = await store();
  const bad = { ...makeIssue(), body: { set: {}, add: {} } };
  await assert.rejects(
    local.applyOps({ user: ALICE, replica: A, ops: [{ seq: seq(1), op: bad }] }),
    ApplyError,
  );
  await assert.rejects(
    local.applyOps({ user: ALICE, replica: A, ops: [{ seq: '1', op: makeIssue() }] }),
    ApplyError,
  );
  await assert.rejects(
    local.applyAck({ user: ALICE, replica: A, seq: seq(1), entries: [entryOf(makeIssue(), ALICE)] }),
    /not an op of replica/,
  );
  assert.throws(
    () => applyOp(applyOp(null, makeIssue(), KINDS), wireOp(B, 2, 1, { rename: {} }), KINDS),
    ApplyError,
  );
  assert.equal(await local.appliedServerSeq({ user: ALICE, replica: A }), null);
});

// The session with a data store: own ops are applied before they leave the outbox, and
// `Hello` names the mark of the data store.
test('the session applies an acked op to the data store before it leaves the outbox', async () => {
  const indexedDB = new MemoryIndexedDB();
  const outbox = await openOutbox({ indexedDB, IDBKeyRange: MemoryKeyRange });
  const local = await store(indexedDB);
  const row = '0000000003e80000' + A;
  await local.applyOps({ user: ALICE, replica: A, ops: [{ seq: seq(3), op: makeIssue() }] });
  const entry = await outbox.append({
    user: ALICE,
    replica: A,
    op: { model: 'Issue', row, body: { make: { title: 'mine', labels: ['x'], votes: [] } } },
    nowMs: 1000,
  });
  const order = [];
  const watched = {
    appliedServerSeq: (args) => local.appliedServerSeq(args),
    applyAck: async (args) => {
      order.push(['apply', (await outbox.pending({ user: ALICE, replica: A })).length]);
      return local.applyAck(args);
    },
    reject: (args) => local.reject(args),
  };
  const socket = new FakeSocket();
  const identity = memoryIdentity({ replica: A });
  const controller = new AbortController();
  const done = runSession({
    socket,
    outbox,
    identity,
    models: ['Issue'],
    queue: new EventQueue(),
    local: watched,
    signal: controller.signal,
    hooks: { onAck: async () => order.push(['hook']) },
    now: () => 1000,
  });
  socket.open();
  await until(() => socket.frames.length === 1, 'hello');
  assert.equal(socket.sent()[0].last_server_seq, seq(3));
  socket.deliver(welcome(A));
  await until(() => socket.frames.length === 2, 'ops frame');
  socket.deliver({ t: 'ack', op: entry.opId, seq: seq(5), restamped: [] });
  await until(() => order.length === 2, 'ack applied');
  assert.deepEqual(order, [['apply', 1], ['hook']]);
  assert.deepEqual(await outbox.pending({ user: ALICE, replica: A }), []);
  assert.equal((await local.row({ user: ALICE, replica: A, model: 'Issue', row })).row.fields.title, 'mine');
  assert.equal(await local.appliedServerSeq({ user: ALICE, replica: A }), seq(5));
  controller.abort();
  await done;
});
