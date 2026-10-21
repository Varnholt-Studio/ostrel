// One sync session of the leader tab: one WebSocket from `Hello` to its close
// (ARCHITECTURE 5.4, 5.5, D32).
//
// The session
//
// 1. waits for the socket to open and sends `Hello` with what `identity` stored for this
//    device (token, replica, its secret, the last applied log position);
// 2. reads `Welcome`, stores a newly issued replica, and discards outbox entries of any other
//    (user, replica) pair, reporting how many for the visible notice of AC-43;
// 3. drains the outbox: it sends the oldest pending ops as one `ops` frame and waits until
//    every op of that batch has its outcome before it sends the next batch. So no more than
//    one batch (at most `ops_per_batch` ops) is ever unconfirmed, which also keeps the client
//    below `outbox_before_ack`;
// 4. ends when the socket closes, the server sends a fatal `error`, a received frame is
//    faulty, or `signal` aborts. It never reconnects; that is the caller's decision.
//
// Outcomes (5.4): an `ack` confirms every op of this replica in the batch up to its op that
// has no `reject` of its own; a `reject` decides its op; a `defer` leaves its op and every
// later op of the batch undecided, the session waits `retry_after_ms` and then sends again
// from the outbox. Confirmed and rejected ops leave the outbox.
//
// With `local` (the data store of `apply.mjs`), confirmed own ops are applied to the confirmed
// state and rejected ones recorded BEFORE they leave the outbox. A tab that dies in between
// keeps the op in the outbox; it is sent again, its replayed `Ack` lies at or below the mark,
// and it only leaves the outbox (5.4, D62). `Hello` then names the mark of the data store.
//
// The server sends the outcomes of a batch in a fixed order: every `reject`, then the `defer`,
// then the `ack` frames (D182). So a `reject` for an op that an `ack` of this session already
// confirmed breaks the protocol: the server never stored that op. The session remembers the
// ops its acks confirmed only while they can still get an outcome: when it sends the next
// batch, every remembered op lies at or below `applied_server_seq` and is forgotten, so the
// set never holds more than one batch (dana #2735). The session reports it
// through `onReject`, so the app can roll the op back, and ends with a fault; the caller
// reconnects and catches up. Any other outcome for an op that is not waiting in the current
// batch is ignored, as the protocol says for an `Ack` of an op that is no longer pending.
//
// This part of the client opens no subscription and makes no call, so an `error`, `evict` or
// `reset` that names a subscription, and a `snapshot` or `return`, concern no open request and
// are dropped (D155).

import { decodeOpId } from '../outbox/clock.mjs';
import { EventQueue, watchSocket } from './events.mjs';
import { DEFAULT_LIMITS, WireError, decodeServer, encodeClient, opBytes, utf8Length } from './frame.mjs';

/** Why a session ended. */
export const END = Object.freeze({
  /** The socket closed (network, server restart). Reconnecting is safe. */
  CLOSED: 'closed',
  /** `signal` aborted, for example because the tab stopped leading. */
  ABORTED: 'aborted',
  /** The server sent a fatal `error`; `code` names it. */
  FATAL: 'fatal',
  /** A received frame was faulty; the session closed the socket. `error` is the WireError. */
  FAULT: 'fault',
  /**
   * The oldest outbox entry cannot be sent (over the op or frame limit). Later ops cannot be
   * sent either, because `seq` must stay contiguous (5.5). `error` says why.
   */
  STUCK: 'stuck',
});

