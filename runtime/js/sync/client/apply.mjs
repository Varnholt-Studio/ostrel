// Confirmed state of the local replica and the optimistic view on top of it (ARCHITECTURE 5.4,
// D62, D151, D154, D156, D190).
//
// The client keeps, per (user, replica), the confirmed state of every row it holds and the mark
// `applied_server_seq`. Both change only in one IndexedDB transaction, so a reload never sees
// a row without the mark that belongs to it, or the other way round.
//
// * Ops from the server (live broadcast and reconnect catch up) arrive in non decreasing
//   `ServerSeq`. An op at or below the mark is dropped (exactly once delivery, D62); every
//   other op is applied to the confirmed row at its position and moves the mark.
// * Register order is log order (D190). A scalar field, a `Rank`, a whole `List` and each key
//   of a `Map` take the value of the op that comes last in log order. The client overwrites
//   them without comparing stamps, exactly as the server does, so its confirmed state equals
//   the server state at its mark. `Set` fields keep their add wins merge with tags (6.2).
// * An own op confirmed by `Ack` (or by the catch up, as an op of this replica) is applied at
//   its position, unless the mark already covers that position: then a `Snapshot` row holds
//   it, and the op only leaves the pending ops (AC-44). The ops of one batch commit in one
//   transaction, so a mark lies either below all of them or at or above the `Ack` position.
// * A `Snapshot` row replaces the confirmed state of its row without merging and keeps its
//   page mark per row; the page mark also moves `applied_server_seq` (D151, D154).
// * A rejected own op never reached the confirmed state. It is kept as a rejected record, so
//   the view can mark its row `rejected` (5.4 flow, B2-15), until the app dismisses it.
//
// The optimistic view (`viewRow`) is the confirmed row with the pending ops of the outbox
// applied on top in `seq` order. Their stamps decide nothing in the confirmed state.
//
// Stores (keys are JSON arrays, so key order is (user, replica, ...) order):
//   marks     [user, replica]               { seq }  applied_server_seq, 16 hex digits
//   rows      [user, replica, model, row]   { model, mark, row }  row in its wire form (5.4)
//   rejected  [user, replica, opId]         { opId, model, row, body, hlc, reason }

import { compareKey } from '../../crdt/canon/canon.mjs';

export const DEFAULT_DB_NAME = 'ostrel-data';
const DB_VERSION = 1;
const MARKS = 'marks';
const ROWS = 'rows';
const REJECTED = 'rejected';

const SERVER_SEQ_RE = /^[0-9a-f]{16}$/;
const OP_ID_RE = /^[0-9a-f]{24}$/;
const HLC_RE = /^[0-9a-f]{32}$/;
const REPLICA_RE = /^[0-9a-f]{16}$/;
const ROW_ID_RE = /^[0-9a-f]{32}$/;

/** Error for input this module cannot apply: an unknown op kind, a malformed id or row. */
export class ApplyError extends Error {
  constructor(message) {
    super(message);
    this.name = 'ApplyError';
  }
}

/**
 * Opens (and on first use creates) the data store of the local replica.
 *
 * `kinds` names the `Set` and `Map` fields of the compiled schema as
 * `{ Model: { field: 'set' | 'map' } }`; every other field is a register. `indexedDB` and
 * `IDBKeyRange` are passed in, so the module runs in the browser and in Node tests.
 */
export function openLocalReplica({ indexedDB, IDBKeyRange, name = DEFAULT_DB_NAME, kinds = {} }) {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(name, DB_VERSION);
    request.onupgradeneeded = () => {
      const db = request.result;
      for (const store of [MARKS, ROWS, REJECTED]) {
        if (!db.objectStoreNames.contains(store)) db.createObjectStore(store);
      }
    };
    request.onsuccess = () => resolve(new LocalReplica(request.result, IDBKeyRange, kinds));
    request.onerror = () => reject(request.error);
    request.onblocked = () => reject(new Error(`data store ${name} is blocked`));
  });
}

