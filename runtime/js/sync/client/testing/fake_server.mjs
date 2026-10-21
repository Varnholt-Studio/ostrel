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

/**
 * The compiled schema of the Rust codec tests (`crates/ostrel_sync/tests/wire_messages.rs`) in
 * the form of the JS backend, plus the function list of T228: `Room` and `Person`, and the
 * server functions `rename(room: Room, name: Text) -> Bool`, `count() -> Int`,
 * `names() -> List[Text]` and `touch() -> Unit`.
 */
export const ROOMS_SCHEMA = Object.freeze({
  app: { name: 'rooms', auth: 'open' },
  enums: [{ name: 'Level', variants: ['member', 'admin'] }],
  models: [
    {
      name: 'Room',
      fields: [
        { name: 'name', type: 'Text', mode: 'plain', merge: 'default' },
        { name: 'topic', type: { optional: 'Text' }, mode: 'plain', merge: 'default' },
        { name: 'owner', type: { ref: 1 }, mode: 'plain', merge: 'default' },
        { name: 'members', type: { set: { ref: 1 } }, mode: 'plain', merge: 'default' },
        { name: 'roles', type: { map: [{ ref: 1 }, { enum: 0 }] }, mode: 'plain', merge: 'default' },
        { name: 'size', type: 'Int', mode: 'plain', merge: 'default' },
        { name: 'keys', type: { list: 'Int' }, mode: 'plain', merge: 'default' },
      ],
    },
    {
      name: 'Person',
      fields: [
        { name: 'handle', type: 'Text', mode: 'fixed', merge: 'default' },
        { name: 'home', type: { optional: { ref: 0 } }, mode: 'plain', merge: 'default' },
        { name: 'age', type: 'Int', mode: 'plain', merge: 'default' },
      ],
    },
  ],
  functions: [
    { name: 'rename', params: [{ ref: 0 }, 'Text'], result: 'Bool' },
    { name: 'count', params: [], result: 'Int' },
    { name: 'names', params: [], result: { list: 'Text' } },
    { name: 'touch', params: [], result: 'Unit' },
  ],
});
