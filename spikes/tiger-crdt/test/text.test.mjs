import { test } from 'node:test';
import assert from 'node:assert/strict';
import { compareText } from '../src/text.mjs';
import { rng } from '../src/workload.mjs';

function byCodePoint(a, b) {
  const x = Array.from(a, (c) => c.codePointAt(0));
  const y = Array.from(b, (c) => c.codePointAt(0));
  for (let i = 0; i < Math.min(x.length, y.length); i++) if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
  return x.length === y.length ? 0 : x.length < y.length ? -1 : 1;
}

test('code point order differs from UTF-16 order where D50 says it must', () => {
  assert.equal(compareText('！', '\u{1f600}'), -1);
  assert.equal('！' < '\u{1f600}', false);
  assert.equal(compareText('a', 'ab'), -1);
  assert.equal(compareText('b', 'ab'), 1);
  assert.equal(compareText('same', 'same'), 0);
});

test('agrees with a reference code point comparison on random strings', () => {
  const r = rng(7);
  const pool = ['a', 'z', 'é', '퟿', '', '￿', '\u{10000}', '\u{1f600}', '\u{10ffff}'];
  for (let i = 0; i < 5000; i++) {
    const s = () => Array.from({ length: r.int(4) }, () => r.pick(pool)).join('');
    const a = s();
    const b = s();
    assert.equal(compareText(a, b), byCodePoint(a, b), JSON.stringify([a, b]));
  }
});
