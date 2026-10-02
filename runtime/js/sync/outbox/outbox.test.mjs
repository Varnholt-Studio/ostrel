import { test } from 'node:test';
import assert from 'node:assert/strict';

import { openOutbox } from './outbox.mjs';
import { MemoryIndexedDB, MemoryKeyRange } from './testing/memory_indexeddb.mjs';

const ALICE = 'user-alice';
const BOB = 'user-bob';
const REPLICA_A = '00000000000000a1';
const REPLICA_B = '00000000000000b2';

function environment() {
  return { indexedDB: new MemoryIndexedDB(), IDBKeyRange: MemoryKeyRange };
}

function op(n) {
  return { kind: 'set', row: 'r1', field: 'title', value: `v${n}` };
}

test('append allocates contiguous seqs and stores the op with user and replica', async () => {
  const outbox = await openOutbox(environment());
  const first = await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(1), nowMs: 1000 });
  const second = await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(2), nowMs: 1000 });

  assert.equal(first.opId, REPLICA_A + '00000001');
  assert.equal(second.opId, REPLICA_A + '00000002');
  assert.equal(first.hlc, '0000000003e8' + '0000' + REPLICA_A);
  assert.equal(second.hlc, '0000000003e8' + '0001' + REPLICA_A);
  assert.deepEqual(await outbox.pending({ user: ALICE, replica: REPLICA_A }), [first, second]);
  assert.equal(first.user, ALICE);
  assert.deepEqual(first.op, op(1));
});

test('two tabs appending at the same time never share an OpId', async () => {
  const env = environment();
  const tabOne = await openOutbox(env);
  const tabTwo = await openOutbox(env);
  const appends = [];
  for (let i = 0; i < 50; i += 1) {
    appends.push(tabOne.append({ user: ALICE, replica: REPLICA_A, op: op(i), nowMs: 1000 }));
    appends.push(tabTwo.append({ user: ALICE, replica: REPLICA_A, op: op(i), nowMs: 1000 }));
  }
  const entries = await Promise.all(appends);

  const seqs = entries.map((entry) => entry.seq).sort((a, b) => a - b);
  assert.deepEqual(seqs, Array.from({ length: 100 }, (_, i) => i + 1));
  const hlcs = new Set(entries.map((entry) => entry.hlc));
  assert.equal(hlcs.size, 100);
  const pending = await tabTwo.pending({ user: ALICE, replica: REPLICA_A, limit: 1000 });
  assert.deepEqual(
    pending.map((entry) => entry.seq),
    seqs,
  );
});

test('the outbox and the clock survive closing and reopening (reload)', async () => {
  const env = environment();
  const before = await openOutbox(env);
  await before.append({ user: ALICE, replica: REPLICA_A, op: op(1), nowMs: 1000 });
  before.close();

  const after = await openOutbox(env);
  const next = await after.append({ user: ALICE, replica: REPLICA_A, op: op(2), nowMs: 500 });
  assert.equal(next.seq, 2);
  assert.equal(next.hlc, '0000000003e8' + '0001' + REPLICA_A, 'HLC never goes back');
  assert.equal((await after.pending({ user: ALICE, replica: REPLICA_A })).length, 2);
});

test('a failed append leaves no entry and no gap in seq', async () => {
  const env = environment();
  const outbox = await openOutbox(env);
  await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(1), nowMs: 1000 });

  env.indexedDB.failNext(({ store, method }) => store === 'entries' && method === 'put');
  await assert.rejects(
    outbox.append({ user: ALICE, replica: REPLICA_A, op: op(2), nowMs: 1000 }),
    /injected failure/,
  );

  const retry = await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(2), nowMs: 1000 });
  assert.equal(retry.seq, 2, 'the clock update was rolled back with the entry');
  const pending = await outbox.pending({ user: ALICE, replica: REPLICA_A });
  assert.deepEqual(
    pending.map((entry) => entry.seq),
    [1, 2],
  );
});

