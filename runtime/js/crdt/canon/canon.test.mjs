import assert from 'node:assert/strict';
import { test } from 'node:test';

import { compareCodePoints, compareKey, compareText, encode, InvalidValue } from './canon.mjs';

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

test('compareText orders astral characters after the BMP', () => {
  // UTF-16 code unit order would put U+1F600 (0xD83D...) before U+FF01.
  assert.equal(compareText('😀', '！'), 1);
  assert.equal(compareText('a', 'ab'), -1);
  assert.equal(compareText('b', 'ab'), 1);
  assert.equal(compareText('same', 'same'), 0);
});

test('compareCodePoints stays exported as an alias of compareText', () => {
  // runtime/js/view/list/sorted_index.js still imports the old name.
  assert.equal(compareCodePoints, compareText);
});

test('compareKey orders booleans, then numbers, then strings (D61)', () => {
  const keys = ['a', 10, true, '9', 9, false, -1.5];
  assert.deepEqual(keys.sort(compareKey), [false, true, -1.5, 9, 10, '9', 'a']);
});

test('compareKey compares numbers by value, not by their encoding', () => {
  assert.equal(compareKey(9, 10), -1);
  assert.equal(compareKey(0, -0), 0);
  assert.equal(compareKey(2.5, 2), 1);
});

test('compareKey compares strings by code point, not by their escaped encoding', () => {
  // Canonical encoding escapes these as \u0001 and \", which would sort them after "A".
  assert.equal(compareKey('\u0001', 'A'), -1);
  assert.equal(compareKey('"', '#'), -1);
  assert.equal(compareKey('\\', 'a'), -1);
  assert.equal(compareKey('😀', '！'), 1);
});

test('compareKey rejects values that cannot be keys', () => {
  for (const value of [null, undefined, [1], { a: 1 }, 1n, Symbol('s')]) {
    assert.throws(() => compareKey(value, 'a'), InvalidValue);
    assert.throws(() => compareKey('a', value), InvalidValue);
  }
  assert.throws(() => compareKey(NaN, 1), InvalidValue);
  assert.throws(() => compareKey(1, Infinity), InvalidValue);
});
