// Tests for one sync session: Hello and Welcome, draining the outbox, outcomes and the end of
// a session. The outbox is the real module on the in-memory IndexedDB stand in.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { openOutbox } from '../outbox/outbox.mjs';
import { MemoryIndexedDB, MemoryKeyRange } from '../outbox/testing/memory_indexeddb.mjs';
import { EventQueue } from './events.mjs';
import { encodeClient } from './frame.mjs';
import { END, runSession } from './session.mjs';
import { FakeSocket, memoryIdentity, until, welcome } from './testing/fake_server.mjs';

const ALICE = 'user-alice';
const BOB = 'user-bob';
const REPLICA = '00000000000000a1';
const OTHER = '00000000000000b2';
const ROW = '0000000003e80000' + REPLICA;
const SEQ = '0000000000000010';

function op(n) {
  return { model: 'Issue', row: ROW, body: { set: { field: 'title', value: `v${n}` } } };
}

async function setup({ identity = { replica: REPLICA }, ops = 0, models = ['Issue'] } = {}) {
  const outbox = await openOutbox({ indexedDB: new MemoryIndexedDB(), IDBKeyRange: MemoryKeyRange });
  const entries = [];
  for (let n = 1; n <= ops; n += 1) {
    entries.push(await outbox.append({ user: ALICE, replica: REPLICA, op: op(n), nowMs: 1000 }));
  }
  const socket = new FakeSocket();
  const store = memoryIdentity(identity);
  const queue = new EventQueue();
  const controller = new AbortController();
  const calls = [];
  const hook = (name) => async (payload) => calls.push({ name, payload });
  const done = runSession({
    socket,
    outbox,
    identity: store,
    models,
    queue,
    signal: controller.signal,
    hooks: {
      onWelcome: hook('welcome'),
      onAck: hook('ack'),
      onReject: hook('reject'),
      onNotice: hook('notice'),
    },
    now: () => 1000,
  });
  return { outbox, entries, socket, store, queue, controller, calls, done };
}

// Opens the socket and answers the hello with a welcome.
async function connect(s, options) {
  s.socket.open();
  await until(() => s.socket.frames.length === 1, 'hello');
  s.socket.deliver(welcome(s.store.state.replica ?? REPLICA, options));
}

const pending = (s) => s.outbox.pending({ user: ALICE, replica: REPLICA });
const ack = (opId, restamped = []) => ({ t: 'ack', op: opId, seq: SEQ, restamped });

test('sends hello with the stored identity and reports the welcome', async () => {
  const s = await setup({
    identity: { replica: REPLICA, token: 'tok', lastServerSeq: SEQ },
  });
  s.socket.open();
  await until(() => s.socket.frames.length === 1, 'hello');
  assert.deepEqual(s.socket.sent()[0], {
    t: 'hello',
    v: 0,
    token: 'tok',
    replica: REPLICA,
    replica_secret: null,
    last_server_seq: SEQ,
  });
  s.socket.deliver(welcome(REPLICA, { serverMs: 4000 }));
  await until(() => s.calls.length === 1, 'welcome hook');
  assert.equal(s.calls[0].name, 'welcome');
  assert.equal(s.calls[0].payload.clockOffsetMs, 3000);
  s.controller.abort();
  assert.equal((await s.done).end, END.ABORTED);
  assert.equal(s.socket.closed, true);
});

test('stores a newly issued replica and its secret', async () => {
  const s = await setup({ identity: { replica: null } });
  s.socket.open();
  await until(() => s.socket.frames.length === 1, 'hello');
  assert.equal(s.socket.sent()[0].replica, null);
  const secret = 'ab'.repeat(16);
  s.socket.deliver(welcome(REPLICA, { secret }));
  await until(() => s.store.state.replica === REPLICA, 'saved replica');
  assert.equal(s.store.state.replicaSecret, secret);
  s.controller.abort();
  await s.done;
});