/**
 * Runs one session on a freshly created socket.
 *
 * @param {object} deps
 * @param {object} deps.socket  a WebSocket or an object with the same `send`, `close` and
 *   `onopen`, `onmessage`, `onclose`, `onerror` members
 * @param {import('../outbox/outbox.mjs').Outbox} deps.outbox
 * @param {{load: Function, saveReplica: Function}} deps.identity  `load()` resolves to
 *   `{ user, token, replica, replicaSecret, lastServerSeq }` (null for what is not known);
 *   `saveReplica({ replica, replicaSecret })` stores a newly issued replica
 * @param {Iterable<string>} [deps.models]  model names of the compiled schema
 * @param {AbortSignal} [deps.signal]
 * @param {EventQueue} [deps.queue]  lets the caller push `{ type: 'wake' }` after a local write
 * @param {object} [deps.local]  the data store of the replica (`apply.mjs`): `appliedServerSeq`,
 *   `applyAck` and `reject` are called before an op leaves the outbox
 * @param {object} [deps.hooks]  `onWelcome`, `onAck`, `onReject`, `onNotice`; each may be async
 * @param {() => number} [deps.now]
 * @param {Function} [deps.setTimer]
 * @param {Function} [deps.clearTimer]
 * @returns {Promise<{end: string, code?: string, error?: Error, welcomed: boolean}>}
 */
export async function runSession({
  socket,
  outbox,
  identity,
  models = [],
  signal = new AbortController().signal,
  queue = new EventQueue(),
  local = null,
  hooks = {},
  now = Date.now,
  setTimer = setTimeout,
  clearTimer = clearTimeout,
}) {
  const session = new Session({
    socket,
    outbox,
    identity,
    models: [...models],
    queue,
    local,
    hooks,
    now,
    setTimer,
    clearTimer,
  });
  const onAbort = () => queue.push({ type: 'abort' });
  if (signal.aborted) onAbort();
  else signal.addEventListener('abort', onAbort, { once: true });
  watchSocket(socket, queue);
  try {
    return await session.run();
  } finally {
    signal.removeEventListener('abort', onAbort);
    session.dispose();
  }
}

class Session {
  constructor(deps) {
    Object.assign(this, deps);
    this.limits = DEFAULT_LIMITS;
    this.user = null;
    this.replica = null;
    this.welcomed = false;
    // The batch on the wire: its outbox entries and per OpId 'waiting', 'confirmed',
    // 'rejected' or 'deferred'.
    this.batch = null;
    // OpId to the log position of the `ack` that confirmed it, to catch a later `reject` of
    // one of them (D182). Pruned with `appliedServerSeq` when the next batch is sent.
    this.confirmed = new Map();
    // The highest log position this session knows to be applied: the mark named in `Hello`,
    // then every `ack`.
    this.appliedServerSeq = null;
    this.deferTimer = null;
  }

  async run() {
    const opened = await this.queue.next();
    if (opened.type !== 'open') return this.finish(opened);

    const stored = await this.identity.load();
    this.user = stored.user;
    let lastServerSeq = stored.lastServerSeq ?? null;
    if (this.local !== null && stored.replica != null && this.user != null) {
      lastServerSeq = await this.local.appliedServerSeq({ user: this.user, replica: stored.replica });
    }
    this.appliedServerSeq = lastServerSeq;
    this.socket.send(
      encodeClient({
        t: 'hello',
        token: stored.token ?? null,
        replica: stored.replica ?? null,
        replica_secret: stored.replicaSecret ?? null,
        last_server_seq: lastServerSeq,
      }),
    );
    this.requested = stored.replica ?? null;

    for (;;) {
      const event = await this.queue.next();
      if (event.type === 'wake' || event.type === 'open') {
        // Handled below: a local write may have added ops to send.
      } else if (event.type === 'resume') {
        this.deferTimer = null;
      } else if (event.type === 'frame') {
        const ended = await this.receive(event.data);
        if (ended !== null) return ended;
      } else {
        return this.finish(event);
      }
      const stuck = await this.sendNextBatch();
      if (stuck !== null) return stuck;
    }
  }

  // Ends the session for a `closed` or `abort` event.
  finish(event) {
    if (event.type === 'abort') return this.end({ end: END.ABORTED });
    return this.end({ end: END.CLOSED });
  }

  end(result) {
    try {
      this.socket.close();
    } catch {
      // Already closed.
    }
    return { ...result, welcomed: this.welcomed };
  }

  dispose() {
    if (this.deferTimer !== null) this.clearTimer(this.deferTimer);
    this.deferTimer = null;
  }

