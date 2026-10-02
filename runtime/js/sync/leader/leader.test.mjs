// Tests for the tab leader (ARCHITECTURE 5.4, D32). Tabs are simulated in one process: each
// tab gets its own view of a shared in-memory lock manager and its own BroadcastChannel
// instance on a per-test channel name.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { createTabLeader, LOCK_NAME } from './leader.mjs';
import { createMemoryLocks } from './testing/memory_locks.mjs';

let channelSeq = 0;

function harness() {
  const locks = createMemoryLocks();
  const channelName = `ostrel-sync-test-${process.pid}-${channelSeq++}`;
  const tabs = [];
  function tab(id, hooks = {}) {
    const lockView = locks.forTab(id);
    const events = [];
    const leader = createTabLeader({
      tabId: id,
      locks: lockView,
      openChannel: (name) => new BroadcastChannel(name),
      channelName,
      onLead: hooks.onLead ?? (async ({ signal }) => {
        events.push('lead');
        await new Promise((resolve) => signal.addEventListener('abort', resolve, { once: true }));
        events.push('resign');
      }),
      onMessage: hooks.onMessage ?? ((msg) => events.push(msg)),
      onError: hooks.onError ?? ((err) => events.push({ error: err })),
    });
    const handle = { id, leader, events, lockView };
    tabs.push(handle);
    return handle;
  }
  async function cleanup() {
    for (const t of tabs) await t.leader.stop();
  }
  return { locks, tab, cleanup };
}

const tick = (ms = 5) => new Promise((resolve) => setTimeout(resolve, ms));

async function until(predicate, what, timeoutMs = 1000) {
  const start = Date.now();
  while (!predicate()) {
    if (Date.now() - start > timeoutMs) throw new Error(`timed out waiting for ${what}`);
    await tick();
  }
}

test('uses the lock name fixed by the architecture', () => {
  assert.equal(LOCK_NAME, 'ostrel-sync');
});

test('the first tab becomes leader and runs onLead', async () => {
  const h = harness();
  try {
    const a = h.tab('a');
    a.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    assert.deepEqual(a.events, ['lead']);
    assert.equal(h.locks.holder(LOCK_NAME), 'a');
  } finally {
    await h.cleanup();
  }
});

test('exactly one of several tabs leads at any time', async () => {
  const h = harness();
  try {
    const tabs = ['a', 'b', 'c'].map((id) => h.tab(id));
    for (const t of tabs) t.leader.start();
    await until(() => tabs.some((t) => t.leader.isLeader()), 'a leader');
    await tick(20);
    assert.equal(tabs.filter((t) => t.leader.isLeader()).length, 1);
    assert.equal(tabs.filter((t) => t.events.includes('lead')).length, 1);
  } finally {
    await h.cleanup();
  }
});

test('followers learn the leader id, also when they join late', async () => {
  const h = harness();
  try {
    const a = h.tab('a');
    a.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    const b = h.tab('b');
    b.leader.start();
    await until(() => b.leader.leaderId() === 'a', 'b to learn leader a');
    assert.equal(b.leader.isLeader(), false);
    assert.equal(a.leader.leaderId(), 'a');
  } finally {
    await h.cleanup();
  }
});

test('when the leader stops, a waiting tab takes over', async () => {
  const h = harness();
  try {
    const a = h.tab('a');
    const b = h.tab('b');
    a.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    b.leader.start();
    await until(() => b.leader.leaderId() === 'a', 'b to see a');
    await a.leader.stop();
    assert.deepEqual(a.events, ['lead', 'resign']);
    await until(() => b.leader.isLeader(), 'b to take over');
    assert.equal(h.locks.holder(LOCK_NAME), 'b');
    await until(() => b.leader.leaderId() === 'b', 'b to know itself');
  } finally {
    await h.cleanup();
  }
});

test('a tab that dies without stopping releases the lock to the next tab', async () => {
  const h = harness();
  try {
    // The tab dies without its leader task noticing; only the lock manager sees it go.
    const a = h.tab('a', {
      onLead: ({ signal }) => new Promise((resolve) => signal.addEventListener('abort', resolve)),
    });
    const b = h.tab('b');
    a.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    b.leader.start();
    await tick(10);
    assert.equal(b.leader.isLeader(), false);
    a.lockView.kill();
    await until(() => b.leader.isLeader(), 'b to take over after crash');
    assert.deepEqual(b.events.filter((e) => e === 'lead'), ['lead']);
  } finally {
    await h.cleanup();
  }
});