test('discards outbox entries of another user or replica with a notice (AC-43)', async () => {
  const s = await setup({ ops: 1 });
  await s.outbox.append({ user: BOB, replica: REPLICA, op: op(9), nowMs: 1000 });
  await s.outbox.append({ user: ALICE, replica: OTHER, op: op(9), nowMs: 1000 });
  await connect(s);
  await until(() => s.socket.frames.length === 2, 'ops frame');
  assert.deepEqual(
    s.calls.find((c) => c.name === 'notice').payload,
    { kind: 'foreign-ops-discarded', count: 2 },
  );
  assert.deepEqual(s.socket.sent()[1].ops.map((o) => o.id), [s.entries[0].opId]);
  s.controller.abort();
  await s.done;
});

test('drains the outbox and removes ops confirmed by a cumulative ack', async () => {
  const s = await setup({ ops: 3 });
  await connect(s);
  await until(() => s.socket.frames.length === 2, 'ops frame');
  const sent = s.socket.sent()[1];
  assert.equal(sent.t, 'ops');
  assert.deepEqual(sent.ops[0], {
    id: s.entries[0].opId,
    hlc: s.entries[0].hlc,
    model: 'Issue',
    row: ROW,
    body: { set: { field: 'title', value: 'v1' } },
  });
  assert.equal(sent.ops.length, 3);

  const later = '0000000007d00000' + REPLICA;
  s.socket.deliver(ack(s.entries[2].opId, [[s.entries[1].opId, later]]));
  await until(() => s.calls.some((c) => c.name === 'ack'), 'ack hook');
  assert.deepEqual(await pending(s), []);
  const outcome = s.calls.find((c) => c.name === 'ack').payload;
  assert.deepEqual(outcome.confirmed, s.entries.map((e) => e.opId));
  // The re-stamped HLC moved the clock: the next op is stamped after it.
  const next = await s.outbox.append({ user: ALICE, replica: REPLICA, op: op(4), nowMs: 1000 });
  assert.ok(next.hlc > later, `${next.hlc} after ${later}`);
  s.controller.abort();
  await s.done;
});

test('a reject decides its op; the ack confirms only the others', async () => {
  const s = await setup({ ops: 3 });
  await connect(s);
  await until(() => s.socket.frames.length === 2, 'ops frame');
  s.socket.deliver({ t: 'reject', op: s.entries[1].opId, reason: 'Denied' });
  s.socket.deliver(ack(s.entries[2].opId));
  await until(() => s.calls.some((c) => c.name === 'ack'), 'ack hook');
  assert.deepEqual(s.calls.find((c) => c.name === 'reject').payload, {
    op: s.entries[1].opId,
    reason: 'Denied',
  });
  assert.deepEqual(s.calls.find((c) => c.name === 'ack').payload.confirmed, [
    s.entries[0].opId,
    s.entries[2].opId,
  ]);
  assert.deepEqual(await pending(s), []);
  // An ack for an op that no longer waits changes nothing.
  s.socket.deliver(ack(s.entries[0].opId));
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(s.calls.filter((c) => c.name === 'reject').length, 1);
  assert.equal(s.calls.filter((c) => c.name === 'ack').length, 1);
  s.controller.abort();
  assert.equal((await s.done).end, END.ABORTED);
});

test('a reject for an op an earlier ack confirmed calls onReject and ends with a fault (D182)', async () => {
  const s = await setup({ ops: 3 });
  await connect(s);
  await until(() => s.socket.frames.length === 2, 'ops frame');
  s.socket.deliver({ t: 'reject', op: s.entries[1].opId, reason: 'Denied' });
  s.socket.deliver(ack(s.entries[2].opId));
  await until(() => s.calls.some((c) => c.name === 'ack'), 'ack hook');
  // The batch is decided; a late reject of a confirmed op is never ignored.
  s.socket.deliver({ t: 'reject', op: s.entries[0].opId, reason: 'Denied' });
  const result = await s.done;
  assert.equal(result.end, END.FAULT);
  assert.equal(result.error.kind, 'Protocol');
  assert.equal(result.welcomed, true);
  assert.equal(s.socket.closed, true);
  assert.deepEqual(
    s.calls.filter((c) => c.name === 'reject').map((c) => c.payload),
    [
      { op: s.entries[1].opId, reason: 'Denied' },
      { op: s.entries[0].opId, reason: 'Denied' },
    ],
  );
});

