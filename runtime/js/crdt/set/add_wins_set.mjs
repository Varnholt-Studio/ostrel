// Add wins observed remove set with add tags (ARCHITECTURE 5.1, 6.2, 5.9; D49).
//
// Semantics:
// * An add op of element e creates the tag t = the op's own OpId.
// * A remove op of e names the tags of e it observed. A tag is deleted only if
//   both its replica and its seq match a named tag. Unknown tags are ignored,
//   so a replayed remove is a no-op.
// * e is present while at least one tag of e is live. A concurrent add carries a
//   tag the remove did not name, so it survives (add wins).
// * At most one live tag per (element, replica): an add from replica R replaces
//   R's earlier tag of the same element. An add whose seq is not greater than
//   R's live tag is a replay and is ignored.
// * No tombstones. Delivery follows the server log (5.4), so a remove never
//   arrives before an add it names.
// * A remove names at most MAX_REMOVE_TAGS tags; removeOps() splits larger
//   removes into several ops for the same element.
//
// Wire (5.1): {"add": e} with the tag implicit in the op's OpId, or
// {"remove": e, "tags": [OpId hex, ...]}. OpId hex is 24 lowercase hex
// characters: replica (16) then seq (8), big endian (5.3).
//
// Elements are wire scalars: strings (no lone surrogates), finite numbers and
// booleans. Element order for values() and encodeState() is: booleans, then
// numbers ascending, then strings in Unicode code point order (D50).

/** Maximum number of tags one remove op may name (ARCHITECTURE 5.9). */
export const MAX_REMOVE_TAGS = 64;

const OP_ID_RE = /^[0-9a-f]{24}$/;
const LONE_SURROGATE_RE = /\p{Surrogate}/u;

/** Error for hostile or malformed input. `code` is always "Invalid". */
export class SetOpError extends Error {
  constructor(message) {
    super(message);
    this.name = "SetOpError";
    this.code = "Invalid";
  }
}

/** Throws SetOpError unless `opId` is a valid OpId hex string. */
export function checkOpId(opId) {
  if (typeof opId !== "string" || !OP_ID_RE.test(opId)) {
    throw new SetOpError("OpId must be 24 lowercase hex characters");
  }
  return opId;
}

function replicaOf(opId) {
  return opId.slice(0, 16);
}

// Fixed width hex compares like the value it encodes, so string order of
// seq parts equals numeric order.
function seqOf(opId) {
  return opId.slice(16);
}

function checkElement(e) {
  switch (typeof e) {
    case "boolean":
      return e;
    case "number":
      if (!Number.isFinite(e)) throw new SetOpError("set element must be a finite number");
      return e === 0 ? 0 : e; // -0 and 0 are the same element
    case "string":
      if (LONE_SURROGATE_RE.test(e)) throw new SetOpError("set element has a lone surrogate");
      return e;
    default:
      throw new SetOpError("set element must be a string, number or boolean");
  }
}

function keyOf(e) {
  return (typeof e)[0] + ":" + String(e);
}

/** Compares two strings by Unicode code point, not by UTF-16 unit (D50). */
function compareCodePoints(a, b) {
  const ia = a[Symbol.iterator]();
  const ib = b[Symbol.iterator]();
  for (;;) {
    const x = ia.next();
    const y = ib.next();
    if (x.done || y.done) return x.done === y.done ? 0 : x.done ? -1 : 1;
    const d = x.value.codePointAt(0) - y.value.codePointAt(0);
    if (d !== 0) return d < 0 ? -1 : 1;
  }
}

const TYPE_RANK = { boolean: 0, number: 1, string: 2 };

/** Total order over set elements, identical on every replica. */
export function compareElements(a, b) {
  const ta = TYPE_RANK[typeof a];
  const tb = TYPE_RANK[typeof b];
  if (ta !== tb) return ta < tb ? -1 : 1;
  if (typeof a === "string") return compareCodePoints(a, b);
  if (a === b) return 0;
  return a < b ? -1 : 1;
}

export class AddWinsSet {
  constructor() {
    // key -> { elem, tags: Map<replicaHex, opIdHex> }
    this._entries = new Map();
  }

  /** True while at least one tag of `e` is live. */
  has(e) {
    const entry = this._entries.get(keyOf(checkElement(e)));
    return entry !== undefined;
  }

  get size() {
    return this._entries.size;
  }

  /** Present elements in the canonical element order. */
  values() {
    return [...this._entries.values()].map((x) => x.elem).sort(compareElements);
  }

  /** Live tags of `e`, sorted (OpId hex order). Empty if absent. */
  tagsOf(e) {
    const entry = this._entries.get(keyOf(checkElement(e)));
    return entry === undefined ? [] : [...entry.tags.values()].sort();
  }

  /**
   * Applies one wire op with its OpId. Used for local and remote ops alike.
   * Throws SetOpError on malformed input; the set is unchanged in that case.
   */
  apply(op, opId) {
    checkOpId(opId);
    if (op === null || typeof op !== "object" || Array.isArray(op)) {
      throw new SetOpError("set op must be an object");
    }
    const keys = Object.keys(op);
    if (keys.length === 1 && keys[0] === "add") {
      this._add(checkElement(op.add), opId);
      return;
    }
    if (keys.length === 2 && "remove" in op && "tags" in op) {
      const e = checkElement(op.remove);
      const tags = op.tags;
      if (!Array.isArray(tags) || tags.length === 0) {
        throw new SetOpError("remove needs a non empty tags array");
      }
      if (tags.length > MAX_REMOVE_TAGS) {
        throw new SetOpError("remove names more than " + MAX_REMOVE_TAGS + " tags");
      }
      for (const t of tags) checkOpId(t);
      this._remove(e, tags);
      return;
    }
    throw new SetOpError("set op must be {add} or {remove, tags}");
  }

  _add(e, opId) {
    const k = keyOf(e);
    let entry = this._entries.get(k);
    if (entry === undefined) {
      entry = { elem: e, tags: new Map() };
      this._entries.set(k, entry);
    }
    const r = replicaOf(opId);
    const live = entry.tags.get(r);
    if (live === undefined || seqOf(live) < seqOf(opId)) entry.tags.set(r, opId);
  }

  _remove(e, tags) {
    const k = keyOf(e);
    const entry = this._entries.get(k);
    if (entry === undefined) return;
    for (const t of tags) {
      const r = replicaOf(t);
      if (entry.tags.get(r) === t) entry.tags.delete(r);
    }
    if (entry.tags.size === 0) this._entries.delete(k);
  }

  /** Wire op for a local add of `e`; apply it with the op's OpId. */
  static addOp(e) {
    return { add: checkElement(e) };
  }

  /**
   * Wire ops for a local remove of `e`: one op per MAX_REMOVE_TAGS observed
   * tags, tags sorted. Empty if `e` is absent (nothing observed to remove).
   */
  removeOps(e) {
    const elem = checkElement(e);
    const tags = this.tagsOf(elem);
    const ops = [];
    for (let i = 0; i < tags.length; i += MAX_REMOVE_TAGS) {
      ops.push({ remove: elem, tags: tags.slice(i, i + MAX_REMOVE_TAGS) });
    }
    return ops;
  }

  /**
   * Deterministic encoding of the full state (elements and their live tags),
   * equal on two replicas exactly when their states are equal.
   */
  encodeState() {
    const elems = [...this._entries.values()].sort((a, b) => compareElements(a.elem, b.elem));
    return JSON.stringify(elems.map((x) => [x.elem, [...x.tags.values()].sort()]));
  }
}