test('post from a follower reaches only the leader', async () => {
  const h = harness();
  try {
    const a = h.tab('a');
    const b = h.tab('b');
    const c = h.tab('c');
    a.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    b.leader.start();
    c.leader.start();
    await until(() => b.leader.leaderId() === 'a' && c.leader.leaderId() === 'a', 'followers');
    b.leader.post({ kind: 'outbox-changed' });
    await until(() => a.events.some((e) => e.kind === 'up'), 'a to receive the post');
    const got = a.events.find((e) => e.kind === 'up');
    assert.deepEqual(got, { kind: 'up', from: 'b', body: { kind: 'outbox-changed' } });
    await tick(10);
    assert.equal(c.events.some((e) => e.kind === 'up'), false);
    assert.equal(b.events.some((e) => e.kind === 'up'), false);
  } finally {
    await h.cleanup();
  }
});

test('post on the leader itself is delivered locally', async () => {
  const h = harness();
  try {
    const a = h.tab('a');
    a.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    a.leader.post({ n: 1 });
    await until(() => a.events.some((e) => e.kind === 'up'), 'local delivery');
    assert.deepEqual(a.events.find((e) => e.kind === 'up'), { kind: 'up', from: 'a', body: { n: 1 } });
  } finally {
    await h.cleanup();
  }
});

test('broadcast from the leader reaches every follower but not the leader', async () => {
  const h = harness();
  try {
    const a = h.tab('a');
    const b = h.tab('b');
    const c = h.tab('c');
    for (const t of [a, b, c]) t.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    await until(() => b.leader.leaderId() === 'a' && c.leader.leaderId() === 'a', 'followers');
    assert.equal(a.leader.broadcast({ state: 7 }), true);
    await until(
      () => [b, c].every((t) => t.events.some((e) => e.kind === 'down')),
      'followers to receive the broadcast',
    );
    for (const t of [b, c]) {
      assert.deepEqual(t.events.find((e) => e.kind === 'down'), {
        kind: 'down',
        from: 'a',
        body: { state: 7 },
      });
    }
    await tick(10);
    assert.equal(a.events.some((e) => e.kind === 'down'), false);
  } finally {
    await h.cleanup();
  }
});

test('broadcast from a follower is refused', async () => {
  const h = harness();
  try {
    const a = h.tab('a');
    const b = h.tab('b');
    a.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    b.leader.start();
    await until(() => b.leader.leaderId() === 'a', 'b to see a');
    assert.equal(b.leader.broadcast({ state: 1 }), false);
    await tick(10);
    assert.equal(a.events.some((e) => e.kind === 'down'), false);
  } finally {
    await h.cleanup();
  }
});

test('malformed channel messages are ignored', async () => {
  const h = harness();
  try {
    const a = h.tab('a');
    a.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    const raw = new BroadcastChannel(a.leader.channelName());
    try {
      for (const junk of [null, 42, 'up', [], { t: 'up' }, { t: 'up', from: 5, body: 1 },
        { t: 'nope', from: 'x' }, { t: 'leader' }, { t: 'up', from: 'x' }]) {
        raw.postMessage(junk);
      }
      await tick(20);
    } finally {
      raw.close();
    }
    assert.deepEqual(a.events, ['lead']);
    assert.equal(a.leader.leaderId(), 'a');
  } finally {
    await h.cleanup();
  }
});

test('a stale resign of a former leader does not clear the current leader', async () => {
  const h = harness();
  try {
    const a = h.tab('a');
    const b = h.tab('b');
    a.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    b.leader.start();
    await until(() => b.leader.leaderId() === 'a', 'b to see a');
    const raw = new BroadcastChannel(b.leader.channelName());
    try {
      raw.postMessage({ t: 'resign', from: 'zombie' });
      await tick(20);
    } finally {
      raw.close();
    }
    assert.equal(b.leader.leaderId(), 'a');
  } finally {
    await h.cleanup();
  }
});