export class LocalReplica {
  #db;
  #keyRange;
  #kinds;

  constructor(db, keyRange, kinds) {
    this.#db = db;
    this.#keyRange = keyRange;
    this.#kinds = kinds;
  }

  /** The mark `applied_server_seq` of (user, replica), or null before the first op or page. */
  async appliedServerSeq({ user, replica }) {
    checkPair(user, replica);
    return this.#transaction([MARKS], 'readonly', (tx, done, step) => {
      const read = tx.objectStore(MARKS).get(markKey(user, replica));
      read.onsuccess = step(() => done(read.result?.seq ?? null));
    });
  }

  /** The confirmed row `{ model, mark, row }`, or null when the replica holds no such row. */
  async row({ user, replica, model, row }) {
    checkPair(user, replica);
    checkRowId(row);
    return this.#transaction([ROWS], 'readonly', (tx, done, step) => {
      const read = tx.objectStore(ROWS).get(rowKey(user, replica, model, row));
      read.onsuccess = step(() => done(read.result ?? null));
    });
  }

  /** Every confirmed row of `model`, in row id order. */
  async rows({ user, replica, model }) {
    checkPair(user, replica);
    const range = prefixRange(this.#keyRange, [user, replica, model]);
    return this.#transaction([ROWS], 'readonly', (tx, done, step) => {
      const read = tx.objectStore(ROWS).getAll(range);
      read.onsuccess = step(() => done(read.result));
    });
  }

  /**
   * Applies the ops of a server `ops` message, each `{ seq, op }` with `op` in its wire form
   * (`{ id, hlc, model, row, body }`). Ops at or below the mark are dropped (D62). Resolves to
   * `{ applied, dropped, own }`: the OpIds applied and dropped, and the OpIds of this replica
   * among both, which the catch up confirms, so the caller removes them from the outbox.
   */
  async applyOps({ user, replica, ops }) {
    checkPair(user, replica);
    if (!Array.isArray(ops)) throw new TypeError('ops must be an array');
    ops.forEach(({ seq, op }) => {
      checkServerSeq(seq);
      checkWireOp(op);
    });
    const kinds = this.#kinds;
    return this.#update(user, replica, ops.map(({ op }) => op), (state) => {
      const result = { applied: [], dropped: [], own: [] };
      for (const { seq, op } of ops) {
        if (op.id.startsWith(replica)) result.own.push(op.id);
        if (!state.advance(seq)) {
          result.dropped.push(op.id);
          continue;
        }
        state.apply(op.model, op.row, (current) => applyOp(current, op, kinds), seq);
        result.applied.push(op.id);
      }
      return result;
    });
  }

  /**
   * Applies own ops confirmed by one `Ack` at log position `seq` (5.4). `entries` are the
   * outbox entries the `Ack` confirms, in `seq` order; `restamped` holds the accepted stamps
   * the server gave some of them (5.5 step 3). When the mark is at or above `seq`, a
   * `Snapshot` already holds these ops, and nothing is applied. Resolves to
   * `{ applied, skipped }` with the OpIds of each.
   */
  async applyAck({ user, replica, seq, entries, restamped = [] }) {
    checkPair(user, replica);
    checkServerSeq(seq);
    if (!Array.isArray(entries)) throw new TypeError('entries must be an array');
    const accepted = new Map(restamped);
    const ops = entries.map((entry) => {
      const op = wireOpOf(entry, accepted.get(entry.opId) ?? entry.hlc);
      checkWireOp(op);
      if (!op.id.startsWith(replica)) {
        throw new ApplyError(`op ${op.id} is not an op of replica ${replica}`);
      }
      return op;
    });
    const kinds = this.#kinds;
    return this.#update(user, replica, ops, (state) => {
      const ids = ops.map((op) => op.id);
      if (ops.length === 0 || !state.advance(seq)) return { applied: [], skipped: ids };
      for (const op of ops) {
        state.apply(op.model, op.row, (current) => applyOp(current, op, kinds), seq);
      }
      return { applied: ids, skipped: [] };
    });
  }

  /**
   * Applies one `Snapshot` page of `model` with mark `seq`: every row replaces the confirmed
   * state of that row and keeps `seq` as its own mark; `applied_server_seq` moves to `seq` if
   * that is higher (D151, D154). A page below the mark changes nothing, because the server
   * never sends a session decreasing marks.
   */
  async applySnapshot({ user, replica, model, seq, rows }) {
    checkPair(user, replica);
    checkServerSeq(seq);
    if (!Array.isArray(rows)) throw new TypeError('rows must be an array');
    rows.forEach(checkWireRow);
    const targets = rows.map((row) => ({ model, row: row.id }));
    return this.#update(user, replica, targets, (state) => {
      if (state.mark !== null && seq < state.mark) return { replaced: 0 };
      state.advance(seq);
      for (const row of rows) state.apply(model, row.id, () => normalRow(row), seq);
      return { replaced: rows.length };
    });
  }

  /** Removes rows that left the reader's scope or were deleted (`Evict`). The mark stays. */
  async evict({ user, replica, model, rows }) {
    checkPair(user, replica);
    if (!Array.isArray(rows)) throw new TypeError('rows must be an array');
    rows.forEach(checkRowId);
    return this.#transaction([ROWS], 'readwrite', (tx, done) => {
      const store = tx.objectStore(ROWS);
      rows.forEach((row) => store.delete(rowKey(user, replica, model, row)));
      done(undefined);
    });
  }

  /**
   * Drops the confirmed state of (user, replica): every row and the mark. Rejected records
   * stay. The next `Hello` names no mark, and the rows come back as `Snapshot`. Used when the
   * confirmed state may hold an op the server rejected and its row is not known (AC-57 (c)).
   */
  async dropConfirmed({ user, replica }) {
    checkPair(user, replica);
    const range = prefixRange(this.#keyRange, [user, replica]);
    return this.#transaction([MARKS, ROWS], 'readwrite', (tx, done, step) => {
      const rowStore = tx.objectStore(ROWS);
      const read = rowStore.getAll(range);
      read.onsuccess = step(() => {
        for (const { model, row } of read.result) rowStore.delete(rowKey(user, replica, model, row.id));
        tx.objectStore(MARKS).delete(markKey(user, replica));
        done(read.result.length);
      });
    });
  }

  /** Records a rejected own op (an outbox entry) with the server's reason. */
  async reject({ user, replica, entry, reason }) {
    checkPair(user, replica);
    const op = wireOpOf(entry, entry.hlc);
    checkWireOp(op);
    if (typeof reason !== 'string' || reason.length === 0) {
      throw new TypeError('reason must be a non empty string');
    }
    const record = { opId: op.id, model: op.model, row: op.row, body: op.body, hlc: op.hlc, reason };
    return this.#transaction([REJECTED], 'readwrite', (tx, done) => {
      tx.objectStore(REJECTED).put(record, JSON.stringify([user, replica, op.id]));
      done(undefined);
    });
  }

  /** The rejected records of (user, replica) in OpId order. */
  async rejected({ user, replica }) {
    checkPair(user, replica);
    const range = prefixRange(this.#keyRange, [user, replica]);
    return this.#transaction([REJECTED], 'readonly', (tx, done, step) => {
      const read = tx.objectStore(REJECTED).getAll(range);
      read.onsuccess = step(() => done(read.result));
    });
  }

  /** Forgets rejected records once the app has shown them. Unknown OpIds are ignored. */
  async dismissRejected({ user, replica, opIds }) {
    checkPair(user, replica);
    if (!Array.isArray(opIds)) throw new TypeError('opIds must be an array');
    return this.#transaction([REJECTED], 'readwrite', (tx, done) => {
      const store = tx.objectStore(REJECTED);
      opIds.forEach((opId) => store.delete(JSON.stringify([user, replica, opId])));
      done(undefined);
    });
  }

  close() {
    this.#db.close();
  }

  // Reads the mark and the rows that `targets` (`{ model, row }`) name, lets `change` work on
  // them through a `State`, and writes the changed rows and the mark back, all in one
  // readwrite transaction.
  #update(user, replica, targets, change) {
    return this.#transaction([MARKS, ROWS], 'readwrite', (tx, done, step) => {
      const marks = tx.objectStore(MARKS);
      const rowStore = tx.objectStore(ROWS);
      const keys = [...new Set(targets.map((t) => rowKey(user, replica, t.model, t.row)))];
      const loaded = new Map();
      let mark = null;
      let waiting = keys.length + 1;
      const finish = () => {
        waiting -= 1;
        if (waiting > 0) return;
        const state = new State(mark, loaded, (model, row) => rowKey(user, replica, model, row));
        const result = change(state);
        for (const key of state.changed) {
          const value = state.records.get(key);
          if (value === null) rowStore.delete(key);
          else rowStore.put(value, key);
        }
        if (state.mark !== mark) marks.put({ seq: state.mark }, markKey(user, replica));
        done(result);
      };
      const readMark = marks.get(markKey(user, replica));
      readMark.onsuccess = step(() => {
        mark = readMark.result?.seq ?? null;
        finish();
      });
      for (const key of keys) {
        const read = rowStore.get(key);
        read.onsuccess = step(() => {
          loaded.set(key, read.result ?? null);
          finish();
        });
      }
    });
  }

  // Runs `body` in one transaction, as the outbox does: requests are registered with callbacks
  // wrapped in `step`, the result goes to `done`, a throwing callback aborts the transaction,
  // and the promise settles after commit or abort.
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
      tx.onabort = () => reject(failure ?? tx.error ?? new Error('data store transaction aborted'));
      step(() => body(tx, done, step))();
    });
  }
}

