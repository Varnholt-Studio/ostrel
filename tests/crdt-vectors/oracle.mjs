// Small reference models for the strategies `lww`, `set` and `rank`. They state the merge rules
// of ARCHITECTURE 5.1 and 6.2 in the simplest possible code, so the hand written expectations
// of the vectors are checked by something independent of the implementations under test
// (runtime/js/crdt/register/, set/ and rank/), which run against the same vectors.
// Input checking is left to the real implementations; these models trust the vector format,
// which `format.mjs` validates first.

import { compareKey, compareText, encode } from '../../runtime/js/crdt/canon/canon.mjs';

const REPLICA_HEX_WIDTH = 16;

/** Last writer wins register: the op with the highest Hlc wins. Op body: `{ "set": value }`. */
export function createLwwReplica() {
  let current = { hlc: null, value: null };
  return {
    apply({ hlc, op }) {
      if (current.hlc === null || current.hlc < hlc) current = { hlc, value: op.set };
    },
    value: () => current.value,
    state: () => ({ hlc: current.hlc, value: current.value }),
  };
}

/**
 * Add wins observed remove set with add tags (D49). Op bodies: `{ "add": e }` creates the tag
 * equal to the op id; `{ "remove": e, "tags": [...] }` deletes exactly the named live tags.
 * Each replica keeps at most one live tag per element, its newest one.
 */
export function createSetReplica() {
  // canonical element -> { element, tags: Map(replica hex -> op id hex) }
  const elements = new Map();

  function sorted() {
    return [...elements.values()].sort((left, right) => compareKey(left.element, right.element));
  }

  return {
    apply({ id, op }) {
      if ('add' in op) {
        const slot = encode(op.add);
        const entry = elements.get(slot) ?? { element: op.add, tags: new Map() };
        const replica = id.slice(0, REPLICA_HEX_WIDTH);
        const older = entry.tags.get(replica);
        if (older === undefined || older < id) entry.tags.set(replica, id);
        elements.set(slot, entry);
        return;
      }
      const slot = encode(op.remove);
      const entry = elements.get(slot);
      if (entry === undefined) return;
      for (const tag of op.tags) {
        const replica = tag.slice(0, REPLICA_HEX_WIDTH);
        if (entry.tags.get(replica) === tag) entry.tags.delete(replica);
      }
      if (entry.tags.size === 0) elements.delete(slot);
    },
    value: () => sorted().map((entry) => entry.element),
    // Tags are lowercase hex (ASCII), so the default sort is already code point order.
    state: () => sorted().map((entry) => [entry.element, [...entry.tags.values()].sort()]),
  };
}

/**
 * `Rank` fields of the rows of one list. Op body: `{ "set": [row, key] }`; per row the highest
 * Hlc wins. Rows are listed by key as text, rows with equal keys by row id.
 */
export function createRankReplica() {
  const rows = new Map(); // row id -> { row, hlc, rank }

  function ordered() {
    return [...rows.values()].sort(
      (left, right) => compareText(left.rank, right.rank) || compareText(left.row, right.row),
    );
  }

  return {
    apply({ hlc, op }) {
      const [row, rank] = op.set;
      const current = rows.get(row);
      if (current === undefined || current.hlc < hlc) rows.set(row, { row, hlc, rank });
    },
    value: () => ordered().map((entry) => entry.row),
    state: () => ordered().map((entry) => [entry.row, { hlc: entry.hlc, rank: entry.rank }]),
  };
}