test('an ack before the reject of an op it covers calls onReject and ends with a fault (D182)', async () => {
  const s = await setup({ ops: 3 });
  await connect(s);
  await until(() => s.socket.frames.length === 2, 'ops frame');
  // Wrong order within one batch: the cumulative ack covers op 2, then op 2 is rejected.
  s.socket.deliver(ack(s.entries[2].opId));
  s.socket.deliver({ t: 'reject', op: s.entries[1].opId, reason: 'Denied' });
  const result = await s.done;
  assert.equal(result.end, END.FAULT);
  assert.equal(result.error.kind, 'Protocol');
  assert.equal(s.socket.closed, true);
  assert.deepEqual(s.calls.find((c) => c.name === 'ack').payload.confirmed, [
    s.entries[0].opId,
    s.entries[1].opId,
    s.entries[2].opId,
  ]);
  assert.deepEqual(
    s.calls.filter((c) => c.name === 'reject').map((c) => c.payload),
    [{ op: s.entries[1].opId, reason: 'Denied' }],
  );
  // Nothing more is sent on the closed session.
  assert.equal(s.socket.frames.length, 2);
});

test('outcomes in the order of D182, reject then defer then ack, keep the session open', async () => {
  const s = await setup({ ops: 4 });
  await connect(s);
  await until(() => s.socket.frames.length === 2, 'ops frame');
  s.socket.deliver({ t: 'reject', op: s.entries[1].opId, reason: 'Denied' });
  s.socket.deliver({ t: 'defer', op: s.entries[3].opId, retry_after_ms: 40 });
  s.socket.deliver(ack(s.entries[2].opId));
  await until(() => s.calls.some((c) => c.name === 'ack'), 'ack hook');
  assert.deepEqual(s.calls.find((c) => c.name === 'ack').payload.confirmed, [
    s.entries[0].opId,
    s.entries[2].opId,
  ]);
  assert.deepEqual((await pending(s)).map((e) => e.opId), [s.entries[3].opId]);
  await until(() => s.socket.frames.length === 3, 'resend after the delay');
  assert.deepEqual(s.socket.sent()[2].ops.map((o) => o.id), [s.entries[3].opId]);
  s.controller.abort();
  assert.equal((await s.done).end, END.ABORTED);
});

test('one batch at a time, at most ops_per_batch ops', async () => {
  const s = await setup({ ops: 5 });
  await connect(s, { limitOverrides: { ops_per_batch: 2 } });
  await until(() => s.socket.frames.length === 2, 'first batch');
  assert.deepEqual(s.socket.sent()[1].ops.map((o) => o.id), [s.entries[0].opId, s.entries[1].opId]);
  // Half of the batch is decided: nothing new is sent.
  s.socket.deliver(ack(s.entries[0].opId));
  await until(() => s.calls.some((c) => c.name === 'ack'), 'first ack');
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(s.socket.frames.length, 2);
  s.socket.deliver(ack(s.entries[1].opId));
  await until(() => s.socket.frames.length === 3, 'second batch');
  assert.deepEqual(s.socket.sent()[2].ops.map((o) => o.id), [s.entries[2].opId, s.entries[3].opId]);
  s.controller.abort();
  await s.done;
});

test('a defer keeps its op and the later ones and resends them after the delay (D108)', async () => {
  const s = await setup({ ops: 3 });
  await connect(s);
  await until(() => s.socket.frames.length === 2, 'ops frame');
  s.socket.deliver({ t: 'defer', op: s.entries[1].opId, retry_after_ms: 40 });
  s.socket.deliver(ack(s.entries[0].opId));
  await until(() => s.calls.some((c) => c.name === 'ack'), 'ack');
  assert.equal(s.socket.frames.length, 2, 'nothing is sent during the delay');
  assert.deepEqual((await pending(s)).map((e) => e.opId), [s.entries[1].opId, s.entries[2].opId]);
  await until(() => s.socket.frames.length === 3, 'resend after the delay');
  assert.deepEqual(s.socket.sent()[2].ops.map((o) => o.id), [s.entries[1].opId, s.entries[2].opId]);
  s.controller.abort();
  await s.done;
});

