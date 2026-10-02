// Tests for the in-memory Web Locks stand in, so leader tests rest on checked behaviour.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { createMemoryLocks } from './memory_locks.mjs';

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

test('grants exclusively and in FIFO order', async () => {
  const m = createMemoryLocks();
  const order = [];
  const releases = {};
  const run = (id) =>
    m.forTab(id).request('l', () => {
      order.push(id);
      return new Promise((resolve) => { releases[id] = resolve; });
    });
  const pa = run('a');
  const pb = run('b');
  const pc = run('c');
  await tick();
  assert.deepEqual(order, ['a']);
  assert.equal(m.holder('l'), 'a');
  releases.a('ra');
  assert.equal(await pa, 'ra');
  await tick();
  assert.deepEqual(order, ['a', 'b']);
  releases.b();
  await pb;
  await tick();
  releases.c();
  await pc;
  assert.equal(m.holder('l'), null);
});

test('a rejecting callback releases the lock and rejects the request', async () => {
  const m = createMemoryLocks();
  await assert.rejects(m.forTab('a').request('l', async () => { throw new Error('x'); }), /x/);
  assert.equal(m.holder('l'), null);
});

test('abort removes a queued request', async () => {
  const m = createMemoryLocks();
  let releaseA;
  const pa = m.forTab('a').request('l', () => new Promise((r) => { releaseA = r; }));
  const ctrl = new AbortController();
  let granted = false;
  const pb = m.forTab('b').request('l', { signal: ctrl.signal }, () => { granted = true; });
  ctrl.abort();
  await assert.rejects(pb, (err) => err.name === 'AbortError');
  releaseA();
  await pa;
  await tick();
  assert.equal(granted, false);
  assert.equal(m.holder('l'), null);
});

test('ifAvailable calls back with null when held', async () => {
  const m = createMemoryLocks();
  m.forTab('a').request('l', () => new Promise(() => {}));
  const got = await m.forTab('b').request('l', { ifAvailable: true }, (lock) => lock);
  assert.equal(got, null);
});

test('kill releases held locks and drops queued requests of that tab', async () => {
  const m = createMemoryLocks();
  const a = m.forTab('a');
  const b = m.forTab('b');
  const c = m.forTab('c');
  a.request('l', () => new Promise(() => {}));
  let bGranted = false;
  b.request('l', () => { bGranted = true; return new Promise(() => {}); });
  let cGranted = false;
  c.request('l', () => { cGranted = true; return new Promise(() => {}); });
  b.kill();
  a.kill();
  await tick();
  assert.equal(bGranted, false);
  assert.equal(cGranted, true);
  assert.equal(m.holder('l'), 'c');
});

test('only exclusive mode is supported', () => {
  const m = createMemoryLocks();
  assert.throws(() => m.forTab('a').request('l', { mode: 'shared' }, () => {}), TypeError);
});