// The mark and the loaded rows of one transaction. Keys are the row store keys.
class State {
  constructor(mark, records, keyOf) {
    this.mark = mark;
    this.records = records;
    this.keyOf = keyOf;
    this.changed = new Set();
  }

  // Moves the mark to `seq` and returns true, or returns false when `seq` is at or below it.
  advance(seq) {
    if (this.mark !== null && seq <= this.mark) return false;
    this.mark = seq;
    return true;
  }

  // Replaces the confirmed row with `change(current row or null)`; null deletes it.
  apply(model, row, change, seq) {
    const key = this.keyOf(model, row);
    const current = this.records.get(key);
    const next = change(current?.row ?? null);
    this.records.set(key, next === null ? null : { model, mark: seq, row: next });
    this.changed.add(key);
  }
}

/**
 * Applies one op in its wire form to a row in its wire form (or null for no row) and returns
 * the new row, or null when the op deletes it. Pure. Registers are overwritten without
 * comparing stamps (D190); `Set` fields merge by tags (6.2). `made` and `changed` take the
 * wall time of the op's stamp, as the server writes them (D186). An op on a row that does not
 * exist, other than `make`, leaves no row.
 */
export function applyOp(row, op, kinds = {}) {
  const fieldKinds = kinds[op.model] ?? {};
  const time = wallMsOf(op.hlc);
  const [kind, args] = bodyOf(op.body);
  if (kind === 'make') return makeRow(op, args, fieldKinds, time);
  if (row === null) return null;
  if (kind === 'drop') return null;
  const next = structuredClone(row);
  switch (kind) {
    case 'set':
      next.fields[args.field] = args.value;
      break;
    case 'add':
      next.sets[args.field] = addElement(next.sets[args.field] ?? [], args.elem, op.id);
      break;
    case 'remove':
      next.sets[args.field] = removeElement(next.sets[args.field] ?? [], args.elem, args.tags);
      break;
    case 'put':
      next.maps[args.field] = putKey(next.maps[args.field] ?? [], args.key, {
        hlc: op.hlc,
        value: args.value,
      });
      break;
    case 'remove_key':
      next.maps[args.field] = putKey(next.maps[args.field] ?? [], args.key, {
        hlc: op.hlc,
        removed: true,
      });
      break;
    default:
      throw new ApplyError(`unknown op kind ${JSON.stringify(kind)}`);
  }
  next.fields.changed = time;
  return next;
}