test('a failing onLead is reported and leadership is handed on', async () => {
  const h = harness();
  try {
    const a = h.tab('a', { onLead: async () => { throw new Error('socket failed'); } });
    const b = h.tab('b');
    a.leader.start();
    await until(() => a.events.some((e) => e.error), 'a to report the error');
    assert.equal(a.events.find((e) => e.error).error.message, 'socket failed');
    b.leader.start();
    await until(() => b.leader.isLeader() || a.leader.isLeader(), 'someone to lead');
    await tick(20);
    assert.equal([a, b].filter((t) => t.leader.isLeader()).length, 1);
  } finally {
    await h.cleanup();
  }
});

test('throwing onError does not leave a second leader', async () => {
  const h = harness();
  const unhandled = [];
  const onUnhandled = (err) => unhandled.push(err);
  process.on('unhandledRejection', onUnhandled);
  try {
    const a = h.tab('a', {
      onLead: async () => { throw new Error('socket failed'); },
      onError: (err) => {
        a.events.push({ error: err });
        throw new Error('onError failed');
      },
    });
    const b = h.tab('b');
    a.leader.start();
    await until(() => a.events.some((e) => e.error), 'a to report the error');
    b.leader.start();
    await until(() => h.locks.holder(LOCK_NAME) === 'b', 'b to hold the lock');
    await until(() => b.leader.isLeader(), 'b to lead');
    await tick(20);
    assert.equal(a.leader.isLeader(), false);
    assert.equal(a.leader.broadcast('x'), false);
    assert.equal([a, b].filter((t) => t.leader.isLeader()).length, 1);
    assert.deepEqual(unhandled, []);
  } finally {
    process.off('unhandledRejection', onUnhandled);
    await h.cleanup();
  }
});

test('a throwing onError on the message path does not break the channel', async () => {
  const h = harness();
  const errors = [];
  try {
    const a = h.tab('a', {
      onMessage: (msg) => {
        if (msg.body === 'bad') throw new Error('handler failed');
        a.events.push(msg);
      },
      onError: (err) => {
        errors.push(err);
        throw new Error('onError failed');
      },
    });
    const b = h.tab('b');
    a.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    b.leader.start();
    b.leader.post('bad');
    await until(() => errors.length === 1, 'the handler error');
    b.leader.post('good');
    await until(() => a.events.some((e) => e.body === 'good'), 'the next message');
    assert.equal(a.leader.isLeader(), true);
  } finally {
    await h.cleanup();
  }
});

test('stop before the lock is granted withdraws the request', async () => {
  const h = harness();
  try {
    const a = h.tab('a');
    const b = h.tab('b');
    a.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    b.leader.start();
    await tick(10);
    await b.leader.stop();
    await a.leader.stop();
    await tick(10);
    assert.equal(h.locks.holder(LOCK_NAME), null);
    assert.equal(b.events.includes('lead'), false);
  } finally {
    await h.cleanup();
  }
});

test('start and stop are idempotent', async () => {
  const h = harness();
  try {
    const a = h.tab('a');
    a.leader.start();
    a.leader.start();
    await until(() => a.leader.isLeader(), 'a to lead');
    assert.deepEqual(a.events, ['lead']);
    await a.leader.stop();
    await a.leader.stop();
    assert.equal(a.leader.isLeader(), false);
    assert.equal(a.leader.post({}), false);
    assert.throws(() => a.leader.start(), /stopped/);
  } finally {
    await h.cleanup();
  }
});

test('rejects missing dependencies and bad tab ids', () => {
  const ok = {
    tabId: 't',
    locks: createMemoryLocks().forTab('t'),
    openChannel: () => new BroadcastChannel('x'),
    onLead: async () => {},
  };
  assert.throws(() => createTabLeader({ ...ok, tabId: '' }), TypeError);
  assert.throws(() => createTabLeader({ ...ok, tabId: 3 }), TypeError);
  assert.throws(() => createTabLeader({ ...ok, locks: undefined }), TypeError);
  assert.throws(() => createTabLeader({ ...ok, openChannel: undefined }), TypeError);
  assert.throws(() => createTabLeader({ ...ok, onLead: undefined }), TypeError);
});
