// Merge behaviour is covered by the shared vectors in tests/crdt-vectors/map/.
// These tests cover hostile input, which the vectors do not describe.

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { InvalidOp, LwwMap } from './map.mjs';

const ID = '000000000000000a00000001';
const HLC = '018bcfe568000000000000000000000a';

function put(key, value, hlc = HLC) {
  return { id: ID, hlc, op: { put: [key, value] } };
}

test('a malformed op throws InvalidOp and leaves the map unchanged', () => {
  const map = new LwwMap();
  map.apply(put('ana', 'admin'));
  const before = JSON.stringify(map.state());

  const broken = [
    null,
    [],
    { id: ID, hlc: HLC },
    { id: 'zz', hlc: HLC, op: { remove: 'ana' } },
    { id: ID, hlc: HLC.toUpperCase(), op: { remove: 'ana' } },
    { id: ID, hlc: HLC, op: {} },
    { id: ID, hlc: HLC, op: { put: ['ana'] } },
    { id: ID, hlc: HLC, op: { put: ['ana', 'x'], remove: 'ana' } },
    { id: ID, hlc: HLC, op: { clear: true } },
    { id: ID, hlc: HLC, op: { remove: { nested: 1 } } },
    { id: ID, hlc: HLC, op: { remove: null } },
    { id: ID, hlc: HLC, op: { put: ['\ud800', 1] } },
    { id: ID, hlc: HLC, op: { put: ['ana', NaN] } },
  ];
  for (const envelope of broken) {
    assert.throws(() => map.apply(envelope), InvalidOp, JSON.stringify(envelope));
  }
  assert.equal(JSON.stringify(map.state()), before);
});

test('two different ops with the same hlc are refused', () => {
  const map = new LwwMap();
  map.apply(put('ana', 'admin'));
  assert.throws(() => map.apply(put('ana', 'viewer')), /same hlc/);
});

test('the same op applied twice is accepted and changes nothing', () => {
  const map = new LwwMap();
  map.apply(put('ana', 'admin'));
  map.apply(put('ana', 'admin'));
  assert.deepEqual(map.value(), [['ana', 'admin']]);
});

test('keys of different JSON types do not collide', () => {
  const map = new LwwMap();
  map.apply(put('1', 'text key'));
  map.apply(put(1, 'number key', '018bcfe568010000000000000000000a'));
  // Both keys stay; numbers come before strings (D61).
  assert.deepEqual(map.value(), [[1, 'number key'], ['1', 'text key']]);
});
