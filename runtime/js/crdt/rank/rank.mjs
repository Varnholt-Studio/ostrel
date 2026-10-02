// `Rank` field: a fractional index key, merged as a last writer wins register
// (ARCHITECTURE 5.1, grammar decision G6).
//
// A `Rank` orders rows by hand, for example the issues of one board column. Moving a row writes
// one new key between the keys of its new neighbours, so a move is a single field write and two
// concurrent moves of different rows never conflict. Two concurrent moves of the same row are an
// ordinary LWW conflict: the higher `Hlc` wins.
//
// Key format: a non empty string of base 62 digits `0-9A-Za-z` that does not end in `0`. A key is
// read as the fraction 0.d1d2d3... in base 62, and the digits are listed in code point order, so
// comparing keys as text (`compareText`, D50) compares the fractions. Because no key ends in `0`,
// every fraction has exactly one key, and there is always room for a new key between two others.
//
// Two replicas that insert between the same neighbours at the same time can create the same key.
// Rows with equal keys are ordered by row id, so every replica still shows the same order
// (`compareRanked`).

import { compareText } from '../canon/canon.mjs';

/** The digits of a key, in ascending order. Their code points ascend as well. */
export const DIGITS = '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz';

const KEY_PATTERN = /^[0-9A-Za-z]*[1-9A-Za-z]$/;
const ROW_ID = /^[0-9a-f]{32}$/;
const OP_ID = /^[0-9a-f]{24}$/;
const HLC = /^[0-9a-f]{32}$/;

export class InvalidRank extends Error {
  constructor(message) {
    super(message);
    this.name = 'InvalidRank';
  }
}

/** True when `key` is a valid `Rank` key: base 62 digits, not empty, no trailing `0`. */
export function isValidRank(key) {
  return typeof key === 'string' && KEY_PATTERN.test(key);
}

/**
 * Returns a new key that sorts strictly between `before` and `after`. `null` stands for the
 * start of the list (`before`) or its end (`after`), so `keyBetween(null, null)` is the key of
 * the first row of an empty list. Throws `InvalidRank` for an invalid key or when `before` does
 * not sort before `after`.
 *
 * The result is deterministic, so the Rust implementation can return the same keys
 * (cases in tests/crdt-vectors/rank/keys/cases.json). It is the shortest key with the middle
 * digit of the gap; appending at one end grows the key by one digit every few moves.
 */
export function keyBetween(before, after) {
  if (before !== null && !isValidRank(before)) throw new InvalidRank(`invalid key: ${before}`);
  if (after !== null && !isValidRank(after)) throw new InvalidRank(`invalid key: ${after}`);
  if (before !== null && after !== null && compareText(before, after) >= 0) {
    throw new InvalidRank(`"${before}" does not sort before "${after}"`);
  }
  return midpoint(before ?? '', after);
}

// The key between the fractions `low` (digits, may be empty for 0) and `high` (digits, or null
// for 1). Requires low < high; neither ends in `0`.
function midpoint(low, high) {
  if (high !== null) {
    // Keep the digits both bounds share; `low` reads as 0 beyond its end.
    let shared = 0;
    while ((low[shared] ?? DIGITS[0]) === high[shared]) shared += 1;
    if (shared > 0) {
      return high.slice(0, shared) + midpoint(low.slice(shared), high.slice(shared));
    }
  }
  const lowDigit = low === '' ? 0 : DIGITS.indexOf(low[0]);
  const highDigit = high === null ? DIGITS.length : DIGITS.indexOf(high[0]);
  if (highDigit - lowDigit > 1) {
    // A digit fits strictly between the two: take the middle one, rounding up.
    return DIGITS[(lowDigit + highDigit + 1) >> 1];
  }
  // Adjacent first digits. A longer `high` leaves room right below it at its first digit alone;
  // otherwise keep the first digit of `low` and look for room after it.
  if (high !== null && high.length > 1) return high[0];
  return DIGITS[lowDigit] + midpoint(low.slice(1), null);
}

/**
 * Orders two ranked rows `{ row, rank }`: by key as text, then by row id. Row ids are fixed width
 * lowercase hex, so text order is their numeric order. Returns -1, 0 or 1.
 */
export function compareRanked(a, b) {
  return compareText(a.rank, b.rank) || compareText(a.row, b.row);
}

/**
 * Creates a replica in the model shape of the shared CRDT vectors (tests/crdt-vectors/README.md,
 * strategy `rank`). It holds the `Rank` field of the rows of one ordered list. The op body
 * `{ "set": [row, key] }` writes the key of one row; per row the write with the highest `Hlc`
 * wins. `value()` is the list of row ids in order, `state()` the pairs
 * `[row, { hlc, rank }]` in the same order.
 *
 * A malformed op throws `InvalidRank` and leaves the replica unchanged. Two different keys for one
 * row under the same `Hlc` cannot come from correct replicas and throw as well.
 */
export function createReplica() {
  // row id -> { row, hlc, rank }
  const rows = new Map();

  function ordered() {
    return [...rows.values()].sort(compareRanked);
  }

  return {
    apply(envelope) {
      const incoming = checkEnvelope(envelope);
      const current = rows.get(incoming.row);
      if (current === undefined || current.hlc < incoming.hlc) {
        // Fixed width lowercase hex compares like the Hlc value itself (ARCHITECTURE 5.3).
        rows.set(incoming.row, incoming);
      } else if (current.hlc === incoming.hlc && current.rank !== incoming.rank) {
        throw new InvalidRank(`two different keys for row ${incoming.row} carry hlc ${current.hlc}`);
      }
    },
    value: () => ordered().map((entry) => entry.row),
    state: () => ordered().map((entry) => [entry.row, { hlc: entry.hlc, rank: entry.rank }]),
  };
}

function checkEnvelope(envelope) {
  if (!isPlainObject(envelope)) throw new InvalidRank('an op must be an object');
  const { id, hlc, op } = envelope;
  if (typeof id !== 'string' || !OP_ID.test(id)) throw new InvalidRank(`bad op id: ${id}`);
  if (typeof hlc !== 'string' || !HLC.test(hlc)) throw new InvalidRank(`bad hlc: ${hlc}`);
  const fields = isPlainObject(op) ? Object.keys(op) : [];
  if (fields.length !== 1 || fields[0] !== 'set') {
    throw new InvalidRank('a rank op body is exactly { "set": [row, key] }');
  }
  const pair = op.set;
  if (!Array.isArray(pair) || pair.length !== 2) {
    throw new InvalidRank('"set" takes a [row, key] pair');
  }
  const [row, rank] = pair;
  if (typeof row !== 'string' || !ROW_ID.test(row)) throw new InvalidRank(`bad row id: ${row}`);
  if (!isValidRank(rank)) throw new InvalidRank(`bad key: ${rank}`);
  return { row, hlc, rank };
}

function isPlainObject(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
