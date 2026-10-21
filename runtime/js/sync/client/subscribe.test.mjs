// Tests for the subscriptions of one session (ARCHITECTURE 5.4, D155, D159).
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { WireError, decodeServer } from './frame.mjs';
import { SubscriptionTable } from './subscribe.mjs';
import { ROOMS_SCHEMA } from './testing/fake_server.mjs';
import { compileSchema } from './value.mjs';

const schema = compileSchema(ROOMS_SCHEMA);
const REPLICA = '0123456789abcdef';
const ROW = '0000000003e80000' + REPLICA;
const SEQ = '0000000000000005';
const rooms = { model: 'Room', filter: null, order: [], limit: null, after: null };
const people = { model: 'Person', filter: null, order: [['age', 'desc']], limit: 10, after: null };

function setup(limits = {}) {
  const timers = [];
  const frames = [];
  const table = new SubscriptionTable({
    schema,
    send: (frame) => frames.push(JSON.parse(frame)),
    limits,
    setTimer: (fn, ms) => timers.push({ fn, ms }) - 1,
    clearTimer: (id) => {
      timers[id] = null;
    },
  });
  // Decodes a server frame against the table and passes it on, as the session does.
  const deliver = (frame) => {
    let msg;
    try {
      msg = decodeServer(frame, {
        schema,
        requests: { subscriptionModel: (sub) => table.modelOf(sub), callFunction: () => null },
      });
    } catch (error) {
      if (error instanceof WireError && !error.closesConnection) return null;
      throw error;
    }
    return table.receive(msg);
  };
  return { table, frames, timers, deliver };
}

const snapshot = (sub, rows = [], last = true) =>
  `{"last":${last},"rows":[${rows.join(',')}],"seq":"${SEQ}","sub":${sub},"t":"snapshot","v":0}`;
const roomRow = `{"fields":{"name":"general"},"id":"${ROW}","maps":{},"sets":{}}`;
const errorFrame = (sub, code, ms = null) =>
  `{"code":"${code}","retry_after_ms":${ms},"sub":${sub},"t":"error","v":0}`;

test('subscribe sends the query; a snapshot becomes rows of that model', () => {
  const s = setup();
  assert.equal(s.table.subscribe(rooms), 0);
  assert.deepEqual(s.frames, [{ v: 0, t: 'subscribe', sub: 0, query: rooms }]);
  assert.equal(s.table.modelOf(0), 'Room');
  assert.deepEqual(s.deliver(snapshot(0, [roomRow], false)), {
    kind: 'snapshot',
    sub: 0,
    model: 'Room',
    rows: [{ id: ROW, fields: { name: 'general' }, sets: {}, maps: {} }],
    seq: SEQ,
    last: false,
  });
});

test('subscription ids rise per session and are never used again (D155)', () => {
  const s = setup();
  const a = s.table.subscribe(rooms);
  s.table.unsubscribe(a);
  const b = s.table.subscribe(people);
  assert.deepEqual([a, b], [0, 1]);
  assert.deepEqual(s.frames.map((f) => [f.t, f.sub]), [['subscribe', 0], ['unsubscribe', 0], ['subscribe', 1]]);
  // A page of the ended subscription 0 that still stood in the stream is dropped; it is never
  // read against the model of another subscription.
  assert.equal(s.deliver(snapshot(0, [roomRow])), null);
  assert.equal(s.deliver('{"model":"Room","rows":[],"sub":0,"t":"evict","v":0}'), null);
  assert.equal(s.deliver('{"sub":0,"t":"reset","v":0}'), null);
  assert.equal(s.deliver(errorFrame(0, 'FilterNotReadable')), null);
  // Unsubscribing twice sends nothing more.
  s.table.unsubscribe(a);
  assert.equal(s.frames.length, 3);
});