  // Handles one received frame. Returns the session result if the session ends, else null.
  async receive(data) {
    let msg;
    try {
      msg = decodeServer(data, { models: this.models, frameBytes: this.limits.frame_bytes });
    } catch (error) {
      if (error instanceof WireError && !error.closesConnection) return null;
      return this.end({ end: END.FAULT, error });
    }
    if (!this.welcomed) return this.receiveFirst(msg);
    switch (msg.t) {
      case 'ack':
        await this.confirm(msg);
        return null;
      case 'reject':
        return this.reject(msg);
      case 'defer':
        this.defer(msg);
        return null;
      case 'error':
        // An error with a subscription id ends only that subscription; none is open here.
        if (msg.sub !== null) return null;
        return this.end({ end: END.FATAL, code: msg.code });
      case 'evict':
      case 'reset':
        // No subscription is open, so the frame concerns none (D155).
        return null;
      default:
        return this.end({ end: END.FAULT, error: unexpected(msg.t) });
    }
  }

  // The first message of the server must be `welcome` or a fatal `error`.
  async receiveFirst(msg) {
    if (msg.t === 'error' && msg.sub === null) {
      return this.end({ end: END.FATAL, code: msg.code });
    }
    if (msg.t !== 'welcome') {
      return this.end({ end: END.FAULT, error: unexpected(msg.t) });
    }
    if (this.requested !== null && msg.replica !== this.requested) {
      return this.end({
        end: END.FAULT,
        error: new WireError('Protocol', 'welcome names another replica than hello', 'replica'),
      });
    }
    this.welcomed = true;
    this.replica = msg.replica;
    this.limits = msg.limits;
    if (this.requested === null) {
      await this.identity.saveReplica({ replica: msg.replica, replicaSecret: msg.replica_secret });
    }
    const discarded = await this.outbox.discardForeign({ user: this.user, replica: this.replica });
    if (discarded > 0) {
      await this.call('onNotice', { kind: 'foreign-ops-discarded', count: discarded });
    }
    await this.call('onWelcome', {
      replica: msg.replica,
      limits: msg.limits,
      serverMs: msg.server_ms,
      clockOffsetMs: msg.server_ms - this.now(),
    });
    return null;
  }

  // Sends the oldest pending ops when no batch waits for its outcomes and no `defer` delay
  // runs. Returns the session result if the outbox head cannot be sent, else null.
  async sendNextBatch() {
    if (!this.welcomed || this.batch !== null || this.deferTimer !== null) return null;
    const pending = await this.outbox.pending({
      user: this.user,
      replica: this.replica,
      limit: this.limits.ops_per_batch,
    });
    if (pending.length === 0) return null;
    this.forgetSettled();
    let entries;
    let frame;
    try {
      entries = this.fitFrame(pending);
      frame = encodeClient(
        { t: 'ops', ops: entries.map(wireOpOf) },
        { frameBytes: this.limits.frame_bytes, opBytes: this.limits.op_bytes },
      );
    } catch (error) {
      if (error instanceof WireError) return this.end({ end: END.STUCK, error });
      throw error;
    }
    this.batch = { entries, state: new Map(entries.map((e) => [e.opId, 'waiting'])) };
    this.socket.send(frame);
    return null;
  }

  // The longest prefix of `pending` whose `ops` frame fits the frame limit.
  fitFrame(pending) {
    const empty = utf8Length(encodeClient({ t: 'ops', ops: [] }));
    let bytes = empty;
    const fitting = [];
    for (const entry of pending) {
      const size = opBytes(wireOpOf(entry));
      if (size > this.limits.op_bytes) {
        throw new WireError('OpTooLarge', `op ${entry.opId} of ${size} bytes is over the op limit`);
      }
      // One comma between two ops.
      const next = bytes + size + (fitting.length > 0 ? 1 : 0);
      if (next > this.limits.frame_bytes) break;
      bytes = next;
      fitting.push(entry);
    }
    if (fitting.length === 0) {
      throw new WireError('FrameTooLarge', `op ${pending[0].opId} does not fit one frame`);
    }
    return fitting;
  }

