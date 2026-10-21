// A stand in for the WebSocket and the server side of it, for Node tests of the sync client.
// It speaks frames, not messages: tests write server frames with `serverFrame` (canonical,
// like the Rust encoder) and read what the client sent with `sent()`.

import { encode } from '../../../crdt/canon/canon.mjs';
import { LIMIT_KEYS } from '../frame.mjs';

/** One fake WebSocket. `open()`, `deliver()` and `drop()` act as the server. */
export class FakeSocket {
  constructor() {
    this.frames = [];
    this.closed = false;
    this.onopen = null;
    this.onmessage = null;
    this.onclose = null;
    this.onerror = null;
  }

  send(text) {
    if (this.closed) throw new Error('send on a closed socket');
    this.frames.push(text);
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    queueMicrotask(() => this.onclose?.({}));
  }

  /** The frames the client sent, parsed. */
  sent() {
    return this.frames.map((frame) => JSON.parse(frame));
  }

  open() {
    this.onopen?.({});
  }

  /** Delivers a server frame: an object (written canonically) or raw text. */
  deliver(frame) {
    const data = typeof frame === 'string' ? frame : serverFrame(frame);
    this.onmessage?.({ data });
  }

  /** The connection breaks. */
  drop() {
    this.closed = true;
    this.onclose?.({});
  }
}

/** A canonical server frame from `{ t, ...fields }`. */
export function serverFrame({ t, ...fields }) {
  return encode({ ...fields, t, v: 0 });
}

/** A full limits table; every value 1000 unless overridden. */
export function limits(overrides = {}) {
  return Object.fromEntries(LIMIT_KEYS.map((key) => [key, overrides[key] ?? 1000]));
}

/** A `welcome` frame for `replica`. */
export function welcome(replica, { secret = null, serverMs = 5000, limitOverrides = {} } = {}) {
  return {
    t: 'welcome',
    replica,
    replica_secret: secret,
    server_ms: serverMs,
    limits: limits(limitOverrides),
  };
}

/** An identity store in memory. */
export function memoryIdentity(initial) {
  const state = {
    user: 'user-alice',
    token: null,
    replica: null,
    replicaSecret: null,
    lastServerSeq: null,
    ...initial,
  };
  return {
    state,
    async load() {
      return { ...state };
    },
    async saveReplica({ replica, replicaSecret }) {
      state.replica = replica;
      state.replicaSecret = replicaSecret;
    },
  };
}

/** Resolves once `predicate()` is true; fails after `timeoutMs`. */
export async function until(predicate, what, timeoutMs = 1000) {
  const start = Date.now();
  while (!predicate()) {
    if (Date.now() - start > timeoutMs) throw new Error(`timed out waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 2));
  }
}
