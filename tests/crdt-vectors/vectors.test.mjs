// Runs every op vector (README.md) against every JavaScript model of its strategy.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

import { encode } from '../../runtime/js/crdt/canon/canon.mjs';
import * as map from '../../runtime/js/crdt/map/map.mjs';
import * as rank from '../../runtime/js/crdt/rank/rank.mjs';
import * as register from '../../runtime/js/crdt/register/register.mjs';
import { AddWinsSet } from '../../runtime/js/crdt/set/add_wins_set.mjs';
import { loadVectors, STRATEGIES, validateVector } from './format.mjs';
import { createLwwReplica, createRankReplica, createSetReplica } from './oracle.mjs';

// The set module has its own interface (`apply(op, opId)`, `values()`, `tagsOf(e)`); this
// adapter gives it the model shape of README.md without changing the module. The set module
// knows no make: the initial elements of a row reach it as one add per element, each with the
// make's OpId as tag (D106, D107), which is what the adapter does.
function createAddWinsSetReplica() {
  const set = new AddWinsSet();
  return {
    apply: ({ id, op }) => {
      if (!('make' in op)) return set.apply(op, id);
      for (const element of op.make) set.apply({ add: element }, id);
    },
    value: () => set.values(),
    state: () => set.values().map((element) => [element, set.tagsOf(element)]),
  };
}

// Strategy -> named replica factories. Every runtime module runs next to the oracle (if the
// strategy has one), so both are checked against the same hand written expectations.
const MODELS = {
  lww: { 'oracle.mjs': createLwwReplica, 'runtime/js/crdt/register': register.createReplica },
  set: { 'oracle.mjs': createSetReplica, 'runtime/js/crdt/set': createAddWinsSetReplica },
  map: { 'runtime/js/crdt/map': map.createReplica },
  rank: { 'oracle.mjs': createRankReplica, 'runtime/js/crdt/rank': rank.createReplica },
};

test('every strategy directory has vectors and at least one model', () => {
  assert.deepEqual(Object.keys(MODELS).sort(), [...STRATEGIES].sort());
  for (const strategy of STRATEGIES) {
    assert.ok(loadVectors(strategy).length > 0, `${strategy}/ has no vectors`);
  }
});

for (const strategy of STRATEGIES) {
  for (const { file, vector } of loadVectors(strategy)) {
    for (const [model, createReplica] of Object.entries(MODELS[strategy])) {
      test(`ac_41_crdt_vectors_js ${file} with ${model}`, () => {
        vector.deliveries.forEach((order, number) => {
          const replica = createReplica();
          for (const index of order) replica.apply(vector.ops[index]);
          const where = `delivery ${number} [${order.join(', ')}]`;
          assert.equal(encode(replica.state()), encode(vector.expect.state), `${where}: state`);
          assert.equal(encode(replica.value()), encode(vector.expect.value), `${where}: value`);
        });
      });
    }
  }
}

// Fixed keys of keyBetween, so the Rust implementation can return the same keys.
const { cases: rankKeyCases } = JSON.parse(
  readFileSync(new URL('rank/keys/cases.json', import.meta.url), 'utf8'),
);

for (const { name, before, after, key, error } of rankKeyCases) {
  test(`ac_41_crdt_vectors_js rank/keys ${name}`, () => {
    if (error !== undefined) {
      assert.throws(() => rank.keyBetween(before, after), rank.InvalidRank);
      return;
    }
    assert.equal(rank.keyBetween(before, after), key);
  });
}

// The validator itself: a few broken vectors must be refused with a reason.
const A1 = '000000000000000a00000001';
const HLC_A = '018bcfe568000000000000000000000a';

function setVector(overrides) {
  return {
    strategy: 'set',
    description: 'probe',
    ops: [
      { id: A1, hlc: HLC_A, op: { add: 'x' } },
      { id: '000000000000000b00000001', hlc: '018bcfe568010000000000000000000b',
        op: { remove: 'x', tags: [A1] } },
    ],
    deliveries: [[0, 1]],
    expect: { value: [], state: [] },
    ...overrides,
  };
}

test('validator accepts the probe vector', () => {
  validateVector(setVector({}), 'set');
});

test('validator refuses an add delivered again after its remove', () => {
  assert.throws(() => validateVector(setVector({ deliveries: [[0, 1, 0]] }), 'set'), /after its remove/);
});

test('validator refuses a delivery that leaves out an op', () => {
  assert.throws(() => validateVector(setVector({ deliveries: [[0]] }), 'set'), /never delivers/);
});