/**
 * The optimistic view of one row: the confirmed row with the pending outbox entries of that
 * row applied on top in `seq` order. `pending` is true while an own op of the row waits for its
 * outcome; `rejected` is true when a rejected record names the row. A row whose `make` was
 * rejected and that the server never confirmed is shown with the values of that `make`, marked
 * rejected, until the app dismisses the record (B2-15). Returns null when nothing is left.
 */
export function viewRow({ model, row, confirmed = null, pending = [], rejected = [], kinds = {} }) {
  const own = pending
    .filter((entry) => entry.op.model === model && entry.op.row === row)
    .sort((a, b) => (a.seq < b.seq ? -1 : a.seq > b.seq ? 1 : 0));
  const refused = rejected.filter((record) => record.model === model && record.row === row);
  let state = confirmed;
  for (const entry of own) state = applyOp(state, wireOpOf(entry, entry.hlc), kinds);
  if (state === null && own.length === 0) {
    const make = refused.find((record) => 'make' in record.body);
    if (make !== undefined) {
      state = applyOp(null, { id: make.opId, hlc: make.hlc, model, row, body: make.body }, kinds);
    }
  }
  if (state === null) return null;
  return { ...state, pending: own.length > 0, rejected: refused.length > 0 };
}

function makeRow(op, fields, fieldKinds, time) {
  if (fields === null || typeof fields !== 'object' || Array.isArray(fields)) {
    throw new ApplyError('make needs an object of fields');
  }
  const row = { id: op.row, fields: {}, sets: {}, maps: {} };
  for (const [name, value] of Object.entries(fields)) {
    const kind = fieldKinds[name] ?? 'register';
    if (kind === 'set') {
      checkArray(value, `set field ${name}`);
      // Every initial element is tagged with the OpId of the make (D106).
      row.sets[name] = sortEntries(value.map((elem) => [elem, [op.id]]));
    } else if (kind === 'map') {
      checkArray(value, `map field ${name}`);
      // Initial entries carry the stamp of the make (D106).
      row.maps[name] = sortEntries(
        value.map((pair) => {
          checkArray(pair, `entry of map field ${name}`);
          return [pair[0], { hlc: op.hlc, value: pair[1] }];
        }),
      );
    } else {
      row.fields[name] = value;
    }
  }
  row.fields.made = time;
  row.fields.changed = time;
  return row;
}

