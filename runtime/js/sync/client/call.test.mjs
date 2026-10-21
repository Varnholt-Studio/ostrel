// Tests for server function calls of one session (SPEC 12.7, D81, D155). Time is a
// controllable clock, never a wall clock wait (D74).
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { CallTable, DEFAULT_CALL_TIMEOUT_MS, offlineCall } from './call.mjs';
import { WireError, decodeServer } from './frame.mjs';
import { ROOMS_SCHEMA } from './testing/fake_server.mjs';
import { compileSchema } from './value.mjs';

const schema = compileSchema(ROOMS_SCHEMA);
const ROW = '0000000003e80000' + '0123456789abcdef';

// A manual clock: `advance(ms)` runs every timer that is due.
function manualClock() {
  let now = 0;
  const timers = new Map();
  let nextTimer = 1;
  return {
    setTimer(fn, ms) {
      const id = nextTimer++;
      timers.set(id, { at: now + ms, fn });
      return id;
    },
    clearTimer(id) {
      timers.delete(id);
    },
    advance(ms) {
      now += ms;
      for (const [id, timer] of [...timers]) {
        if (timer.at <= now) {
          timers.delete(id);
          timer.fn();
        }
      }
    },
    pending: () => timers.size,
  };
}

function setup({ limits = {}, send } = {}) {
  const clock = manualClock();
  const frames = [];
  const table = new CallTable({
    schema,
    send: send ?? ((frame) => frames.push(JSON.parse(frame))),
    limits,
    setTimer: clock.setTimer,
    clearTimer: clock.clearTimer,
  });
  // Decodes a server frame against the table, as the session does.
  const deliver = (frame) => {
    let msg;
    try {
      msg = decodeServer(frame, {
        schema,
        requests: { subscriptionModel: () => null, callFunction: (id) => table.functionOf(id) },
      });
    } catch (error) {
      if (error instanceof WireError && !error.closesConnection) return 'dropped';
      throw error;
    }
    table.receive(msg);
    return 'settled';
  };
  return { table, frames, clock, deliver };
}

const returnFrame = (call, result) => `{"call":${call},"result":${result},"t":"return","v":0}`;

test('a call sends one call frame and settles with the typed result of its return', async () => {
  const s = setup();
  const result = s.table.call('rename', [ROW, 'lobby']);
  assert.deepEqual(s.frames, [{ v: 0, t: 'call', call: 0, function: 'rename', args: [ROW, 'lobby'] }]);
  assert.equal(s.table.functionOf(0), 'rename');
  assert.equal(s.deliver(returnFrame(0, '{"ok":true}')), 'settled');
  assert.deepEqual(await result, { ok: true, value: true });
  assert.equal(s.table.functionOf(0), null);
  assert.equal(s.clock.pending(), 0, 'the timeout is cleared');
});

test('a Unit function settles with null (D193); errors keep their kind', async () => {
  const s = setup();
  const touch = s.table.call('touch', []);
  const denied = s.table.call('count', []);
  const runtime = s.table.call('count', []);
  s.deliver(returnFrame(0, '{"ok":null}'));
  s.deliver(returnFrame(1, '{"err":"Denied"}'));
  s.deliver(returnFrame(2, '{"runtime":"StepLimit"}'));
  assert.deepEqual(await touch, { ok: true, value: null });
  assert.deepEqual(await denied, { ok: false, kind: 'Denied' });
  assert.deepEqual(await runtime, { ok: false, kind: 'StepLimit' });
});

test('call ids rise per session and are never used again (D155)', async () => {
  const s = setup();
  const first = s.table.call('count', []);
  s.deliver(returnFrame(0, '{"ok":1}'));
  await first;
  s.table.call('count', []);
  s.table.call('count', []);
  assert.deepEqual(s.frames.map((f) => f.call), [0, 1, 2]);
  // A second return for the settled call 0 is dropped, not read against call 1 or 2.
  assert.equal(s.deliver(returnFrame(0, '{"ok":2}')), 'dropped');
});

test('a call without a return gets OutcomeUnknown after call_timeout_ms; a late return is dropped', async () => {
  const s = setup({ limits: { call_timeout_ms: 500 } });
  const result = s.table.call('count', []);
  s.clock.advance(499);
  s.clock.advance(1);
  assert.deepEqual(await result, { ok: false, kind: 'OutcomeUnknown' });
  // Sent exactly once; the client never sends it again (D81).
  assert.equal(s.frames.length, 1);
  assert.equal(s.deliver(returnFrame(0, '{"ok":1}')), 'dropped');
  assert.equal(DEFAULT_CALL_TIMEOUT_MS, 30000);
});

test('the end of the session settles every waiting call with OutcomeUnknown', async () => {
  const s = setup();
  const a = s.table.call('count', []);
  const b = s.table.call('names', []);
  s.table.close();
  assert.deepEqual(await a, { ok: false, kind: 'OutcomeUnknown' });
  assert.deepEqual(await b, { ok: false, kind: 'OutcomeUnknown' });
  assert.equal(s.clock.pending(), 0);
  // After the end nothing is sent: a new call is Offline.
  assert.deepEqual(await s.table.call('count', []), { ok: false, kind: 'Offline' });
  assert.equal(s.frames.length, 2);
});

test('a frame that never reached the socket is Offline, never OutcomeUnknown', async () => {
  const s = setup({
    send: () => {
      throw new Error('send on a closed socket');
    },
  });
  assert.deepEqual(await s.table.call('count', []), { ok: false, kind: 'Offline' });
  assert.equal(s.clock.pending(), 0);
  assert.deepEqual(await offlineCall(), { ok: false, kind: 'Offline' });
});

test('arguments over the frame limit are refused without sending; bad arguments are a defect', async () => {
  const s = setup({ limits: { frame_bytes: 100 } });
  assert.deepEqual(await s.table.call('rename', [ROW, 'x'.repeat(100)]), { ok: false, kind: 'Limit' });
  assert.equal(s.frames.length, 0);
  assert.throws(() => s.table.call('rename', [ROW, 3]), (e) => e instanceof WireError && e.kind === 'Value');
  assert.throws(() => s.table.call('nope', []), (e) => e.kind === 'NotInSchema');
  // The refused call used an id; the next one does not reuse it.
  s.table.call('count', []);
  assert.deepEqual(s.frames.map((f) => f.call), [1]);
});