  async confirm(ack) {
    const acked = decodeOpId(ack.op);
    for (const [opId, hlc] of ack.restamped) {
      if (decodeOpId(opId).replica === this.replica) {
        await this.outbox.observe({ replica: this.replica, hlc });
      }
    }
    if (acked.replica !== this.replica) return;
    const confirmed = this.waiting().filter((entry) => entry.seq <= acked.seq);
    if (confirmed.length === 0) return;
    const ids = confirmed.map((entry) => entry.opId);
    if (this.local !== null) {
      await this.local.applyAck({
        user: this.user,
        replica: this.replica,
        seq: ack.seq,
        entries: confirmed,
        restamped: ack.restamped,
      });
    }
    await this.outbox.remove(ids);
    ids.forEach((opId) => {
      this.batch.state.set(opId, 'confirmed');
      this.confirmed.set(opId, ack.seq);
    });
    if (this.appliedServerSeq === null || ack.seq > this.appliedServerSeq) {
      this.appliedServerSeq = ack.seq;
    }
    this.closeBatchIfDecided();
    await this.call('onAck', {
      op: ack.op,
      seq: ack.seq,
      confirmed: ids,
      restamped: ack.restamped,
    });
  }

  // Returns the session result if the reject ends the session, else null.
  async reject({ op, reason }) {
    if (this.confirmed.has(op)) {
      // An `ack` came before this `reject` (D182 forbids that). The server is authoritative:
      // the op is rejected, and the session no longer trusts the order of its outcomes.
      this.confirmed.delete(op);
      await this.call('onReject', { op, reason });
      return this.end({
        end: END.FAULT,
        error: new WireError(
          'Protocol',
          `reject for op ${op}, which an ack already confirmed (D182)`,
          'op',
        ),
      });
    }
    const entry = this.waiting().find((waiting) => waiting.opId === op);
    if (entry === undefined) return null;
    if (this.local !== null) {
      await this.local.reject({ user: this.user, replica: this.replica, entry, reason });
    }
    await this.outbox.remove([op]);
    this.batch.state.set(op, 'rejected');
    this.closeBatchIfDecided();
    await this.call('onReject', { op, reason });
    return null;
  }

  defer({ op, retry_after_ms: delay }) {
    const deferred = decodeOpId(op);
    const later = this.waiting().filter(
      (entry) => decodeOpId(entry.opId).replica === deferred.replica && entry.seq >= deferred.seq,
    );
    if (later.length === 0) return;
    later.forEach((entry) => this.batch.state.set(entry.opId, 'deferred'));
    this.closeBatchIfDecided();
    if (this.deferTimer !== null) this.clearTimer(this.deferTimer);
    this.deferTimer = this.setTimer(() => this.queue.push({ type: 'resume' }), delay);
  }

  // Forgets confirmed ops at or below `appliedServerSeq`. Called before a new batch is sent:
  // the previous batch is decided then, so none of them can still get an outcome.
  forgetSettled() {
    for (const [opId, seq] of this.confirmed) {
      if (this.appliedServerSeq !== null && seq <= this.appliedServerSeq) this.confirmed.delete(opId);
    }
  }

  // Entries of the current batch that have no outcome yet.
  waiting() {
    if (this.batch === null) return [];
    return this.batch.entries.filter((entry) => this.batch.state.get(entry.opId) === 'waiting');
  }

  closeBatchIfDecided() {
    if (this.waiting().length === 0) this.batch = null;
  }

  async call(name, payload) {
    const hook = this.hooks[name];
    if (typeof hook === 'function') await hook(payload);
  }
}

// The wire form of an outbox entry. The entry's `op` holds `{ model, row, body }` in wire form
// (names, canonical values); id and stamp come from the outbox, which allocated them.
function wireOpOf(entry) {
  const { model, row, body } = entry.op;
  return { id: entry.opId, hlc: entry.hlc, model, row, body };
}

function unexpected(name) {
  return new WireError('Protocol', `unexpected message ${JSON.stringify(name)} at this point`);
}
