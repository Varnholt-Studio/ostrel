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
// adapter gives it the model shape of README.md without changing the module.
function createAddWinsSetReplica() {
  const set = new AddWinsSet();
  return {
    apply: ({ id, op }) => set.apply(op, id),
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
