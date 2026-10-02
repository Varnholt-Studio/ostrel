import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialRank, isRank, rankBetween } from '../src/rank.mjs';
import { rng } from '../src/workload.mjs';

test('initial ranks are strictly increasing', () => {
  for (let i = 0; i < 10000; i++) assert.ok(initialRank(i) < initialRank(i + 1));
});

test('rankBetween returns a key strictly between its bounds', () => {
  const r = rng(3);
  const keys = [initialRank(0), initialRank(1)];
  for (let i = 0; i < 3000; i++) {
    keys.sort();
    const at = r.int(keys.length + 1);
    const lo = at === 0 ? '' : keys[at - 1];
    const hi = at === keys.length ? null : keys[at];
    if (lo === hi) continue;
    const k = rankBetween(lo, hi);
    assert.ok(k > lo, `${k} > ${lo}`);
    if (hi !== null) assert.ok(k < hi, `${k} < ${hi}`);
    assert.notEqual(k.at(-1), '0');
    keys.push(k);
  }
});

test('adjacent and equal bounds still give a key above the lower bound', () => {
  assert.ok(rankBetween('a', 'a1') > 'a' && rankBetween('a', 'a1') < 'a1');
  assert.ok(rankBetween('abc', 'abc') > 'abc');
});

test('keys ending in 0 are invalid: no key lies directly below them', () => {
  assert.equal(isRank('a0'), false);
  assert.equal(isRank(''), false);
  assert.equal(isRank('a1'), true);
  assert.throws(() => rankBetween('a', 'a0'), RangeError);
  assert.throws(() => rankBetween('a0', null), RangeError);
});
