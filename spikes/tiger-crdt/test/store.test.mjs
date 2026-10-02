import { test } from 'node:test';
import assert from 'node:assert/strict';
import { decodeOps } from '../src/decode.mjs';
import { STATUSES } from '../src/schema.mjs';
import { BatchError, Store } from '../src/store.mjs';
import { backgroundEdit, Editor, makeDataset, message, relay, rng, SEED_HLC, SEED_REPLICA } from '../src/workload.mjs';

function setup(count) {
  const store = new Store();
  store.load(makeDataset(1, count), SEED_HLC, SEED_REPLICA);
  const queries = STATUSES.map((s) => store.query('status', s, 'rank'));
  return { store, queries, rows: [...store.rows.values()] };
}

function recompute(store, q) {
  const items = [...store.rows.values()].filter((r) => r.status.v === q.filterValue);
  items.sort((a, b) => q.compare(a.rank.v, a.id, b.rank.v, b.id));
  return items.map((r) => r.id);
}

test('incremental live queries equal a full recompute after random edits', () => {
  const { store, queries, rows } = setup(800);
  const r = rng(5);
  const eds = [new Editor(2), new Editor(3), new Editor(4)];
  let wall = eds[0].wall;
  for (let i = 0; i < 2000; i++) {
    const batch = [];
    for (let k = 0; k < 1 + r.int(4); k++) {
      wall += 1;
      batch.push(...backgroundEdit(r, eds[r.int(3)], wall, store, rows, queries));
    }
    const ops = decodeOps(message(batch));
    store.invalidate(store.apply(ops));
    if (i % 100 === 0) {
      for (const q of queries) assert.deepEqual(q.items.map((x) => x.id), recompute(store, q));
    }
  }
  for (const q of queries) assert.deepEqual(q.items.map((x) => x.id), recompute(store, q));
});

test('last writer wins by HLC regardless of arrival order, replays are skipped', () => {
  const a = setup(10);
  const b = setup(10);
  const row = a.rows[0].id;
  const e2 = new Editor(2);
  const e3 = new Editor(3);
  const x = e2.op(e2.wall + 5, row, 'status', 'set', { v: 'done' });
  const y = e3.op(e3.wall + 1, row, 'status', 'set', { v: 'review' });
  const toA = relay([x, y]);
  a.store.invalidate(a.store.apply(toA));
  b.store.invalidate(b.store.apply(relay([y, x])));
  assert.equal(a.store.rows.get(row).status.v, 'done');
  assert.equal(b.store.rows.get(row).status.v, 'done');
  const touched = a.store.apply([toA[0]]);
  assert.equal(touched.size, 0);
});

test('a redelivered add after its remove does not bring the element back (D62)', () => {
  const { store, rows } = setup(5);
  const row = rows[0];
  const ed = new Editor(2);
  const [add] = relay([ed.op(ed.wall + 1, row.id, 'labels', 'sadd', { e: 'zz' })]);
  store.apply([add]);
  assert.ok(row.labels.has('zz'));
  const [rem] = relay([ed.op(ed.wall + 1, row.id, 'labels', 'srem', { e: 'zz', tags: row.labels.observed('zz') })]);
  store.apply([rem]);
  // Reconnect overlap: catch up resends the add with its original ServerSeq.
  const touched = store.apply(decodeOps(message([add])));
  assert.equal(touched.size, 0);
  assert.equal(row.labels.has('zz'), false);
});

test('ops are deduplicated by ServerSeq, not by the sending replica seq', () => {
  const { store, rows } = setup(5);
  const row = rows[1];
  const ed = new Editor(2);
  const earlier = ed.op(ed.wall + 1, row.id, 'assignee', 'set', { v: 'user7' });
  const later = ed.op(ed.wall + 1, row.id, 'title', 'set', { v: 'second title of this row' });
  // The log may hold a replica's ops out of seq order or with a gap (for example after a
  // split outbox drain); every log position is applied exactly once.
  store.apply(relay([later]));
  const touched = store.apply(relay([earlier]));
  assert.equal(touched.size, 1);
  assert.equal(row.assignee.v, 'user7');
});

test('a batch with an unknown text reference is refused as a whole', () => {
  const { store, rows } = setup(5);
  const row = rows[2];
  const ed = new Editor(2);
  const before = { status: row.status.v, desc: row.desc.text(), mark: store.appliedServerSeq };
  const bad = relay([
    ed.op(ed.wall + 1, row.id, 'status', 'set', { v: before.status === 'done' ? 'todo' : 'done' }),
    ed.op(ed.wall + 1, row.id, 'desc', 'ins', { after: null, c: 9000, s: 'x' }),
    ed.op(ed.wall + 1, row.id, 'desc', 'del', { at: 'ffffffff' + ed.replica }),
  ]);
  assert.throws(() => store.apply(bad), BatchError);
  assert.equal(row.status.v, before.status);
  assert.equal(row.desc.text(), before.desc);
  assert.equal(store.appliedServerSeq, before.mark);
  // A delete of an element inserted earlier in the same batch is fine.
  const ok = relay([
    ed.op(ed.wall + 1, row.id, 'desc', 'ins', { after: null, c: 9001, s: 'y' }),
    ed.op(ed.wall + 1, row.id, 'desc', 'del', { at: (9001).toString(16).padStart(8, '0') + ed.replica }),
  ]);
  store.apply(ok);
  assert.equal(row.desc.text(), before.desc);
  assert.equal(store.appliedServerSeq, ok[1].ss);
});