test('validator refuses uppercase hex and mismatched replicas', () => {
  const upper = setVector({});
  upper.ops[0] = { ...upper.ops[0], hlc: HLC_A.toUpperCase() };
  assert.throws(() => validateVector(upper, 'set'), /Hlc/);

  const mixed = setVector({});
  mixed.ops[0] = { ...mixed.ops[0], hlc: '018bcfe568000000000000000000000b' };
  assert.throws(() => validateVector(mixed, 'set'), /replica in hlc and id differ/);
});

test('validator refuses a gap in the seq of a replica', () => {
  const gap = setVector({});
  gap.ops[0] = { ...gap.ops[0], id: '000000000000000a00000002' };
  assert.throws(() => validateVector(gap, 'set'), /must use seq 1/);
});

test('validator refuses a remove with more than 64 tags', () => {
  const tags = Array.from({ length: 65 }, (_, i) => `000000000000000c${(i + 1).toString(16).padStart(8, '0')}`);
  const vector = setVector({});
  vector.ops[1] = { ...vector.ops[1], op: { remove: 'x', tags } };
  assert.throws(() => validateVector(vector, 'set'), /at most 64 tags/);
});

test('validator refuses a vector filed under the wrong strategy', () => {
  assert.throws(() => validateVector(setVector({}), 'map'), /does not match directory/);
});

// A tag is the OpId of an add (README.md); the id of a remove is an authoring error.
const B1 = '000000000000000b00000001';
const C1 = '000000000000000c00000001';
const HLC_C = '018bcfe568020000000000000000000c';

test('validator refuses a tag that names another remove', () => {
  const vector = setVector({ deliveries: [[0, 1, 2]] });
  vector.ops.push({ id: C1, hlc: HLC_C, op: { remove: 'x', tags: [B1] } });
  const reason = /names tag 000000000000000b00000001, which is the id of the remove ops\[1\]/;
  assert.throws(() => validateVector(vector, 'set'), reason);
});

test('validator refuses a remove that names its own id', () => {
  const vector = setVector({});
  vector.ops[1] = { ...vector.ops[1], op: { remove: 'x', tags: [A1, B1] } };
  assert.throws(() => validateVector(vector, 'set'), /which is the id of the remove ops\[1\]/);
});

test('validator refuses a set op that is both add and remove', () => {
  const vector = setVector({ deliveries: [[0, 1, 2]] });
  vector.ops.push({ id: C1, hlc: HLC_C, op: { remove: 'x', tags: [B1] } });
  vector.ops[1] = { ...vector.ops[1], op: { add: 'x', remove: 'x', tags: [A1] } };
  assert.throws(() => validateVector(vector, 'set'), /must be \{ "add": e \} or/);
});

// make (D107): one op tags every initial element of the row with its own id.
const M1 = '000000000000000a00000001';

function makeVector(make, extra, deliveries) {
  return setVector({
    ops: [{ id: M1, hlc: HLC_A, op: { make } }, ...extra],
    deliveries,
    expect: { value: [], state: [] },
  });
}

test('validator accepts a make, also an empty one', () => {
  const remove = { id: B1, hlc: '018bcfe568010000000000000000000b', op: { remove: 'e1', tags: [M1] } };
  validateVector(makeVector(['e1', 'e2'], [remove], [[0, 1]]), 'set');
  validateVector(makeVector([], [remove], [[0, 1], [1, 0]]), 'set');
});

test('validator refuses a second make', () => {
  const second = { id: C1, hlc: HLC_C, op: { make: ['e3'] } };
  assert.throws(() => validateVector(makeVector(['e1'], [second], [[0, 1]]), 'set'), /second make/);
});

test('validator refuses a make that lists one element twice', () => {
  assert.throws(() => validateVector(makeVector(['e1', 'e2', 'e1'], [], [[0]]), 'set'), /twice/);
  assert.throws(() => validateVector(makeVector([1, 'x', 1.5, 1], [], [[0]]), 'set'), /twice/);
});

test('validator refuses a make whose list is not an array', () => {
  assert.throws(() => validateVector(makeVector('e1', [], [[0]]), 'set'), /array of elements/);
});

test('validator refuses a make delivered after a remove of one of its elements', () => {
  const remove = { id: B1, hlc: '018bcfe568010000000000000000000b', op: { remove: 'e2', tags: [M1] } };
  const late = makeVector(['e1', 'e2'], [remove], [[0, 1, 0]]);
  assert.throws(() => validateVector(late, 'set'), /delivers ops\[0\] after its remove ops\[1\]/);
});

test('validator ignores a make tag named for an element the make does not list (D96)', () => {
  const remove = { id: B1, hlc: '018bcfe568010000000000000000000b', op: { remove: 'e3', tags: [M1] } };
  validateVector(makeVector(['e1', 'e2'], [remove], [[1, 0], [0, 1, 0]]), 'set');
});
