import { test } from 'node:test';
import assert from 'node:assert/strict';
import { decodeOps, DecodeError } from '../src/decode.mjs';
import { hex, seqId } from '../src/ids.mjs';
import { Editor, message, relay } from '../src/workload.mjs';

const ROW = hex(5, 32);
const ed = new Editor(2);
const good = () => ed.op(ed.wall + 1, ROW, 'status', 'set', { v: 'todo' });

function rejects(ops, raw) {
  assert.throws(() => decodeOps(raw ?? message(ops)), DecodeError);
}

test('accepts every op kind the spike uses', () => {
  const ops = [
    good(),
    ed.op(ed.wall, ROW, 'title', 'set', { v: 'emoji \u{1f600} ok' }),
    ed.op(ed.wall, ROW, 'rank', 'set', { v: '00a1i' }),
    ed.op(ed.wall, ROW, 'labels', 'sadd', { e: 'bug' }),
    ed.op(ed.wall, ROW, 'labels', 'srem', { e: 'bug', tags: [hex(1, 24)] }),
    ed.op(ed.wall, ROW, 'desc', 'ins', { after: null, c: 9, s: '\u{1f600}' }),
    ed.op(ed.wall, ROW, 'desc', 'del', { at: seqId(1, hex(1, 16)) }),
  ];
  assert.equal(decodeOps(message(ops)).length, ops.length);
});

test('rejects hostile messages', () => {
  rejects(null, 'not json');
  rejects(null, JSON.stringify({ v: 1, t: 'Ops', ops: [] }));
  rejects(null, JSON.stringify({ v: 0, t: 'Ops', ops: [null] }));
  rejects(Array.from({ length: 501 }, good));
  rejects([{ ...good(), id: good().id.toUpperCase() }]);
  rejects([{ ...good(), row: ROW + '0' }]);
  rejects([{ ...good(), hlc: good().hlc.slice(0, 16) + hex(9, 16) }]);
  rejects([{ ...good(), f: '__proto__' }]);
  rejects([{ ...good(), f: 'constructor' }]);
  rejects([{ ...good(), v: 'nope' }]);
  rejects([{ ...good(), k: 'drop' }]);
  rejects([{ ...good(), f: 'title', v: 'lone \ud800 surrogate' }]);
  rejects([{ ...good(), f: 'title', v: 'x'.repeat(256 * 1024 + 1) }]);
  rejects([{ ...good(), f: 'rank', v: 'A!' }]);
  rejects([{ ...good(), f: 'desc', k: 'ins', after: null, c: 1, s: 'ab' }]);
  rejects([{ ...good(), f: 'desc', k: 'ins', after: null, c: 0, s: 'a' }]);
  rejects([{ ...good(), f: 'desc', k: 'set', v: 'x' }]);
  rejects([{ ...good(), f: 'labels', k: 'srem', e: 'bug', tags: Array(65).fill(hex(1, 24)) }]);
  rejects([{ ...good(), f: 'labels', k: 'srem', e: 'bug', tags: [] }]);
  rejects([{ ...good(), f: 'rank', v: 'a0' }]);
  // Lamport condition: counter must be above the origin's counter.
  rejects([{ ...good(), f: 'desc', k: 'ins', after: seqId(9, hex(1, 16)), c: 9, s: 'a' }]);
});

test('rejects missing, malformed and non increasing ServerSeq', () => {
  const [x, y] = relay([good(), good()]);
  const raw = (ops) => JSON.stringify({ v: 0, t: 'Ops', ops });
  assert.equal(decodeOps(raw([x, y])).length, 2);
  rejects(null, raw([{ ...x, ss: undefined }]));
  rejects(null, raw([{ ...x, ss: 7 }]));
  rejects(null, raw([{ ...x, ss: x.ss.toUpperCase() + 'A' }]));
  rejects(null, raw([y, x]));
  rejects(null, raw([x, x]));
});