test('a wake after a local write sends the new op', async () => {
  const s = await setup();
  await connect(s);
  await until(() => s.calls.some((c) => c.name === 'welcome'), 'welcome');
  assert.equal(s.socket.frames.length, 1);
  const entry = await s.outbox.append({ user: ALICE, replica: REPLICA, op: op(1), nowMs: 1000 });
  s.queue.push({ type: 'wake' });
  await until(() => s.socket.frames.length === 2, 'ops frame');
  assert.deepEqual(s.socket.sent()[1].ops.map((o) => o.id), [entry.opId]);
  s.controller.abort();
  await s.done;
});

test('a fatal error ends the session with its code; one with a subscription is dropped', async () => {
  const s = await setup();
  await connect(s);
  s.socket.deliver({ t: 'error', sub: 4, code: 'RateLimited', retry_after_ms: 100 });
  s.socket.deliver({ t: 'evict', sub: 4, model: 'Issue', rows: [ROW] });
  s.socket.deliver({ t: 'reset', sub: 4 });
  s.socket.deliver({ t: 'error', sub: null, code: 'Overloaded', retry_after_ms: null });
  const result = await s.done;
  assert.deepEqual(result, { end: END.FATAL, code: 'Overloaded', welcomed: true });
  assert.equal(s.socket.closed, true);
});

test('an error frame without retry_after_ms is a fault, not an error of the server (dana #2591)', async () => {
  const s = await setup();
  await connect(s);
  s.socket.deliver('{"code":"Overloaded","sub":null,"t":"error","v":0}');
  const result = await s.done;
  assert.equal(result.end, END.FAULT);
  assert.equal(result.error.kind, 'MissingKey');
  assert.equal(result.error.key, 'retry_after_ms');
});

test('the server refusing the hello ends the session before a welcome', async () => {
  const s = await setup();
  s.socket.open();
  await until(() => s.socket.frames.length === 1, 'hello');
  s.socket.deliver({ t: 'error', sub: null, code: 'Unauthorized', retry_after_ms: null });
  assert.deepEqual(await s.done, { end: END.FATAL, code: 'Unauthorized', welcomed: false });
});

test('a message before the welcome, or a welcome for another replica, is a fault', async () => {
  const early = await setup({ ops: 1 });
  early.socket.open();
  await until(() => early.socket.frames.length === 1, 'hello');
  early.socket.deliver(ack(early.entries[0].opId));
  assert.equal((await early.done).end, END.FAULT);
  assert.equal((await pending(early)).length, 1, 'nothing confirmed');

  const other = await setup();
  other.socket.open();
  await until(() => other.socket.frames.length === 1, 'hello');
  other.socket.deliver(welcome(OTHER));
  const result = await other.done;
  assert.equal(result.end, END.FAULT);
  assert.equal(result.welcomed, false);
});

test('a second welcome and an unsupported message are faults', async () => {
  const s = await setup();
  await connect(s);
  s.socket.deliver(welcome(REPLICA));
  assert.equal((await s.done).end, END.FAULT);

  const t = await setup();
  await connect(t);
  t.socket.deliver('{"ops":[],"t":"ops","v":0}');
  const result = await t.done;
  assert.equal(result.end, END.FAULT);
  assert.equal(result.error.kind, 'Unsupported');
});

test('a closed socket ends the session; ops stay in the outbox', async () => {
  const s = await setup({ ops: 2 });
  await connect(s);
  await until(() => s.socket.frames.length === 2, 'ops frame');
  s.socket.drop();
  assert.deepEqual(await s.done, { end: END.CLOSED, welcomed: true });
  assert.equal((await pending(s)).length, 2);
});

test('an outbox head over the op limit stops the session instead of skipping a seq', async () => {
  const s = await setup({ ops: 2 });
  await connect(s, { limitOverrides: { op_bytes: 100, frame_bytes: 1 << 20 } });
  const result = await s.done;
  assert.equal(result.end, END.STUCK);
  assert.equal(result.error.kind, 'OpTooLarge');
  assert.equal((await pending(s)).length, 2);
});

