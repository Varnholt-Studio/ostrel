import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Bucket, Window } from '../src/limits.ts';

test('bucket allows the burst, then refills at the rate', () => {
  const b = new Bucket(50, 200, 0);
  assert.ok(b.take(200, 0));
  assert.ok(!b.take(1, 0));
  assert.ok(b.take(50, 1000));
  assert.ok(!b.take(1, 1000));
});

test('window counts per key and forgets old hits', () => {
  const w = new Window(10, 60_000);
  for (let i = 0; i < 10; i++) assert.ok(w.allow('ip-a', 1, 0));
  assert.ok(!w.allow('ip-a', 1, 0));
  assert.ok(w.allow('ip-b', 1, 0));
  assert.equal(w.remaining('ip-a', 60_001), 10);
  assert.ok(!w.allow('ip-b', 10, 1), 'a batch over the limit is refused as a whole');
  assert.equal(w.remaining('ip-b', 1), 9);
});
