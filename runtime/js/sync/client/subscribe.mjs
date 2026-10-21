// Subscriptions of one sync session (ARCHITECTURE 5.4, 5.6, 5.9, D101, D106, D155, D159).
//
// A `SubscriptionTable` belongs to one session, from its `Welcome` to its end. It
//
// * gives every subscription an id of its own: ids rise per session and are never used
//   twice, also not after `unsubscribe`, an `error` with the id or a `reset` (D155), so a page
//   that still stands in the session stream is never read against another model;
// * writes `subscribe`, `held` and `unsubscribe` frames and hands them to `send`;
// * turns the server messages that name a subscription into events for the store, and drops
//   those for an id that is not open (D155): `snapshot`, `evict`, `reset`, and `error` with a
//   subscription id.
//
// Events (`receive` returns one, or null when the message concerns no open subscription):
//
// * `{ kind: 'snapshot', sub, model, rows, seq, last }`: rows of the subscription, read at
//   log position `seq` (D151); `last` ends its snapshot.
// * `{ kind: 'evict', sub, model, rows }`: row ids that left the reader's scope.
// * `{ kind: 'reset', sub, next }`: the server dropped the subscription. The store drops its
//   rows; the table has already subscribed the same query again under the id `next` (5.4).
// * `{ kind: 'retry', sub, next }`: the server refused the `subscribe` with `RateLimited`;
//   the table subscribes the same query again under the id `next` after `retry_after_ms`
//   (D159). Until then `next` is reserved and receives nothing.
// * `{ kind: 'failed', sub, code }`: the server refused the subscription for good
//   (`TooManySubscriptions`, `FilterTooLarge`, `FilterNotReadable` or `Protocol` for an invalid
//   query, D106). The id is closed.
//
// Server `ops` name no subscription; the session passes them to the store as they are.
//
// The queries the store wants across sessions, and the row ids it holds for each, are the
// store's: on a new session it opens each query again here and sends its `held` pages
// (5.4 "Offline and reconnect").

import { DEFAULT_LIMITS, WireError, encodeClient } from './frame.mjs';

export class SubscriptionTable {
  /**
   * @param {object} deps
   * @param {import('./value.mjs').WireSchema} deps.schema
   * @param {(frame: string) => void} deps.send  hands a frame to the socket
   * @param {object} [deps.limits]  the limits of `Welcome` (`ids_per_page`, `frame_bytes`)
   * @param {Function} [deps.setTimer]
   * @param {Function} [deps.clearTimer]
   */
  constructor({ schema, send, limits = {}, setTimer = setTimeout, clearTimer = clearTimeout }) {
    this.schema = schema;
    this.send = send;
    this.idsPerPage = limits.ids_per_page ?? DEFAULT_LIMITS.ids_per_page;
    this.frameBytes = limits.frame_bytes ?? DEFAULT_LIMITS.frame_bytes;
    this.setTimer = setTimer;
    this.clearTimer = clearTimer;
    this.nextId = 0;
    // Open subscriptions: id -> { query }.
    this.open = new Map();
    // Subscriptions waiting to be sent again after `RateLimited`: id -> timer.
    this.retries = new Map();
    this.closed = false;
  }

  /**
   * Subscribes to `query` and returns the new subscription id.
   *
   * @param {object} query  the wire form `{ model, filter, order, limit, after }` (5.4)
   * @returns {number}
   * @throws {WireError} when the query does not fit the schema (a defect of the caller)
   */
  subscribe(query) {
    if (this.closed) throw new Error('the session of this table has ended');
    const id = this.allocate();
    this.sendSubscribe(id, query);
    return id;
  }

  /**
   * Sends the row ids the store holds for subscription `sub`, in pages of at most
   * `ids_per_page` ids; the last page carries the last page flag, also when there is no id.
   */
  held(sub, rows) {
    if (!this.open.has(sub)) return;
    const ids = [...rows];
    for (let start = 0; ; start += this.idsPerPage) {
      const page = ids.slice(start, start + this.idsPerPage);
      const last = start + this.idsPerPage >= ids.length;
      this.send(encodeClient({ t: 'held', sub, rows: page, last }, { frameBytes: this.frameBytes }));
      if (last) return;
    }
  }

  /** Ends subscription `sub`. Its id is never used again; later frames for it are dropped. */
  unsubscribe(sub) {
    if (this.retries.has(sub)) {
      this.clearTimer(this.retries.get(sub));
      this.retries.delete(sub);
      this.open.delete(sub);
      return;
    }
    if (!this.open.delete(sub)) return;
    this.send(encodeClient({ t: 'unsubscribe', sub }));
  }

  /** The model of an open subscription, or null (for `decodeServer`). */
  modelOf(sub) {
    if (this.retries.has(sub)) return null;
    return this.open.get(sub)?.query.model ?? null;
  }

  /**
   * Turns a decoded server message into an event, or null when it names no open subscription.
   *
   * @param {object} msg  a decoded `snapshot`, `evict`, `reset` or `error` with a subscription
   * @throws {WireError} for an `evict` that names another model than its subscription (a
   *   fault of the server; the session closes the connection)
   */
  receive(msg) {
    // An id reserved for a retry has not been sent yet, so nothing can name it.
    if (this.retries.has(msg.sub)) return null;
    const entry = this.open.get(msg.sub);
    if (entry === undefined) return null;
    switch (msg.t) {
      case 'snapshot':
        return {
          kind: 'snapshot',
          sub: msg.sub,
          model: msg.model,
          rows: msg.rows,
          seq: msg.seq,
          last: msg.last,
        };
      case 'evict':
        if (msg.model !== entry.query.model) {
          throw new WireError('Protocol', `evict for subscription ${msg.sub} names another model`, 'model');
        }
        return { kind: 'evict', sub: msg.sub, model: msg.model, rows: msg.rows };
      case 'reset': {
        this.open.delete(msg.sub);
        const next = this.allocate();
        this.sendSubscribe(next, entry.query);
        return { kind: 'reset', sub: msg.sub, next };
      }
      case 'error':
        return this.refused(msg, entry.query);
      default:
        throw new TypeError(`message ${msg.t} names no subscription`);
    }
  }

  /** The session ended: pending retries stop and every id is closed. */
  close() {
    this.closed = true;
    for (const timer of this.retries.values()) this.clearTimer(timer);
    this.retries.clear();
    this.open.clear();
  }

  // An `error` with this subscription's id: the server did not take the `subscribe`.
  refused({ sub, code, retry_after_ms: delay }, query) {
    this.open.delete(sub);
    if (code !== 'RateLimited') return { kind: 'failed', sub, code };
    const next = this.allocate();
    this.open.set(next, { query });
    this.retries.set(
      next,
      this.setTimer(() => {
        this.retries.delete(next);
        if (!this.closed) this.sendSubscribe(next, query);
      }, delay),
    );
    return { kind: 'retry', sub, next };
  }

  allocate() {
    const id = this.nextId;
    this.nextId += 1;
    return id;
  }

  sendSubscribe(id, query) {
    const frame = encodeClient(
      { t: 'subscribe', sub: id, query },
      { schema: this.schema, frameBytes: this.frameBytes },
    );
    this.open.set(id, { query });
    this.send(frame);
  }
}
