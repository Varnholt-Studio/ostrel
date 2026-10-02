import { test } from 'node:test';
import assert from 'node:assert/strict';
import { opId, hex } from '../src/ids.mjs';
import { OrSet } from '../src/orset.mjs';

const A = hex(2, 16);
const B = hex(3, 16);

test('a concurrent add survives a remove that did not observe it (add wins)', () => {
  const s = new OrSet();
  s.add('bug', opId(A, 1));
  const observedByB = s.observed('bug');
  s.add('bug', opId(A, 2)); // A re-adds, B has not seen it
  s.remove('bug', observedByB);
  assert.ok(s.has('bug'));
});

test('a remove of every observed tag removes the element', () => {
  const s = new OrSet();
  s.add('bug', opId(A, 1));
  s.add('bug', opId(B, 1));
  s.remove('bug', s.observed('bug'));
  assert.equal(s.has('bug'), false);
});

test('at most one live tag per element and replica, replays are no-ops', () => {
  const s = new OrSet();
  assert.equal(s.add('x', opId(A, 1)), true);
  assert.equal(s.add('x', opId(A, 4)), true);
  assert.equal(s.add('x', opId(A, 1)), false);
  assert.deepEqual(s.observed('x'), [opId(A, 4)]);
  assert.equal(s.remove('x', [opId(A, 1)]), false);
  assert.ok(s.has('x'));
});
