import { test } from 'node:test';
import assert from 'node:assert/strict';
import { hex, seqId } from '../src/ids.mjs';
import { Seq } from '../src/seq.mjs';
import { Editor, rng, textEdit } from '../src/workload.mjs';

const SEED = hex(1, 16);

function applyTo(seq, ops) {
  for (const op of ops) {
    if (op.k === 'ins') seq.insert(op.after, seqId(op.c, op.id.slice(0, 16)), op.s);
    else seq.remove(op.at);
  }
}

test('a snapshot reads back unchanged and expands lazily', () => {
  const s = new Seq('héllo \u{1f600}', SEED);
  assert.equal(s.elems, null);
  assert.equal(s.text(), 'héllo \u{1f600}');
  assert.equal(s.visibleIds().length, 7);
  assert.equal(s.text(), 'héllo \u{1f600}');
});

test('concurrent edits converge in both delivery orders and runs do not interleave', () => {
  const r = rng(11);
  for (let round = 0; round < 300; round++) {
    const base = new Seq('the quick brown fox jumps over the lazy dog', SEED);
    const row = { id: 'r', desc: base };
    const a = new Editor(2);
    const b = new Editor(3);
    const len = base.text().length;
    const pa = r.int(len);
    const pb = r.int(2) ? pa : r.int(len); // same origin half of the time
    const opsA = textEdit(a, a.wall + 1, row, pa, r.int(5), 'AAAA');
    const opsB = textEdit(b, b.wall + 1, row, pb, r.int(5), 'BBBB');
    const x = base.clone();
    const y = base.clone();
    applyTo(x, [...opsA, ...opsB]);
    applyTo(y, [...opsB, ...opsA]);
    assert.equal(x.text(), y.text());
    assert.ok(x.text().includes('AAAA') && x.text().includes('BBBB'), x.text());
  }
});

test('replayed inserts and deletes are idempotent', () => {
  const s = new Seq('abc', SEED);
  const row = { id: 'r', desc: s };
  const ops = textEdit(new Editor(2), 1, row, 1, 1, 'XY');
  applyTo(s, ops);
  const once = s.text();
  for (const op of ops) {
    if (op.k === 'ins') assert.equal(s.insert(op.after, seqId(op.c, op.id.slice(0, 16)), op.s), false);
    else assert.equal(s.remove(op.at), false);
  }
  assert.equal(s.text(), once);
  assert.equal(once, 'aXYc');
});