// An add tags the element with the op's own OpId. At most one live tag per (element, replica):
// an add of a replica replaces that replica's earlier tag, and an add whose seq is not above it
// is a replay and changes nothing (6.2, `runtime/js/crdt/set`).
function addElement(entries, elem, opId) {
  const replica = opId.slice(0, 16);
  const index = entries.findIndex(([e]) => sameKey(e, elem));
  if (index < 0) return sortEntries([...entries, [elem, [opId]]]);
  const tags = entries[index][1];
  const mine = tags.find((tag) => tag.slice(0, 16) === replica);
  if (mine !== undefined && mine >= opId) return entries;
  const next = entries.slice();
  next[index] = [elem, [...tags.filter((tag) => tag !== mine), opId].sort()];
  return next;
}

// A remove deletes the tags it names; the element is gone when no tag is left. Unknown tags are
// ignored, so a replayed remove changes nothing.
function removeElement(entries, elem, tags) {
  checkArray(tags, 'tags of a remove');
  const index = entries.findIndex(([e]) => sameKey(e, elem));
  if (index < 0) return entries;
  const named = new Set(tags);
  const left = entries[index][1].filter((tag) => !named.has(tag));
  const next = entries.slice();
  if (left.length === 0) next.splice(index, 1);
  else next[index] = [elem, left];
  return next;
}

