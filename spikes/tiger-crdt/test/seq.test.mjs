import { test } from 'node:test';
import assert from 'node:assert/strict';
import { hex, seqId } from '../src/ids.mjs';
import { RgaReference } from '../src/rga.mjs';
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

test('the linear scan agrees with the independent tree reference', () => {
  const r = rng(23);
  for (let round = 0; round < 60; round++) {
    const base = 'concurrent editing \u{1f600} of one description';
    const seq = new Seq(base, SEED);
    const ref = new RgaReference(base, SEED);
    const row = { id: 'r', desc: seq };
    const eds = [new Editor(2), new Editor(3), new Editor(4)];
    for (let step = 0; step < 20; step++) {
      // Several editors edit the same state, then all ops arrive in a random order that
      // keeps each editor's own order (causal per editor).
      const batches = eds.map((ed) => {
        const len = seq.visibleIds().length;
        return textEdit(ed, ed.wall + 1, row, r.int(len + 1), r.int(3), 'xyz'.slice(0, 1 + r.int(3)));
      });
      const merged = [];
      while (batches.some((b) => b.length)) {
        const open = batches.filter((x) => x.length);
        merged.push(open[r.int(open.length)].shift());
      }
      applyTo(seq, merged);
      ref.applyOps(merged);
      assert.equal(seq.text(), ref.text());
    }
  }
});

test('has answers for an unexpanded snapshot without expanding it', () => {
  const s = new Seq('a\u{1f600}c', SEED);
  assert.equal(s.has(seqId(3, SEED)), true);
  assert.equal(s.has(seqId(4, SEED)), false);
  assert.equal(s.has(seqId(1, hex(2, 16))), false);
  assert.equal(s.elems, null);
});
