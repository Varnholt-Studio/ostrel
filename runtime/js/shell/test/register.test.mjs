import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ACTIVATE_TIMEOUT_MS, RELOAD_FLAG, startShell } from '../register.mjs';
import { SKIP_WAITING } from '../worker.mjs';

function fakeHost({ supported = true, waiting = false, controller = true, activates = true, failRegister = false } = {}) {
  const listeners = new Set();
  const storage = new Map();
  const timers = [];
  const host = {
    reloads: 0,
    registered: [],
    posted: [],
    location: { reload: () => (host.reloads += 1) },
    sessionStorage: {
      getItem: (k) => (storage.has(k) ? storage.get(k) : null),
      setItem: (k, v) => storage.set(k, String(v)),
      removeItem: (k) => storage.delete(k),
    },
    storage,
    timers,
    setTimeout: (fn, ms) => {
      timers.push({ fn, ms });
      return timers.length;
    },
    clearTimeout: (id) => {
      if (timers[id - 1]) timers[id - 1].fn = null;
    },
    navigator: {},
  };
  if (supported) {
    const worker = {
      postMessage: (msg) => {
        host.posted.push(msg);
        if (activates) queueMicrotask(() => listeners.forEach((fn) => fn()));
        else queueMicrotask(() => timers.forEach((t) => t.fn && t.fn()));
      },
    };
    host.navigator.serviceWorker = {
      controller: controller ? {} : null,
      register: async (url, opts) => {
        if (failRegister) throw new Error('blocked');
        host.registered.push([url, opts]);
        return { waiting: waiting ? worker : null };
      },
      addEventListener: (type, fn) => type === 'controllerchange' && listeners.add(fn),
      removeEventListener: (type, fn) => type === 'controllerchange' && listeners.delete(fn),
    };
  }
  return host;
}

test('startShell reports browsers without service workers', async () => {
  assert.deepEqual(await startShell(fakeHost({ supported: false })), { status: 'unsupported' });
});

test('startShell registers the worker as a module that is always revalidated', async () => {
  const host = fakeHost();
  assert.equal((await startShell(host, 'sw.js')).status, 'ready');
  assert.deepEqual(host.registered, [['sw.js', { type: 'module', updateViaCache: 'none' }]]);
  assert.equal(host.reloads, 0);
});

test('startShell keeps running when registration fails', async () => {
  const result = await startShell(fakeHost({ failRegister: true }));
  assert.equal(result.status, 'failed');
  assert.match(String(result.error), /blocked/);
});

test('a waiting build on first install (no controller) is not forced', async () => {
  const host = fakeHost({ waiting: true, controller: false });
  assert.equal((await startShell(host)).status, 'ready');
  assert.deepEqual(host.posted, []);
});

test('a waiting build is activated on the next load with exactly one reload', async () => {
  const host = fakeHost({ waiting: true });
  assert.equal((await startShell(host)).status, 'reloading');
  assert.deepEqual(host.posted, [{ type: SKIP_WAITING }]);
  assert.equal(host.reloads, 1);
  assert.equal(host.storage.get(RELOAD_FLAG), '1');
  // After the reload the build is active, no worker is waiting and the flag is cleared.
  const after = fakeHost();
  after.storage.set(RELOAD_FLAG, '1');
  assert.equal((await startShell(after)).status, 'ready');
  assert.equal(after.storage.has(RELOAD_FLAG), false);
});

test('a build that does not take control in time keeps the running build', async () => {
  const host = fakeHost({ waiting: true, activates: false });
  assert.equal((await startShell(host)).status, 'update-stuck');
  assert.equal(host.reloads, 0);
  assert.equal(host.timers[0].ms, ACTIVATE_TIMEOUT_MS);
});

test('no reload loop when the update is still waiting after a reload', async () => {
  const host = fakeHost({ waiting: true });
  host.storage.set(RELOAD_FLAG, '1');
  assert.equal((await startShell(host)).status, 'update-stuck');
  assert.equal(host.reloads, 0);
  assert.deepEqual(host.posted, []);
  assert.equal(host.storage.has(RELOAD_FLAG), false);
});

test('blocked session storage does not break startup', async () => {
  const host = fakeHost({ waiting: true });
  host.sessionStorage = {
    getItem: () => {
      throw new Error('denied');
    },
    setItem: () => {
      throw new Error('denied');
    },
    removeItem: () => {
      throw new Error('denied');
    },
  };
  assert.equal((await startShell(host)).status, 'reloading');
});