test('pending returns the oldest entries first and respects the limit', async () => {
  const outbox = await openOutbox(environment());
  for (let i = 1; i <= 20; i += 1) {
    await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(i), nowMs: 1000 + i });
  }
  const page = await outbox.pending({ user: ALICE, replica: REPLICA_A, limit: 5 });
  assert.deepEqual(
    page.map((entry) => entry.seq),
    [1, 2, 3, 4, 5],
  );
});

test('remove deletes acknowledged entries and ignores repeats', async () => {
  const outbox = await openOutbox(environment());
  const first = await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(1), nowMs: 1 });
  const second = await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(2), nowMs: 2 });
  await outbox.remove([first.opId]);
  await outbox.remove([first.opId]);

  assert.deepEqual(await outbox.pending({ user: ALICE, replica: REPLICA_A }), [second]);
  const third = await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(3), nowMs: 3 });
  assert.equal(third.seq, 3, 'removing entries does not reuse seqs');
  await assert.rejects(outbox.remove(['not-an-op-id']), TypeError);
});

test('entries of another user or replica are never pending and can be discarded', async () => {
  const outbox = await openOutbox(environment());
  await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(1), nowMs: 1 });
  await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(2), nowMs: 2 });
  const bobs = await outbox.append({ user: BOB, replica: REPLICA_B, op: op(3), nowMs: 3 });

  assert.deepEqual(await outbox.pending({ user: BOB, replica: REPLICA_A }), []);
  assert.equal(await outbox.discardForeign({ user: BOB, replica: REPLICA_B }), 2);
  assert.deepEqual(await outbox.pending({ user: BOB, replica: REPLICA_B }), [bobs]);
  assert.deepEqual(await outbox.pending({ user: ALICE, replica: REPLICA_A }), []);
  assert.equal(await outbox.discardForeign({ user: BOB, replica: REPLICA_B }), 0);
});

test('observe and resumeAfter move the clock forward for later appends', async () => {
  const outbox = await openOutbox(environment());
  await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(1), nowMs: 1000 });
  await outbox.observe({
    replica: REPLICA_A,
    hlc: '000000002710' + '0004' + REPLICA_B,
    nowMs: 1000,
  });
  await outbox.resumeAfter({ replica: REPLICA_A, lastSeq: 41 });

  const next = await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(2), nowMs: 1000 });
  assert.equal(next.seq, 42);
  assert.equal(next.hlc, '000000002710' + '0005' + REPLICA_A);
});

test('invalid arguments reject without touching the database', async () => {
  const outbox = await openOutbox(environment());
  await assert.rejects(outbox.append({ user: '', replica: REPLICA_A, op: op(1) }), TypeError);
  await assert.rejects(outbox.append({ user: ALICE, replica: 'a1', op: op(1) }), TypeError);
  await assert.rejects(outbox.append({ user: ALICE, replica: REPLICA_A, op: null }), TypeError);
  await assert.rejects(outbox.pending({ user: ALICE, replica: REPLICA_A, limit: 0 }), RangeError);
  await assert.rejects(outbox.observe({ replica: REPLICA_A, hlc: 'zz' }), TypeError);
  assert.deepEqual(await outbox.pending({ user: ALICE, replica: REPLICA_A }), []);
});

test('an exhausted clock rejects the append and stores nothing', async () => {
  const outbox = await openOutbox(environment());
  await outbox.resumeAfter({ replica: REPLICA_A, lastSeq: 0xffffffff });
  await assert.rejects(
    outbox.append({ user: ALICE, replica: REPLICA_A, op: op(1), nowMs: 1 }),
    RangeError,
  );
  assert.deepEqual(await outbox.pending({ user: ALICE, replica: REPLICA_A }), []);
});

test('an op that cannot be stored aborts the append, clock included', async () => {
  const outbox = await openOutbox(environment());
  await assert.rejects(
    outbox.append({ user: ALICE, replica: REPLICA_A, op: { notCloneable: () => 1 }, nowMs: 1 }),
    { name: 'DataCloneError' },
  );
  const next = await outbox.append({ user: ALICE, replica: REPLICA_A, op: op(1), nowMs: 1 });
  assert.equal(next.seq, 1);
});
