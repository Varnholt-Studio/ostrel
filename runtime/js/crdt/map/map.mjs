// `Map[K, V]` field: a last writer wins register per key, with tombstones
// (ARCHITECTURE 5.1, 6.2, grammar decision G6).
//
// Every op sets or removes exactly one key (G12). For each key the replica keeps the entry with
// the highest `Hlc`; a removal is kept as a tombstone, so an older `put` that arrives later
// cannot bring the key back. Because `Hlc` is a total order (wall time, counter, replica), every
// replica that applied the same ops holds the same entries, whatever the delivery order, and
// applying an op twice changes nothing.
//
// Op shape (wire JSON), wrapped as `{ id, hlc, op }` like every CRDT op:
//   { "put": [key, value] }   sets `key` to `value`
//   { "remove": key }         removes `key`
// Keys are strings, numbers or booleans.

import { compareCodePoints, encode, InvalidValue } from '../canon/canon.mjs';

const OP_ID = /^[0-9a-f]{24}$/;
const HLC = /^[0-9a-f]{32}$/;

export class InvalidOp extends Error {
  constructor(message) {
    super(message);
    this.name = 'InvalidOp';
  }
}

export class LwwMap {
  // Entries by canonical key encoding: `{ key, hlc, value }` for a live key and
  // `{ key, hlc, removed: true }` for a tombstone.
  #entries = new Map();

  /** Applies one op `{ id, hlc, op }`. Malformed input throws `InvalidOp` and changes nothing. */
  apply(envelope) {
    const { hlc, op } = checkEnvelope(envelope);
    const incoming = toEntry(hlc, op);
    const slot = encodeKey(incoming.key);
    const current = this.#entries.get(slot);
    if (current === undefined || current.hlc < incoming.hlc) {
      // Fixed width lowercase hex compares like the Hlc value itself (ARCHITECTURE 5.3).
      this.#entries.set(slot, incoming);
    } else if (current.hlc === incoming.hlc && encode(current) !== encode(incoming)) {
      throw new InvalidOp(`two different ops carry the same hlc ${hlc}`);
    }
  }

  /** The visible value: `[key, value]` pairs of live keys, ordered by canonical key encoding. */
  value() {
    return this.#sortedEntries()
      .filter((entry) => !entry.removed)
      .map((entry) => [entry.key, entry.value]);
  }

  /** The full replica state including tombstones, compared across replicas in canonical form. */
  state() {
    return this.#sortedEntries().map((entry) =>
      entry.removed
        ? [entry.key, { hlc: entry.hlc, removed: true }]
        : [entry.key, { hlc: entry.hlc, value: entry.value }],
    );
  }

  #sortedEntries() {
    return [...this.#entries.entries()]
      .sort(([left], [right]) => compareCodePoints(left, right))
      .map(([, entry]) => entry);
  }
}

/** Strategy entry point used by the vector runner in `tests/crdt-vectors/`. */
export function createReplica() {
  return new LwwMap();
}

function checkEnvelope(envelope) {
  if (!isPlainObject(envelope)) throw new InvalidOp('an op must be an object');
  const { id, hlc, op } = envelope;
  if (typeof id !== 'string' || !OP_ID.test(id)) throw new InvalidOp(`bad op id: ${id}`);
  if (typeof hlc !== 'string' || !HLC.test(hlc)) throw new InvalidOp(`bad hlc: ${hlc}`);
  if (!isPlainObject(op)) throw new InvalidOp('the op body must be an object');
  return { hlc, op };
}

function toEntry(hlc, op) {
  const fields = Object.keys(op);
  if (fields.length !== 1) throw new InvalidOp('a map op has exactly one of "put" or "remove"');
  if (fields[0] === 'put') {
    const pair = op.put;
    if (!Array.isArray(pair) || pair.length !== 2) {
      throw new InvalidOp('"put" takes a [key, value] pair');
    }
    const [key, value] = pair;
    checkKey(key);
    checkValue(value);
    return { key, hlc, value };
  }
  if (fields[0] === 'remove') {
    checkKey(op.remove);
    return { key: op.remove, hlc, removed: true };
  }
  throw new InvalidOp(`unknown map op "${fields[0]}"`);
}

function checkKey(key) {
  const kind = typeof key;
  if (kind !== 'string' && kind !== 'number' && kind !== 'boolean') {
    throw new InvalidOp('a map key is a string, number or boolean');
  }
  encodeKey(key);
}

function checkValue(value) {
  try {
    encode(value);
  } catch (error) {
    if (error instanceof InvalidValue) throw new InvalidOp(`bad value: ${error.message}`);
    throw error;
  }
}

function encodeKey(key) {
  try {
    return encode(key);
  } catch (error) {
    if (error instanceof InvalidValue) throw new InvalidOp(`bad key: ${error.message}`);
    throw error;
  }
}

function isPlainObject(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