test('ops are packed into frames that fit the frame limit', async () => {
  const s = await setup({ ops: 3 });
  const wire = (e) => ({ id: e.opId, hlc: e.hlc, ...e.op });
  // Room for exactly two ops, one byte short of three.
  const two = encodeClient({ t: 'ops', ops: s.entries.slice(0, 2).map(wire) }).length;
  const three = encodeClient({ t: 'ops', ops: s.entries.map(wire) }).length;
  await connect(s, { limitOverrides: { frame_bytes: three - 1, op_bytes: 1000 } });
  await until(() => s.socket.frames.length === 2, 'ops frame');
  const frame = s.socket.frames[1];
  assert.equal(frame.length, two);
  assert.equal(JSON.parse(frame).ops.length, 2);
  // Exactly at the limit, all three fit.
  const t = await setup({ ops: 3 });
  await connect(t, { limitOverrides: { frame_bytes: three, op_bytes: 1000 } });
  await until(() => t.socket.frames.length === 2, 'ops frame');
  assert.equal(JSON.parse(t.socket.frames[1]).ops.length, 3);
  t.controller.abort();
  await t.done;
  s.controller.abort();
  await s.done;
});

test('a reject for an op of an earlier batch ends with a fault; the session keeps one number (D182, dana #2735)', async () => {
  const s = await setup({ ops: 3 });
  await connect(s, { limitOverrides: { ops_per_batch: 1 } });
  await until(() => s.socket.frames.length === 2, 'first batch');
  s.socket.deliver({ t: 'ack', op: s.entries[0].opId, seq: '0000000000000010', restamped: [] });
  await until(() => s.socket.frames.length === 3, 'second batch');
  // The first batch is decided and the second is on the wire: a reject naming the confirmed op
  // of the first batch is never ignored.
  s.socket.deliver({ t: 'reject', op: s.entries[0].opId, reason: 'Denied' });
  const result = await s.done;
  assert.equal(result.end, END.FAULT);
  assert.equal(result.error.kind, 'Protocol');
  assert.equal(s.socket.closed, true);
  assert.deepEqual(
    s.calls.filter((c) => c.name === 'reject').map((c) => c.payload),
    [{ op: s.entries[0].opId, reason: 'Denied' }],
  );
  // The op of the batch on the wire stays in the outbox for the next session.
  assert.deepEqual((await pending(s)).map((e) => e.opId), [s.entries[1].opId, s.entries[2].opId]);
  assert.equal(s.socket.frames.length, 3);
});

test('a second reject of a rejected op is a fault; one above every confirmed op is ignored', async () => {
  const s = await setup({ ops: 3 });
  await connect(s);
  await until(() => s.socket.frames.length === 2, 'ops frame');
  s.socket.deliver({ t: 'reject', op: s.entries[2].opId, reason: 'Denied' });
  s.socket.deliver(ack(s.entries[1].opId));
  await until(() => s.calls.some((c) => c.name === 'ack'), 'ack hook');
  // Op 3 lies above the highest confirmed op: its repeated reject concerns no confirmed op.
  s.socket.deliver({ t: 'reject', op: s.entries[2].opId, reason: 'Denied' });
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(s.calls.filter((c) => c.name === 'reject').length, 1);
  // A reject for an op of another replica concerns no own op.
  s.socket.deliver({ t: 'reject', op: OTHER + '00000001', reason: 'Denied' });
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(s.calls.filter((c) => c.name === 'reject').length, 1);

  const t = await setup({ ops: 3 });
  await connect(t);
  await until(() => t.socket.frames.length === 2, 'ops frame');
  t.socket.deliver({ t: 'reject', op: t.entries[0].opId, reason: 'Denied' });
  t.socket.deliver(ack(t.entries[2].opId));
  await until(() => t.calls.some((c) => c.name === 'ack'), 'ack hook');
  // Op 1 lies below the confirmed op 3: it is decided, a second reject breaks the protocol.
  t.socket.deliver({ t: 'reject', op: t.entries[0].opId, reason: 'Denied' });
  assert.equal((await t.done).end, END.FAULT);
  assert.equal(t.calls.filter((c) => c.name === 'reject').length, 2);
  s.controller.abort();
  assert.equal((await s.done).end, END.ABORTED);
});