// Writes one key of a map: the value or the removal of the op last in log order wins (D190).
function putKey(entries, key, entry) {
  const next = entries.filter(([k]) => !sameKey(k, key));
  next.push([key, entry]);
  return sortEntries(next);
}

function sortEntries(entries) {
  return entries.sort(([a], [b]) => compareKey(a, b));
}

function sameKey(a, b) {
  return compareKey(a, b) === 0;
}

// The one key of an op body and its arguments.
function bodyOf(body) {
  if (body === null || typeof body !== 'object' || Array.isArray(body)) {
    throw new ApplyError('op body must be an object');
  }
  const keys = Object.keys(body);
  if (keys.length !== 1) throw new ApplyError('op body must have exactly one key');
  return [keys[0], body[keys[0]]];
}

// Wall time in ms of an HLC hex stamp: its first 12 hex digits (5.3).
function wallMsOf(hlc) {
  return Number.parseInt(hlc.slice(0, 12), 16);
}

// A snapshot row in the stored form: every part present, entries in key order.
function normalRow(row) {
  return {
    id: row.id,
    fields: { ...(row.fields ?? {}) },
    sets: Object.fromEntries(
      Object.entries(row.sets ?? {}).map(([name, entries]) => [name, sortEntries([...entries])]),
    ),
    maps: Object.fromEntries(
      Object.entries(row.maps ?? {}).map(([name, entries]) => [name, sortEntries([...entries])]),
    ),
  };
}

// The wire op of an outbox entry, with the stamp the server accepted for it.
function wireOpOf(entry, hlc) {
  if (entry === null || typeof entry !== 'object' || entry.op === null || typeof entry.op !== 'object') {
    throw new ApplyError('an outbox entry needs an op');
  }
  const { model, row, body } = entry.op;
  return { id: entry.opId, hlc, model, row, body };
}

function checkWireOp(op) {
  if (op === null || typeof op !== 'object') throw new ApplyError('op must be an object');
  if (typeof op.id !== 'string' || !OP_ID_RE.test(op.id)) throw new ApplyError('malformed op id');
  if (typeof op.hlc !== 'string' || !HLC_RE.test(op.hlc)) throw new ApplyError('malformed hlc');
  if (typeof op.model !== 'string' || op.model.length === 0) {
    throw new ApplyError('op needs a model');
  }
  checkRowId(op.row);
  bodyOf(op.body);
}

function checkWireRow(row) {
  if (row === null || typeof row !== 'object') throw new ApplyError('row must be an object');
  checkRowId(row.id);
}

function checkRowId(row) {
  if (typeof row !== 'string' || !ROW_ID_RE.test(row)) throw new ApplyError('malformed row id');
}

function checkServerSeq(seq) {
  if (typeof seq !== 'string' || !SERVER_SEQ_RE.test(seq)) {
    throw new ApplyError('malformed server seq');
  }
}

function checkPair(user, replica) {
  if (typeof user !== 'string' || user.length === 0) {
    throw new TypeError('user must be a non empty string');
  }
  if (typeof replica !== 'string' || !REPLICA_RE.test(replica)) {
    throw new TypeError('replica must be 16 lowercase hex characters');
  }
}

function checkArray(value, what) {
  if (!Array.isArray(value)) throw new ApplyError(`${what} must be an array`);
}

function markKey(user, replica) {
  return JSON.stringify([user, replica]);
}

function rowKey(user, replica, model, row) {
  return JSON.stringify([user, replica, model, row]);
}

// The key range of every key that starts with the JSON array `parts` (one more element after).
function prefixRange(keyRange, parts) {
  const open = JSON.stringify(parts).slice(0, -1) + ',';
  return keyRange.bound(open, open + '￿');
}

