// In memory stand ins for Cache Storage, the network and a service worker scope, used by the
// shell tests. Only the parts of the browser API that the shell calls are implemented.

export class FakeCache {
  constructor() {
    this.entries = new Map();
  }
  async match(key) {
    const url = typeof key === 'string' ? key : key.url;
    const hit = this.entries.get(url);
    return hit ? hit.clone() : undefined;
  }
  async put(key, response) {
    const url = typeof key === 'string' ? key : key.url;
    this.entries.set(url, response.clone());
  }
  async keys() {
    return [...this.entries.keys()];
  }
}

export class FakeCacheStorage {
  constructor() {
    this.store = new Map();
    this.failPut = false;
  }
  async has(name) {
    return this.store.has(name);
  }
  async open(name) {
    if (!this.store.has(name)) {
      const cache = new FakeCache();
      if (this.failPut) {
        cache.put = async () => {
          throw new Error('quota exceeded');
        };
      }
      this.store.set(name, cache);
    }
    return this.store.get(name);
  }
  async delete(name) {
    return this.store.delete(name);
  }
  async keys() {
    return [...this.store.keys()];
  }
}

/** A network that serves `files` (url -> body) and records every request. */
export function fakeNetwork(files) {
  const net = {
    online: true,
    calls: [],
    overrides: new Map(),
    fetch: async (input) => {
      const url = typeof input === 'string' ? input : input.url;
      net.calls.push(url);
      if (!net.online) throw new TypeError('Failed to fetch');
      if (net.overrides.has(url)) return net.overrides.get(url)();
      if (!files.has(url)) return new Response('not found', { status: 404 });
      return new Response(files.get(url), { status: 200 });
    },
  };
  return net;
}

export function request(url, { method = 'GET', mode = 'cors' } = {}) {
  return { url, method, mode };
}

/** Minimal service worker global scope that records listeners and pending promises. */
export function fakeWorkerScope({ caches, fetch, scope }) {
  const listeners = new Map();
  const self = {
    caches,
    fetch,
    registration: { scope },
    skipped: 0,
    skipWaiting: async () => {
      self.skipped += 1;
    },
    addEventListener: (type, fn) => listeners.set(type, fn),
    async dispatch(type, extra = {}) {
      const pending = [];
      let responded = null;
      const event = {
        ...extra,
        waitUntil: (p) => pending.push(p),
        respondWith: (p) => {
          responded = p;
        },
      };
      listeners.get(type)(event);
      await Promise.all(pending);
      return responded;
    },
  };
  return self;
}
