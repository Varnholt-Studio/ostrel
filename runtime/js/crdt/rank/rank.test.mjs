// Merge behaviour and fixed keys are covered by the shared vectors in tests/crdt-vectors/rank/.
// These tests cover the properties of keyBetween over many keys, and hostile input.

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { compareText } from '../canon/canon.mjs';
import { createReplica, DIGITS, InvalidRank, isValidRank, keyBetween } from './rank.mjs';

const ROW = '018bcfe568000000000000000000000a';
const ID = '000000000000000a00000001';
const HLC = '018bcfe568000000000000000000000a';

// Deterministic pseudo random numbers, so a failure can be reproduced from the seed.
function random(seed) {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 2 ** 32;
  };
}

test('the digits ascend in code point order', () => {
  for (let i = 1; i < DIGITS.length; i += 1) {
    assert.equal(compareText(DIGITS[i - 1], DIGITS[i]), -1, DIGITS.slice(i - 1, i + 1));
  }
});

test('keyBetween returns a valid key strictly between its bounds', () => {
  const next = random(74);
  const keys = [keyBetween(null, null)];
  for (let step = 0; step < 2000; step += 1) {
    const at = Math.floor(next() * (keys.length + 1));
    const before = at === 0 ? null : keys[at - 1];
    const after = at === keys.length ? null : keys[at];
    const key = keyBetween(before, after);
    assert.ok(isValidRank(key), `invalid key ${key}`);
    if (before !== null) assert.equal(compareText(before, key), -1, `${before} < ${key}`);
    if (after !== null) assert.equal(compareText(key, after), -1, `${key} < ${after}`);
    keys.splice(at, 0, key);
  }
  assert.deepEqual([...keys].sort(compareText), keys);
});

test('appending at either end grows a key by about one digit every five moves', () => {
  let last = keyBetween(null, null);
  let first = last;
  for (let step = 0; step < 1000; step += 1) {
    last = keyBetween(last, null);
    first = keyBetween(null, first);
  }
  // Each append halves the gap to the end, and a digit holds log2(62), almost 6 halvings.
  // Measured: 201 digits after 1000 appends at the end. The bound documents that growth.
  assert.ok(last.length <= 210, `append at the end grew to ${last.length}`);
  assert.ok(first.length <= 210, `append at the start grew to ${first.length}`);
});

test('keyBetween refuses invalid keys and bounds in the wrong order', () => {
  for (const bad of ['', 'a0', 'a-b', 'é', 5, undefined, 'A ']) {
    assert.throws(() => keyBetween(bad, null), InvalidRank, String(bad));
    assert.throws(() => keyBetween(null, bad), InvalidRank, String(bad));
  }
  assert.throws(() => keyBetween('b', 'a'), InvalidRank);
  assert.throws(() => keyBetween('a', 'a'), InvalidRank);
});

test('a malformed op throws InvalidRank and leaves the replica unchanged', () => {
  const replica = createReplica();
  replica.apply({ id: ID, hlc: HLC, op: { set: [ROW, 'V'] } });
  const before = JSON.stringify(replica.state());

  const broken = [
    null,
    [],
    { id: ID, hlc: HLC },
    { id: 'zz', hlc: HLC, op: { set: [ROW, 'a'] } },
    { id: ID, hlc: HLC.toUpperCase(), op: { set: [ROW, 'a'] } },
    { id: ID, hlc: HLC, op: { set: [ROW] } },
    { id: ID, hlc: HLC, op: { set: [ROW, 'a0'] } },
    { id: ID, hlc: HLC, op: { set: [ROW.toUpperCase(), 'a'] } },
    { id: ID, hlc: HLC, op: { set: [ROW, 'a'], extra: 1 } },
    { id: ID, hlc: HLC, op: { remove: ROW } },
  ];
  for (const op of broken) {
    assert.throws(() => replica.apply(op), InvalidRank, JSON.stringify(op));
  }
  assert.equal(JSON.stringify(replica.state()), before);
});

test('two different keys for one row under the same hlc are refused', () => {
  const replica = createReplica();
  replica.apply({ id: ID, hlc: HLC, op: { set: [ROW, 'V'] } });
  replica.apply({ id: ID, hlc: HLC, op: { set: [ROW, 'V'] } });
  assert.throws(() => replica.apply({ id: ID, hlc: HLC, op: { set: [ROW, 'W'] } }), InvalidRank);
  assert.deepEqual(replica.value(), [ROW]);
});
