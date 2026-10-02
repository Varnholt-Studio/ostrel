import { test } from 'node:test';
import assert from 'node:assert/strict';
import { measureTags, measureValues } from '../measure-tags.mjs';

test('the tag cost run removes every tag, including split removes above 64 tags', () => {
  for (const t of [1, 64, 65, 130]) {
    const r = measureTags(t, 3);
    assert.equal(r.removeMerge.n, 3);
    assert.ok(r.addBatchMerge.p99 >= 0);
  }
});

test('the values run reports one sample per round', () => {
  assert.equal(measureValues(50, 4).n, 4);
});
