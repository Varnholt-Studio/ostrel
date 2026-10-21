// Tests for the wiring of tab leader, outbox and socket. Tabs are simulated in one process with
// the in-memory lock manager, one shared in-memory IndexedDB and real BroadcastChannels on a
// per-test channel name, as in the leader tests.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { createMemoryLocks } from '../leader/testing/memory_locks.mjs';
import { openOutbox } from '../outbox/outbox.mjs';
import { MemoryIndexedDB, MemoryKeyRange } from '../outbox/testing/memory_indexeddb.mjs';
import { createSyncClient } from './client.mjs';
import { FakeSocket, memoryIdentity, until, welcome } from './testing/fake_server.mjs';

const ALICE = 'user-alice';
const REPLICA = '00000000000000a1';
const ROW = '0000000003e80000' + REPLICA;
const SEQ = '0000000000000010';

let channelSeq = 0;

function op(n) {
  return { model: 'Issue', row: ROW, body: { set: { field: 'title', value: `v${n}` } } };
}

// A server that welcomes every hello on REPLICA unless `refuse` is set, and records sockets.
function harness({ retryMs = 20 } = {}) {
  const locks = createMemoryLocks();
  const indexedDB = new MemoryIndexedDB();
  const channelName = `ostrel-sync-client-test-${process.pid}-${channelSeq++}`;
  const sockets = [];
  const server = { refuse: null, autoWelcome: true };
  const tabs = [];

  function connect() {
    const socket = new FakeSocket();
    sockets.push(socket);
    queueMicrotask(() => {
      socket.open();
      queueMicrotask(() => {
        if (server.refuse !== null) {
          socket.deliver({ t: 'error', sub: null, code: server.refuse, retry_after_ms: null });
        } else if (server.autoWelcome) {
          socket.deliver(welcome(REPLICA));
        }
      });
    });
    return socket;
  }

  async function tab(id) {
    const outbox = await openOutbox({ indexedDB, IDBKeyRange: MemoryKeyRange });
    const events = [];
    const record = (name) => (payload) => events.push({ name, payload });
    const client = createSyncClient({
      tabId: id,
      locks: locks.forTab(id),
      openChannel: (name) => new BroadcastChannel(name),
      connect,
      outbox,
      identity: memoryIdentity({ user: ALICE, replica: REPLICA }),
      models: ['Issue'],
      hooks: {
        onWelcome: record('welcome'),
        onAck: record('ack'),
        onReject: record('reject'),
        onNotice: record('notice'),
        onError: record('error'),
      },
      reconnectMs: 5,
      maxReconnectMs: 20,
      leaderOptions: { channelName, retryMs },
    });
    const handle = { id, client, outbox, events };
    tabs.push(handle);
    return handle;
  }

  async function cleanup() {
    for (const t of tabs) await t.client.stop();
  }

  return { sockets, server, tab, cleanup, locks };
}

const named = (t, name) => t.events.filter((e) => e.name === name);
const tick = (ms = 10) => new Promise((resolve) => setTimeout(resolve, ms));

test('only the leader connects; a write in a follower reaches the socket', async () => {
  const h = harness();
  try {
    const a = await h.tab('a');
    const b = await h.tab('b');
    a.client.start();
    await until(() => a.client.isLeader(), 'a leads');
    b.client.start();
    await until(() => named(a, 'welcome').length === 1, 'welcome');
    await tick();
    assert.equal(h.sockets.length, 1);
    assert.equal(b.client.isLeader(), false);

    const entry = await b.outbox.append({ user: ALICE, replica: REPLICA, op: op(1), nowMs: 1000 });
    b.client.changed();
    const socket = h.sockets[0];
    await until(() => socket.frames.length === 2, 'ops frame');
    assert.deepEqual(socket.sent()[1].ops.map((o) => o.id), [entry.opId]);

    // The outcome reaches both tabs: the leader directly, the follower over the channel.
    socket.deliver({ t: 'ack', op: entry.opId, seq: SEQ, restamped: [] });
    await until(() => named(b, 'ack').length === 1, 'ack in the follower');
    assert.deepEqual(named(b, 'ack')[0].payload.confirmed, [entry.opId]);
    assert.deepEqual(named(a, 'ack')[0].payload.confirmed, [entry.opId]);
  } finally {
    await h.cleanup();
  }
});

