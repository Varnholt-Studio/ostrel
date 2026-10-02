// Durable outbox of local ops, shared by all tabs of one origin (ARCHITECTURE 5.4, D32).
//
// Every local edit is appended here in ONE IndexedDB transaction that also allocates the op's
// seq and advances the replica's HLC. IndexedDB runs overlapping read/write transactions one
// after another, also across tabs, so two tabs can never allocate the same OpId, and a failed
// append leaves neither a gap in seq nor an entry behind.
//
// The outbox does not talk to the network and does not decide which tab drains it. The tab
// leader (runtime/js/sync/leader/) reads `pending`, sends the ops and calls `remove` once the
// server has acknowledged them.
//
// Stores:
//   clocks   key: ReplicaId hex      value: { seq, wallMs, counter }
//   entries  key: OpId hex           value: { opId, user, replica, seq, hlc, op }
// OpId hex starts with the replica, so key order is (replica, seq) order.

import {
  checkReplica,
  decodeHlc,
  decodeOpId,
  encodeHlc,
  encodeOpId,
  initialClock,
  nextLocal,
  observe,
  resumeAfter,
} from './clock.mjs';

export const DEFAULT_DB_NAME = 'ostrel-outbox';
const DB_VERSION = 1;
const CLOCKS = 'clocks';
const ENTRIES = 'entries';

/**
 * Opens (and on first use creates) the outbox database.
 * `indexedDB` and `IDBKeyRange` are passed in so the module runs against the browser's
 * IndexedDB and, in tests, against a stand in.
 */
export function openOutbox({ indexedDB, IDBKeyRange, name = DEFAULT_DB_NAME }) {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(name, DB_VERSION);
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(CLOCKS)) db.createObjectStore(CLOCKS);
      if (!db.objectStoreNames.contains(ENTRIES)) db.createObjectStore(ENTRIES);
    };
    request.onsuccess = () => resolve(new Outbox(request.result, IDBKeyRange));
    request.onerror = () => reject(request.error);
    request.onblocked = () => reject(new Error(`outbox database ${name} is blocked`));
  });
}

export class Outbox {
  #db;
  #keyRange;

  constructor(db, keyRange) {
    this.#db = db;
    this.#keyRange = keyRange;
  }

  /**
   * Appends one local op for (user, replica). Allocates the next seq and HLC in the same
   * transaction. Resolves to the stored entry once the transaction has committed.
   */
  async append({ user, replica, op, nowMs = Date.now() }) {
    checkUser(user);
    checkReplica(replica);
    if (op === null || typeof op !== 'object') {
      throw new TypeError('op must be an object');
    }
    return this.#transaction([CLOCKS, ENTRIES], 'readwrite', (tx, done, step) => {
      const clocks = tx.objectStore(CLOCKS);
      const read = clocks.get(replica);
      read.onsuccess = step(() => {
        const next = nextLocal(read.result ?? initialClock(), nowMs);
        const entry = {
          opId: encodeOpId(replica, next.seq),
          user,
          replica,
          seq: next.seq,
          hlc: encodeHlc({ wallMs: next.wallMs, counter: next.counter, replica }),
          op,
        };
        clocks.put(next.clock, replica);
        tx.objectStore(ENTRIES).put(entry, entry.opId);
        done(entry);
      });
    });
  }

  /**
   * Returns up to `limit` entries of (user, replica) in seq order, oldest first.
   * Entries of other users are never returned; call `discardForeign` before draining.
   */
  async pending({ user, replica, limit = 1000 }) {
    checkUser(user);
    checkReplica(replica);
    if (!Number.isSafeInteger(limit) || limit < 1) {
      throw new RangeError('limit must be a positive integer');
    }
    const range = this.#keyRange.bound(replica + '00000000', replica + 'ffffffff');
    return this.#transaction([ENTRIES], 'readonly', (tx, done, step) => {
      const read = tx.objectStore(ENTRIES).getAll(range, limit);
      read.onsuccess = step(() => done(read.result.filter((entry) => entry.user === user)));
    });
  }

  /** Deletes acknowledged entries. Unknown OpIds are ignored, so a repeated Ack is harmless. */
  async remove(opIds) {
    if (!Array.isArray(opIds)) {
      throw new TypeError('opIds must be an array');
    }
    opIds.forEach((opId) => decodeOpId(opId));
    return this.#transaction([ENTRIES], 'readwrite', (tx, done) => {
      const entries = tx.objectStore(ENTRIES);
      opIds.forEach((opId) => entries.delete(opId));
      done(undefined);
    });
  }

  /**
   * Deletes every entry that does not belong to (user, replica) and resolves to the number of
   * deleted entries, so the caller can show the visible notice that AC-43 requires.
   */
  async discardForeign({ user, replica }) {
    checkUser(user);
    checkReplica(replica);
    return this.#transaction([ENTRIES], 'readwrite', (tx, done, step) => {
      const entries = tx.objectStore(ENTRIES);
      const read = entries.getAll();
      read.onsuccess = step(() => {
        const foreign = read.result.filter(
          (entry) => entry.user !== user || entry.replica !== replica,
        );
        foreign.forEach((entry) => entries.delete(entry.opId));
        done(foreign.length);
      });
    });
  }

  /** Advances the replica's HLC past a stamp seen elsewhere (remote op or re-stamped Ack). */
  async observe({ replica, hlc, nowMs = Date.now() }) {
    checkReplica(replica);
    const stamp = decodeHlc(hlc);
    return this.#updateClock(replica, (clock) => observe(clock, stamp, nowMs));
  }

  /** Raises the replica's seq to at least `lastSeq` (the last seq the server accepted). */
  async resumeAfter({ replica, lastSeq }) {
    checkReplica(replica);
    return this.#updateClock(replica, (clock) => resumeAfter(clock, lastSeq));
  }

  close() {
    this.#db.close();
  }

  #updateClock(replica, change) {
    return this.#transaction([CLOCKS], 'readwrite', (tx, done, step) => {
      const clocks = tx.objectStore(CLOCKS);
      const read = clocks.get(replica);
      read.onsuccess = step(() => {
        const next = change(read.result ?? initialClock());
        clocks.put(next, replica);
        done(next);
      });
    });
  }

  // Runs `body` in one transaction. `body` registers its requests with callbacks (no awaits,
  // so the transaction cannot commit early), wraps each callback in `step` and passes its
  // result to `done`. A callback that throws aborts the transaction, and the promise rejects
  // with that error. The promise settles only after the transaction committed or aborted.
  #transaction(storeNames, mode, body) {
    return new Promise((resolve, reject) => {
      const tx = this.#db.transaction(storeNames, mode);
      let result;
      let failure;
      const fail = (error) => {
        failure ??= error;
        try {
          tx.abort();
        } catch {
          // Already finished; the abort or complete event reports the outcome.
        }
      };
      const done = (value) => {
        result = value;
      };
      const step = (callback) => () => {
        try {
          callback();
        } catch (error) {
          fail(error);
        }
      };
      tx.oncomplete = () => resolve(result);
      tx.onabort = () => reject(failure ?? tx.error ?? new Error('outbox transaction aborted'));
      step(() => body(tx, done, step))();
    });
  }
}

function checkUser(user) {
  if (typeof user !== 'string' || user.length === 0) {
    throw new TypeError('user must be a non empty string');
  }
}
