// Small reference models for the strategies whose runtime implementation is still in progress
// (`lww`: runtime/js/crdt/register/, `set`: runtime/js/crdt/set/). They state the merge rules
// of ARCHITECTURE 5.1 and 6.2 in the simplest possible code, so the hand written expectations
// of the vectors are checked by something independent of the implementations under test.
// Input checking is left to the real implementations; these models trust the vector format,
// which `format.mjs` validates first.

import { compareCodePoints, encode } from '../../runtime/js/crdt/canon/canon.mjs';

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
    return [...elements.entries()]
      .sort(([left], [right]) => compareCodePoints(left, right))
      .map(([, entry]) => entry);
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