test('a reject reaches the follower that wrote the op', async () => {
  const h = harness();
  try {
    const a = await h.tab('a');
    const b = await h.tab('b');
    a.client.start();
    await until(() => named(a, 'welcome').length === 1, 'welcome');
    b.client.start();
    await tick();
    const entry = await b.outbox.append({ user: ALICE, replica: REPLICA, op: op(1), nowMs: 1000 });
    b.client.changed();
    await until(() => h.sockets[0].frames.length === 2, 'ops frame');
    h.sockets[0].deliver({ t: 'reject', op: entry.opId, reason: 'Denied' });
    await until(() => named(b, 'reject').length === 1, 'reject in the follower');
    assert.deepEqual(named(b, 'reject')[0].payload, { op: entry.opId, reason: 'Denied' });
  } finally {
    await h.cleanup();
  }
});

test('a broken connection reconnects and resends the ops still pending', async () => {
  const h = harness();
  try {
    const a = await h.tab('a');
    const entry = await a.outbox.append({ user: ALICE, replica: REPLICA, op: op(1), nowMs: 1000 });
    a.client.start();
    await until(() => h.sockets[0]?.frames.length === 2, 'first ops frame');
    h.sockets[0].drop();
    await until(() => h.sockets[1]?.frames.length === 2, 'ops frame after reconnect');
    assert.equal(h.sockets[1].sent()[0].t, 'hello');
    assert.deepEqual(h.sockets[1].sent()[1].ops.map((o) => o.id), [entry.opId]);
  } finally {
    await h.cleanup();
  }
});

test('Overloaded reconnects; the delay grows while no welcome arrives', async () => {
  const h = harness();
  try {
    h.server.refuse = 'Overloaded';
    const a = await h.tab('a');
    a.client.start();
    await until(() => h.sockets.length >= 4, 'several attempts');
    h.server.refuse = null;
    await until(() => named(a, 'welcome').length === 1, 'welcome at last');
    assert.equal(named(a, 'notice').length, 0);
  } finally {
    await h.cleanup();
  }
});

test('Superseded makes the tab step down and open no session until it leads again', async () => {
  const h = harness({ retryMs: 60000 });
  try {
    const a = await h.tab('a');
    const b = await h.tab('b');
    a.client.start();
    await until(() => named(a, 'welcome').length === 1, 'welcome');
    b.client.start();
    await tick();
    h.sockets[0].deliver({ t: 'error', sub: null, code: 'Superseded', retry_after_ms: null });
    await until(() => named(a, 'notice').length === 1, 'notice');
    assert.deepEqual(named(a, 'notice')[0].payload, { kind: 'superseded' });
    // The lock passed to b, which opens its own session; a opens none.
    await until(() => b.client.isLeader(), 'b leads');
    await until(() => named(b, 'welcome').length === 1, 'b welcome');
    assert.equal(a.client.isLeader(), false);
    assert.equal(h.sockets.length, 2);
  } finally {
    await h.cleanup();
  }
});

test('Unauthorized keeps the tab offline until reconnect() is called', async () => {
  const h = harness();
  try {
    h.server.refuse = 'Unauthorized';
    const a = await h.tab('a');
    a.client.start();
    await until(() => named(a, 'notice').length === 1, 'notice');
    assert.deepEqual(named(a, 'notice')[0].payload, { kind: 'session-refused', code: 'Unauthorized' });
    await tick(40);
    assert.equal(h.sockets.length, 1, 'no new attempt on its own');
    h.server.refuse = null;
    a.client.reconnect();
    await until(() => named(a, 'welcome').length === 1, 'welcome after reconnect()');
    assert.equal(h.sockets.length, 2);
  } finally {
    await h.cleanup();
  }
});

test('when the leader stops, the next tab takes over and drains the outbox', async () => {
  const h = harness();
  try {
    const a = await h.tab('a');
    const b = await h.tab('b');
    a.client.start();
    await until(() => named(a, 'welcome').length === 1, 'welcome');
    b.client.start();
    await tick();
    const entry = await a.outbox.append({ user: ALICE, replica: REPLICA, op: op(1), nowMs: 1000 });
    await a.client.stop();
    assert.equal(h.sockets[0].closed, true);
    await until(() => h.sockets[1]?.frames.length === 2, 'b sends the pending op');
    assert.deepEqual(h.sockets[1].sent()[1].ops.map((o) => o.id), [entry.opId]);
  } finally {
    await h.cleanup();
  }
});
