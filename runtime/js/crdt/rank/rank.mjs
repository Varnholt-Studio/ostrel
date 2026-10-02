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

/**
 * The longest valid key. A peer can send any key, so the length is bounded like every other
 * hostile input (SPEC 2, ARCHITECTURE 5.9). Keys made by `keyBetween` stay far below it unless
 * one gap is split thousands of times; see `RankLimit`.
 */
export const MAX_RANK_DIGITS = 1024;

const KEY_PATTERN = /^[0-9A-Za-z]*[1-9A-Za-z]$/;
const ROW_ID = /^[0-9a-f]{32}$/;
const OP_ID = /^[0-9a-f]{24}$/;
const HLC = /^[0-9a-f]{32}$/;

const ZERO = DIGITS[0];
const TOP = DIGITS[DIGITS.length - 1];
const BASE = DIGITS.length;

export class InvalidRank extends Error {
  constructor(message) {
    super(message);
    this.name = 'InvalidRank';
  }
}

/**
 * Thrown by `keyBetween` when both bounds are valid but the new key would be longer than
 * `MAX_RANK_DIGITS`. This happens only after about 6 000 inserts into the same gap; the list
 * then needs new keys for its rows (rebalancing), which is not part of this module.
 */
export class RankLimit extends Error {
  constructor(message) {
    super(message);
    this.name = 'RankLimit';
  }
}

/** True when `key` is a valid `Rank` key: 1 to 1024 base 62 digits, no trailing `0`. */
export function isValidRank(key) {
  return typeof key === 'string' && key.length <= MAX_RANK_DIGITS && KEY_PATTERN.test(key);
}

/**
 * Returns a new key that sorts strictly between `before` and `after`. `null` stands for the
 * start of the list (`before`) or its end (`after`), so `keyBetween(null, null)` is the key of
 * the first row of an empty list. Throws `InvalidRank` for an invalid key or when `before` does
 * not sort before `after`, and `RankLimit` when the new key would be too long.
 *
 * The result is deterministic, so the Rust implementation can return the same keys
 * (cases in tests/crdt-vectors/rank/keys/cases.json). Between two keys it is the shortest key
 * with the middle digit of the gap. At either end of the list it is a small step away from the
 * outermost key, so appending n rows at one end gives keys of about 2 * log62(n) digits.
 */
export function keyBetween(before, after) {
  if (before !== null && !isValidRank(before)) throw new InvalidRank(`invalid key: ${before}`);
  if (after !== null && !isValidRank(after)) throw new InvalidRank(`invalid key: ${after}`);
  if (before !== null && after !== null && compareText(before, after) >= 0) {
    throw new InvalidRank(`"${before}" does not sort before "${after}"`);
  }
  let key;
  if (before === null && after === null) key = DIGITS[BASE >> 1];
  else if (after === null) key = keyAfter(before);
  else if (before === null) key = keyBefore(after);
  else key = midpoint(before, after);
  if (key.length > MAX_RANK_DIGITS) {
    throw new RankLimit(`a key between "${before}" and "${after}" needs ${key.length} digits`);
  }
  return key;
}

// The key between the fractions `low` and `high` (both valid keys, low < high). All loops below
// are iterative, so a long key never deepens the call stack.
function midpoint(low, high) {
  // Keep the digits both bounds share; `low` reads as 0 beyond its end.
  let shared = 0;
  while ((low[shared] ?? ZERO) === high[shared]) shared += 1;
  const lowDigit = digitAt(low, shared);
  const highDigit = digitAt(high, shared);
  const prefix = high.slice(0, shared);
  if (highDigit - lowDigit > 1) {
    // A digit fits strictly between the two: take the middle one, rounding up.
    return prefix + DIGITS[(lowDigit + highDigit + 1) >> 1];
  }
  // Adjacent digits. A longer `high` leaves room right below it at this digit alone.
  if (high.length > shared + 1) return prefix + DIGITS[highDigit];
  // Otherwise keep the digit of `low` and go up from the rest of `low` towards 1: skip its
  // leading top digits, then take the middle digit between the next one and the top.
  let end = shared + 1;
  while (low[end] === TOP) end += 1;
  return prefix + DIGITS[lowDigit] + low.slice(shared + 1, end) + middleAbove(digitAt(low, end));
}

// The middle digit strictly between `digit` (not the top digit) and 1, rounding up.
function middleAbove(digit) {
  return DIGITS[(digit + BASE + 1) >> 1];
}

// A key after `key` with nothing behind it. The key is read as a level, the number k of its
// leading top digits, and a counter of k + 1 digits after them. The new key is that counter plus
// one; at the end of a level it carries into the next level. Level k holds about 62^(k + 1)
// keys, so the length grows with the logarithm of the number of appends.
function keyAfter(key) {
  let level = 0;
  while (key[level] === TOP) level += 1;
  const counter = digitsOf(key, level, level + 1);
  let at = counter.length - 1;
  // The first counter digit is below the top digit (or 0 if `key` is all top digits), so the
  // carry stops inside the counter.
  while (counter[at] === BASE - 1) {
    counter[at] = 0;
    at -= 1;
  }
  counter[at] += 1;
  return trimZeros(TOP.repeat(level) + counter.map((d) => DIGITS[d]).join(''));
}

// A key before `key` with nothing in front of it: the mirror of `keyAfter`, counting down with
// leading `0` digits as the level.
function keyBefore(key) {
  let level = 0;
  while (key[level] === ZERO) level += 1;
  const counter = digitsOf(key, level, level + 1);
  let at = counter.length - 1;
  // The first counter digit is at least 1, so the borrow stops inside the counter.
  while (counter[at] === 0) {
    counter[at] = BASE - 1;
    at -= 1;
  }
  counter[at] -= 1;
  const result = trimZeros(ZERO.repeat(level) + counter.map((d) => DIGITS[d]).join(''));
  // Only `key` = 0...01... at level 0 counts down to nothing; the next level starts below it.
  return result === '' ? ZERO.repeat(level + 1) + TOP : result;
}

// The `count` digit values of `key` from `start`, reading 0 beyond its end.
function digitsOf(key, start, count) {
  const digits = [];
  for (let i = start; i < start + count; i += 1) digits.push(digitAt(key, i));
  return digits;
}

function digitAt(key, index) {
  return index < key.length ? DIGITS.indexOf(key[index]) : 0;
}

function trimZeros(key) {
  let end = key.length;
  while (end > 0 && key[end - 1] === ZERO) end -= 1;
  return key.slice(0, end);
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
