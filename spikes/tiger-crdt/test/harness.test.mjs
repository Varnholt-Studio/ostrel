import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createDocument } from '../src/fakedom.mjs';
import { runSpike } from '../src/harness.mjs';
import { Seq } from '../src/seq.mjs';
import { Store } from '../src/store.mjs';

function env() {
  const doc = createDocument();
  return { doc, root: doc.body, now: () => performance.now(), nextFrame: async () => {}, sleep: async () => {} };
}

const SMALL = { count: 500, probes: 150, warmup: 10 };

test('a small run has no oracle failures and reports every step', async () => {
  const res = await runSpike(env(), SMALL);
  assert.deepEqual(res.failures, []);
  for (const k of ['decode', 'merge', 'query', 'view', 'frame', 'total']) {
    assert.equal(res.steps[k].n, SMALL.probes);
    assert.equal(res.raw[k].length, SMALL.probes);
  }
});

test('the oracle catches a merge that ignores the HLC rule', async () => {
  const original = Store.prototype.apply;
  Store.prototype.apply = function (ops) {
    // Sabotage: last arrival wins instead of highest HLC.
    const touched = new Map();
    for (const op of ops) {
      if (op.k === 'set') this.rows.get(op.row)[op.f].h = '0'.repeat(32);
      for (const [id, t] of original.call(this, [op])) {
        if (touched.has(id)) for (const f of t.fields) touched.get(id).fields.add(f);
        else touched.set(id, t);
      }
    }
    return touched;
  };
  try {
    const res = await runSpike(env(), SMALL);
    assert.ok(res.failures.length > 0);
  } finally {
    Store.prototype.apply = original;
  }
});

test('the oracle catches a text merge that depends on arrival order', async () => {
  const original = Seq.prototype.insert;
  Seq.prototype.insert = function (after, id, ch) {
    // Sabotage: no tie order, a new element always goes right after its origin.
    this.expand();
    if (this.ids.has(id)) return false;
    const at = after === null ? 0 : this.indexOf(after) + 1;
    this.elems.splice(at, 0, { id, ch, del: false });
    this.ids.add(id);
    this.cache = null;
    return true;
  };
  try {
    const res = await runSpike(env(), SMALL);
    assert.ok(res.failures.some((f) => f.kind === 'text'));
  } finally {
    Seq.prototype.insert = original;
  }
});

test('the measure script runs in Node mode', () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const out = execFileSync(process.execPath, [join(here, '..', 'measure.mjs'), '--count', '300', '--probes', '30', '--warmup', '5'], {
    encoding: 'utf8',
  });
  assert.match(out, /failures: 0/);
  assert.match(out, /^total /m);
});
