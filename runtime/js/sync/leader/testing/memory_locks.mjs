// In-memory stand in for the subset of the Web Locks API (`navigator.locks.request`) used by
// the tab leader, for Node tests. Only exclusive locks are supported. Requests are granted in
// FIFO order per lock name, as in browsers.
//
// `createMemoryLocks()` returns a shared manager. `forTab(id)` gives one tab its own view with
// `request(name, [options], callback)` and `kill()`, which simulates a tab that closes or
// crashes: its held locks are released and its queued requests are dropped without settling.

export function createMemoryLocks() {
  // name -> { holder: entry | null, queue: entry[] }
  const locks = new Map();

  function state(name) {
    let s = locks.get(name);
    if (!s) {
      s = { holder: null, queue: [] };
      locks.set(name, s);
    }
    return s;
  }

  function grantNext(name) {
    const s = state(name);
    if (s.holder || s.queue.length === 0) return;
    const entry = s.queue.shift();
    entry.detachAbort();
    grant(name, entry);
  }

  function grant(name, entry) {
    const s = state(name);
    s.holder = entry;
    let result;
    try {
      result = Promise.resolve(entry.callback({ name, mode: 'exclusive' }));
    } catch (err) {
      result = Promise.reject(err);
    }
    result.then(
      (value) => release(name, entry, () => entry.resolve(value)),
      (err) => release(name, entry, () => entry.reject(err)),
    );
  }

  function release(name, entry, settle) {
    if (entry.dead) return;
    const s = state(name);
    if (s.holder === entry) s.holder = null;
    settle();
    grantNext(name);
  }

  function abortError() {
    const err = new Error('The lock request was aborted');
    err.name = 'AbortError';
    return err;
  }

  function forTab(tabId) {
    const mine = new Set();

    function request(name, options, callback) {
      if (typeof options === 'function') {
        callback = options;
        options = {};
      }
      if (typeof name !== 'string') throw new TypeError('lock name must be a string');
      if (typeof callback !== 'function') throw new TypeError('callback must be a function');
      const mode = options.mode ?? 'exclusive';
      if (mode !== 'exclusive') throw new TypeError('only exclusive locks are supported');
      const { signal, ifAvailable = false } = options;
      return new Promise((resolve, reject) => {
        if (signal?.aborted) {
          reject(abortError());
          return;
        }
        const s = state(name);
        const entry = { tabId, callback, resolve, reject, dead: false, detachAbort: () => {} };
        mine.add({ name, entry });
        if (!s.holder && s.queue.length === 0) {
          grant(name, entry);
          return;
        }
        if (ifAvailable) {
          Promise.resolve()
            .then(() => callback(null))
            .then(resolve, reject);
          return;
        }
        s.queue.push(entry);
        if (signal) {
          const onAbort = () => {
            const i = s.queue.indexOf(entry);
            if (i >= 0) {
              s.queue.splice(i, 1);
              reject(abortError());
            }
          };
          signal.addEventListener('abort', onAbort, { once: true });
          entry.detachAbort = () => signal.removeEventListener('abort', onAbort);
        }
      });
    }

    function kill() {
      for (const { name, entry } of mine) {
        const s = state(name);
        entry.dead = true;
        entry.detachAbort();
        const i = s.queue.indexOf(entry);
        if (i >= 0) s.queue.splice(i, 1);
        if (s.holder === entry) {
          s.holder = null;
          grantNext(name);
        }
      }
      mine.clear();
    }

    return { request, kill };
  }

  function holder(name) {
    return locks.get(name)?.holder?.tabId ?? null;
  }

  return { forTab, holder };
}
