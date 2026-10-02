import assert from 'node:assert/strict';
import { test } from 'node:test';

import { compareCodePoints, encode, InvalidValue } from './canon.mjs';

test('rejects numbers without a canonical form', () => {
  for (const number of [NaN, Infinity, -Infinity]) {
    assert.throws(() => encode(number), InvalidValue);
  }
});

test('rejects values that are not JSON', () => {
  for (const value of [undefined, () => 1, 1n, Symbol('s'), new Date(0), new Map()]) {
    assert.throws(() => encode(value), InvalidValue);
  }
});

test('rejects an unpaired surrogate inside arrays and object keys', () => {
  assert.throws(() => encode(['ok', '\ud800']), InvalidValue);
  assert.throws(() => encode({ '\udc00': 1 }), InvalidValue);
});

test('accepts objects without a prototype', () => {
  const object = Object.create(null);
  object.b = 1;
  object.a = 2;
  assert.equal(encode(object), '{"a":2,"b":1}');
});

test('compareCodePoints orders astral characters after the BMP', () => {
  // UTF-16 code unit order would put U+1F600 (0xD83D...) before U+FF01.
  assert.ok(compareCodePoints('😀', '！') > 0);
  assert.ok(compareCodePoints('a', 'ab') < 0);
  assert.equal(compareCodePoints('same', 'same'), 0);
});
