import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createDocument } from '../src/fakedom.mjs';
import { handleMessage } from '../src/pipeline.mjs';
import { STATUSES } from '../src/schema.mjs';
import { Store } from '../src/store.mjs';
import { Board } from '../src/view.mjs';
import { backgroundEdit, Editor, makeDataset, message, rng, SEED_HLC, SEED_REPLICA } from '../src/workload.mjs';

test('patched lists always show the query window in order with current values', () => {
  const doc = createDocument();
  const store = new Store();
  store.load(makeDataset(2, 600), SEED_HLC, SEED_REPLICA);
  const board = new Board(doc, doc.body, store, STATUSES, 12);
  const rows = [...store.rows.values()];
  const queries = board.columns.map((c) => c.query);
  const r = rng(9);
  const ed = new Editor(2);
  let wall = ed.wall;
  for (let i = 0; i < 1500; i++) {
    if (i % 50 === 0) {
      const col = board.columns[r.int(5)];
      col.scrollTo(r.int(col.query.items.length));
      board.detail.show(r.pick(rows));
    }
    wall += 1;
    handleMessage(message(backgroundEdit(r, ed, wall, store, rows, queries)), store, board, () => 0);
    for (const col of board.columns) {
      const want = col.visible();
      const got = col.el.childNodes;
      assert.equal(got.length, want.length);
      for (let k = 0; k < want.length; k++) {
        assert.equal(got[k].getAttribute('data-id'), want[k].id);
        assert.equal(got[k].childNodes[0].textContent, want[k].title.v);
        assert.equal(got[k].childNodes[3].textContent, want[k].labels.values().join(', '));
      }
    }
    const d = board.detail;
    assert.equal(d.rendered('desc'), d.row.desc.text());
    assert.equal(d.rendered('status'), d.row.status.v);
  }
});
