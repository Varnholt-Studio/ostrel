// Ordered membership of one live list (ARCHITECTURE 5.8, step "live query invalidation").
//
// The index keeps the row ids of a list sorted by (sort key, id) and remembers, for every
// member, the key it was indexed under. Searches use those indexed keys only, never the
// current row values: when one batch moves several rows, the rows already carry their new
// values while the array still has the old order, and a search over live values misses
// rows (TIGER-1 finding 1, spikes/tiger-crdt/README.md).
//
// Order is total and deterministic: numbers compare numerically, strings by Unicode code
// point (never by UTF-16 units, D50/D61), arrays element by element; ties break on the id.

import { compareCodePoints } from "../../crdt/canon/canon.mjs";

/** Error raised for invalid list input. `code` is stable for tests. */
export class ListError extends Error {
  constructor(code, message) {
    super(`${code}: ${message}`);
    this.name = "ListError";
    this.code = code;
  }
}

// Rank of a key type: numbers sort before strings, strings before arrays.
function typeRank(key) {
  if (typeof key === "number") return 0;
  if (typeof key === "string") return 1;
  return 2;
}

function checkKey(key, depth = 0) {
  if (typeof key === "string") return;
  if (typeof key === "number" && Number.isFinite(key)) return;
  if (Array.isArray(key) && depth === 0) {
    for (const part of key) checkKey(part, 1);
    return;
  }
  throw new ListError(
    "KeyType",
    "sort key must be a string, a finite number or a flat array of those",
  );
}

function checkId(id) {
  if (typeof id !== "string") throw new ListError("IdType", "row id must be a string");
}

/** Total order on sort keys. Returns a negative number, zero or a positive number. */
export function compareSortKeys(a, b) {
  const ra = typeRank(a);
  const rb = typeRank(b);
  if (ra !== rb) return ra - rb;
  if (ra === 0) return a < b ? -1 : a > b ? 1 : 0;
  if (ra === 1) return compareCodePoints(a, b);
  const shared = Math.min(a.length, b.length);
  for (let i = 0; i < shared; i++) {
    const c = compareSortKeys(a[i], b[i]);
    if (c !== 0) return c;
  }
  return a.length - b.length;
}

export class SortedIndex {
  constructor() {
    this.ids = [];
    // id -> key the id was indexed under. The only source for searches.
    this.keys = new Map();
  }

  get size() {
    return this.ids.length;
  }

  /** Id at position `i`, or undefined. */
  at(i) {
    return this.ids[i];
  }

  has(id) {
    return this.keys.has(id);
  }

  /** Key `id` is indexed under, or undefined. */
  keyOf(id) {
    return this.keys.get(id);
  }

  compare(keyA, idA, keyB, idB) {
    return compareSortKeys(keyA, keyB) || compareCodePoints(idA, idB);
  }

  // First position whose indexed (key, id) is not less than (key, id).
  lowerBound(key, id) {
    let lo = 0;
    let hi = this.ids.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      const other = this.ids[mid];
      if (this.compare(this.keys.get(other), other, key, id) < 0) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  }

  /** Position of `id`, or -1. O(log n). */
  indexOf(id) {
    const key = this.keys.get(id);
    if (key === undefined) return -1;
    const at = this.lowerBound(key, id);
    return this.ids[at] === id ? at : -1;
  }

  /**
   * Applies one batch. `updates` is an iterable of [id, key]; key `undefined` removes the
   * id from the list. The whole batch is validated before the first change, so an invalid
   * entry leaves the index untouched. Returns { moved, first }: `moved` is true when the
   * order or the membership changed, `first` is the lowest position that changed (or -1).
   */
  apply(updates) {
    const batch = new Map();
    for (const [id, key] of updates) {
      checkId(id);
      if (key !== undefined) checkKey(key);
      if (batch.has(id)) throw new ListError("DuplicateUpdate", `id ${id} updated twice in one batch`);
      batch.set(id, key);
    }
    let first = -1;
    const note = (at) => {
      if (first < 0 || at < first) first = at;
    };
    // Phase 1: take out every member that leaves or changes its key, found by its
    // indexed key. Members not yet processed keep their indexed keys, so every search
    // runs over a consistently ordered array.
    const enter = [];
    for (const [id, key] of batch) {
      const old = this.keys.get(id);
      if (old !== undefined) {
        if (key !== undefined && compareSortKeys(old, key) === 0) continue;
        const at = this.lowerBound(old, id);
        if (this.ids[at] !== id) throw new ListError("IndexDesync", `id ${id} not at its indexed key`);
        this.ids.splice(at, 1);
        this.keys.delete(id);
        note(at);
      }
      if (key !== undefined) enter.push([id, key]);
    }
    // Phase 2: insert under the new keys.
    for (const [id, key] of enter) {
      const at = this.lowerBound(key, id);
      this.ids.splice(at, 0, id);
      this.keys.set(id, key);
      note(at);
    }
    return { moved: first >= 0, first };
  }
}
