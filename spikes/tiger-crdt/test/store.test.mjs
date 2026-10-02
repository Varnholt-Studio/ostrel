import { test } from 'node:test';
import assert from 'node:assert/strict';
import { decodeOps } from '../src/decode.mjs';
import { STATUSES } from '../src/schema.mjs';
import { Store } from '../src/store.mjs';
import { backgroundEdit, Editor, makeDataset, message, rng, SEED_HLC, SEED_REPLICA } from '../src/workload.mjs';

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
  a.store.invalidate(a.store.apply([x, y]));
  b.store.invalidate(b.store.apply([y, x]));
  assert.equal(a.store.rows.get(row).status.v, 'done');
  assert.equal(b.store.rows.get(row).status.v, 'done');
  const touched = a.store.apply([x]);
  assert.equal(touched.size, 0);
});
