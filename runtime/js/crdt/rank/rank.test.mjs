// Merge behaviour and fixed keys are covered by the shared vectors in tests/crdt-vectors/rank/.
// These tests cover the properties of keyBetween over many keys, and hostile input.

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { compareText } from '../canon/canon.mjs';
import {
  createReplica,
  DIGITS,
  InvalidRank,
  isValidRank,
  keyBetween,
  MAX_RANK_DIGITS,
  RankLimit,
} from './rank.mjs';

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

test('appending at either end grows a key with the logarithm of the number of rows', () => {
  let last = keyBetween(null, null);
  let first = last;
  for (let step = 0; step < 10000; step += 1) {
    const nextLast = keyBetween(last, null);
    const nextFirst = keyBetween(null, first);
    assert.equal(compareText(last, nextLast), -1, `${last} < ${nextLast}`);
    assert.equal(compareText(nextFirst, first), -1, `${nextFirst} < ${first}`);
    last = nextLast;
    first = nextFirst;
  }
  // Level 0 holds 36 keys after the start, level 1 about 62^2, level 2 about 62^3: after 10 000
  // appends the key is at level 2, which is two leading digits and a counter of three.
  assert.ok(last.length <= 5, `append at the end grew to ${last.length} digits: ${last}`);
  assert.ok(first.length <= 5, `append at the start grew to ${first.length} digits: ${first}`);
});

test('long keys never deepen the call stack', () => {
  const longest = 'V' + 'a'.repeat(MAX_RANK_DIGITS - 1);
  assert.equal(keyBetween(longest, null), 'W');
  assert.equal(keyBetween(null, longest), 'U');
  const low = 'V' + 'z'.repeat(MAX_RANK_DIGITS - 2);
  assert.equal(keyBetween(low, 'W'), low + 'V');
  const shared = 'a'.repeat(MAX_RANK_DIGITS - 1);
  assert.equal(keyBetween(shared + '1', shared + '3'), shared + '2');
  const zeros = '0'.repeat(MAX_RANK_DIGITS - 3);
  assert.equal(keyBetween('V', `V${zeros}1`), `V${zeros}0V`);
});

test('a key longer than the limit is invalid, and so is a new key that would exceed it', () => {
  assert.ok(isValidRank('a'.repeat(MAX_RANK_DIGITS)));
  assert.ok(!isValidRank('a'.repeat(MAX_RANK_DIGITS + 1)));
  assert.throws(() => keyBetween('z'.repeat(7501), null), InvalidRank);
  assert.throws(() => keyBetween(null, 'a'.repeat(5 * 1024 * 1024)), InvalidRank);

  // Bounds of the full length that differ only in their last digits leave no room below the limit.
  const full = 'a'.repeat(MAX_RANK_DIGITS - 1);
  assert.throws(() => keyBetween(full + '1', full + '2'), RankLimit);
  assert.throws(() => keyBetween('z'.repeat(MAX_RANK_DIGITS), null), RankLimit);

  // Splitting one gap again and again reaches the limit as RankLimit, never as a RangeError.
  let low = 'V';
  const high = 'W';
  let splits = 0;
  assert.throws(() => {
    for (;;) {
      low = keyBetween(low, high);
      splits += 1;
    }
  }, RankLimit);
  assert.ok(splits > 5000, `only ${splits} splits of one gap fit`);
});

test('a replica refuses a key longer than the limit from a peer', () => {
  const replica = createReplica();
  const op = { id: ID, hlc: HLC, op: { set: [ROW, 'a'.repeat(5 * 1024 * 1024)] } };
  assert.throws(() => replica.apply(op), InvalidRank);
  assert.deepEqual(replica.value(), []);
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