test('held sends pages of at most ids_per_page ids, the last one flagged', () => {
  const s = setup({ ids_per_page: 2 });
  const sub = s.table.subscribe(rooms);
  const ids = [1, 2, 3].map((n) => `000000000${n}e80000${REPLICA}`);
  s.table.held(sub, ids);
  s.table.held(sub, []);
  assert.deepEqual(
    s.frames.slice(1).map((f) => [f.rows.length, f.last]),
    [[2, false], [1, true], [0, true]],
  );
  assert.deepEqual(s.frames.slice(1, 3).flatMap((f) => f.rows), ids);
  // Exactly a full page: no empty page after it.
  s.table.held(sub, ids.slice(0, 2));
  assert.deepEqual(s.frames.slice(4).map((f) => [f.rows.length, f.last]), [[2, true]]);
  // Not for a subscription that is not open.
  s.table.held(99, ids);
  assert.equal(s.frames.length, 5);
});

test('evict names its rows; one that names another model than its subscription is a fault', () => {
  const s = setup();
  s.table.subscribe(rooms);
  assert.deepEqual(s.deliver(`{"model":"Room","rows":["${ROW}"],"sub":0,"t":"evict","v":0}`), {
    kind: 'evict',
    sub: 0,
    model: 'Room',
    rows: [ROW],
  });
  assert.throws(
    () => s.deliver('{"model":"Person","rows":[],"sub":0,"t":"evict","v":0}'),
    (e) => e instanceof WireError && e.closesConnection,
  );
});

test('reset subscribes the same query again under a new id', () => {
  const s = setup();
  s.table.subscribe(people);
  assert.deepEqual(s.deliver('{"sub":0,"t":"reset","v":0}'), { kind: 'reset', sub: 0, next: 1 });
  assert.deepEqual(s.frames[1], { v: 0, t: 'subscribe', sub: 1, query: people });
  assert.equal(s.table.modelOf(0), null);
  assert.equal(s.table.modelOf(1), 'Person');
});

test('RateLimited subscribes again after retry_after_ms (D159); other codes end it', () => {
  const s = setup();
  s.table.subscribe(rooms);
  assert.deepEqual(s.deliver(errorFrame(0, 'RateLimited', 250)), { kind: 'retry', sub: 0, next: 1 });
  assert.equal(s.timers[0].ms, 250);
  assert.equal(s.frames.length, 1, 'nothing is sent before the delay');
  // The reserved id receives nothing until its subscribe is sent.
  assert.equal(s.table.modelOf(1), null);
  assert.equal(s.deliver(snapshot(1)), null);
  assert.equal(s.deliver('{"sub":1,"t":"reset","v":0}'), null);
  assert.equal(s.deliver('{"model":"Room","rows":[],"sub":1,"t":"evict","v":0}'), null);
  s.timers[0].fn();
  assert.deepEqual(s.frames[1], { v: 0, t: 'subscribe', sub: 1, query: rooms });
  assert.equal(s.table.modelOf(1), 'Room');

  for (const code of ['TooManySubscriptions', 'FilterTooLarge', 'FilterNotReadable', 'Protocol']) {
    const sub = s.table.subscribe(rooms);
    assert.deepEqual(s.deliver(errorFrame(sub, code)), { kind: 'failed', sub, code });
    assert.equal(s.table.modelOf(sub), null);
  }
});

test('the end of the session stops pending retries; unsubscribe of a waiting retry sends nothing', () => {
  const s = setup();
  s.table.subscribe(rooms);
  s.table.subscribe(people);
  s.deliver(errorFrame(0, 'RateLimited', 100));
  s.deliver(errorFrame(1, 'RateLimited', 100));
  s.table.unsubscribe(2);
  assert.equal(s.timers[0], null);
  s.table.close();
  assert.equal(s.timers[1], null);
  assert.equal(s.frames.length, 2);
  assert.throws(() => s.table.subscribe(rooms), /ended/);
});

test('a query that does not fit the schema is a defect of the caller', () => {
  const s = setup();
  assert.throws(() => s.table.subscribe({ ...rooms, model: 'Nope' }), (e) => e.kind === 'NotInSchema');
  assert.equal(s.frames.length, 0);
});
