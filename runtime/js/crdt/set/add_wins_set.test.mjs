import { test } from "node:test";
import assert from "node:assert/strict";
import { AddWinsSet, MAX_REMOVE_TAGS, SetOpError, compareElements } from "./add_wins_set.mjs";

// OpId hex: replica (16 hex) then seq (8 hex).
function opId(replica, seq) {
  return replica.toString(16).padStart(16, "0") + seq.toString(16).padStart(8, "0");
}

const A = 0xa;
const B = 0xb;
const C = 0xc;

function invalid(fn) {
  assert.throws(fn, (err) => err instanceof SetOpError && err.code === "Invalid");
}

test("add then has, remove observed tags then absent", () => {
  const s = new AddWinsSet();
  s.apply(AddWinsSet.addOp("x"), opId(A, 1));
  assert.equal(s.has("x"), true);
  assert.deepEqual(s.tagsOf("x"), [opId(A, 1)]);
  for (const op of s.removeOps("x")) s.apply(op, opId(A, 2));
  assert.equal(s.has("x"), false);
  assert.equal(s.size, 0);
});

test("concurrent add survives a remove that did not observe it (add wins)", () => {
  // A adds x, B observes it and removes; concurrently C adds x.
  const removeOp = { remove: "x", tags: [opId(A, 1)] };
  const orders = [
    [[{ add: "x" }, opId(A, 1)], [removeOp, opId(B, 1)], [{ add: "x" }, opId(C, 1)]],
    [[{ add: "x" }, opId(A, 1)], [{ add: "x" }, opId(C, 1)], [removeOp, opId(B, 1)]],
    [[{ add: "x" }, opId(C, 1)], [{ add: "x" }, opId(A, 1)], [removeOp, opId(B, 1)]],
  ];
  const states = orders.map((order) => {
    const s = new AddWinsSet();
    for (const [op, id] of order) s.apply(op, id);
    assert.equal(s.has("x"), true);
    assert.deepEqual(s.tagsOf("x"), [opId(C, 1)]);
    return s.encodeState();
  });
  assert.equal(new Set(states).size, 1);
});

test("re-add by the same replica replaces its tag; remove of the older tag keeps it", () => {
  const s = new AddWinsSet();
  s.apply({ add: "x" }, opId(A, 1));
  s.apply({ add: "x" }, opId(A, 2));
  assert.deepEqual(s.tagsOf("x"), [opId(A, 2)]);
  // B observed only A's first add.
  s.apply({ remove: "x", tags: [opId(A, 1)] }, opId(B, 1));
  assert.equal(s.has("x"), true);
  assert.deepEqual(s.tagsOf("x"), [opId(A, 2)]);
});

test("re-add after remove makes the element present again", () => {
  const s = new AddWinsSet();
  s.apply({ add: 7 }, opId(A, 1));
  s.apply({ remove: 7, tags: [opId(A, 1)] }, opId(A, 2));
  assert.equal(s.has(7), false);
  s.apply({ add: 7 }, opId(A, 3));
  assert.equal(s.has(7), true);
});

test("replay: duplicate and older adds, duplicate removes are no-ops", () => {
  const s = new AddWinsSet();
  s.apply({ add: "x" }, opId(A, 1));
  s.apply({ add: "x" }, opId(A, 3));
  const before = s.encodeState();
  s.apply({ add: "x" }, opId(A, 3));
  s.apply({ add: "x" }, opId(A, 2));
  assert.equal(s.encodeState(), before);
  s.apply({ add: "y" }, opId(B, 1));
  s.apply({ remove: "y", tags: [opId(B, 1)] }, opId(C, 1));
  const afterRemove = s.encodeState();
  s.apply({ remove: "y", tags: [opId(B, 1)] }, opId(C, 1));
  s.apply({ remove: "z", tags: [opId(B, 9)] }, opId(C, 2));
  s.apply({ remove: "x", tags: [opId(B, 9)] }, opId(C, 3));
  assert.equal(s.encodeState(), afterRemove);
  assert.deepEqual(s.values(), ["x"]);
});

test("remove of a tag that belongs to another element does nothing", () => {
  const s = new AddWinsSet();
  s.apply({ add: "x" }, opId(A, 1));
  s.apply({ add: "y" }, opId(A, 2));
  s.apply({ remove: "y", tags: [opId(A, 1)] }, opId(B, 1));
  assert.deepEqual(s.values(), ["x", "y"]);
});

test("split remove: more than MAX_REMOVE_TAGS observed tags give several ops", () => {
  const s = new AddWinsSet();
  const n = MAX_REMOVE_TAGS * 2 + 1;
  for (let r = 1; r <= n; r++) s.apply({ add: "x" }, opId(r, 1));
  const ops = s.removeOps("x");
  assert.deepEqual(ops.map((op) => op.tags.length), [MAX_REMOVE_TAGS, MAX_REMOVE_TAGS, 1]);
  const all = ops.flatMap((op) => op.tags);
  assert.deepEqual(all, s.tagsOf("x"));
  // Concurrent add from a replica the remover did not observe.
  const other = new AddWinsSet();
  for (let r = 1; r <= n; r++) other.apply({ add: "x" }, opId(r, 1));
  other.apply({ add: "x" }, opId(n + 1, 1));
  ops.forEach((op, i) => other.apply(op, opId(0xfff, i + 1)));
  assert.deepEqual(other.tagsOf("x"), [opId(n + 1, 1)]);
  ops.forEach((op, i) => s.apply(op, opId(0xfff, i + 1)));
  assert.equal(s.has("x"), false);
  assert.deepEqual(s.removeOps("x"), []);
});

test("hostile input is rejected as Invalid and leaves the set unchanged", () => {
  const s = new AddWinsSet();
  s.apply({ add: "x" }, opId(A, 1));
  const before = s.encodeState();
  const tooMany = Array.from({ length: MAX_REMOVE_TAGS + 1 }, (_, i) => opId(B, i + 1));
  invalid(() => s.apply({ add: "x" }, "A".repeat(24)));
  invalid(() => s.apply({ add: "x" }, opId(A, 1) + "0"));
  invalid(() => s.apply({ add: "x" }, 5));
  invalid(() => s.apply(null, opId(A, 2)));
  invalid(() => s.apply([], opId(A, 2)));
  invalid(() => s.apply({}, opId(A, 2)));
  invalid(() => s.apply({ add: "x", tags: [] }, opId(A, 2)));
  invalid(() => s.apply({ add: {} }, opId(A, 2)));
  invalid(() => s.apply({ add: null }, opId(A, 2)));
  invalid(() => s.apply({ add: NaN }, opId(A, 2)));
  invalid(() => s.apply({ add: Infinity }, opId(A, 2)));
  invalid(() => s.apply({ add: "\ud800" }, opId(A, 2)));
  invalid(() => s.apply({ remove: "x" }, opId(A, 2)));
  invalid(() => s.apply({ remove: "x", tags: [] }, opId(A, 2)));
  invalid(() => s.apply({ remove: "x", tags: "nope" }, opId(A, 2)));
  invalid(() => s.apply({ remove: "x", tags: [opId(A, 1), "zz"] }, opId(A, 2)));
  invalid(() => s.apply({ remove: "x", tags: tooMany }, opId(A, 2)));
  invalid(() => s.apply({ remove: "x", tags: [opId(A, 1)], extra: 1 }, opId(A, 2)));
  assert.equal(s.encodeState(), before);
});

test("element identity: -0 equals 0, types are distinct, astral text is fine", () => {
  const s = new AddWinsSet();
  s.apply({ add: -0 }, opId(A, 1));
  assert.equal(s.has(0), true);
  s.apply({ add: "0" }, opId(A, 2));
  s.apply({ add: false }, opId(A, 3));
  s.apply({ add: "\u{1F600}" }, opId(A, 4));
  assert.equal(s.size, 4);
  assert.deepEqual(s.values(), [false, 0, "0", "\u{1F600}"]);
});

test("element order is code point order, not UTF-16 order (D50)", () => {
  // U+FF01 is below U+1F600 by code point, above it by UTF-16 unit.
  assert.equal(compareElements("！", "\u{1F600}"), -1);
  const s = new AddWinsSet();
  s.apply({ add: "\u{1F600}" }, opId(A, 1));
  s.apply({ add: "！" }, opId(A, 2));
  s.apply({ add: "a" }, opId(A, 3));
  s.apply({ add: 2 }, opId(A, 4));
  s.apply({ add: -1 }, opId(A, 5));
  s.apply({ add: true }, opId(A, 6));
  assert.deepEqual(s.values(), [true, -1, 2, "a", "！", "\u{1F600}"]);
});

// Seeded PRNG so failures are reproducible.
function rng(seed) {
  let x = seed >>> 0 || 1;
  return () => {
    x ^= x << 13;
    x >>>= 0;
    x ^= x >>> 17;
    x ^= x << 5;
    x >>>= 0;
    return x / 0x100000000;
  };
}

// Builds a causal history: each replica issues ops against its own view, and
// sees other replicas' ops in some delivery order. Then every replica applies
// all ops in a different causal order; states must converge.
test("convergence: random causal delivery orders give equal states", () => {
  const ELEMS = ["a", "b", "c"];
  for (let seed = 1; seed <= 200; seed++) {
    const rand = rng(seed);
    const replicas = [1, 2, 3].map((r) => ({ id: r, seq: 0, view: new AddWinsSet(), seen: new Set() }));
    const log = []; // { op, id, deps: Set of ids }
    for (let step = 0; step < 30; step++) {
      const rep = replicas[Math.floor(rand() * replicas.length)];
      // Deliver some random already logged ops to this replica, causally.
      for (const entry of log) {
        if (!rep.seen.has(entry.id) && rand() < 0.5 && [...entry.deps].every((d) => rep.seen.has(d))) {
          rep.view.apply(entry.op, entry.id);
          rep.seen.add(entry.id);
        }
      }
      const e = ELEMS[Math.floor(rand() * ELEMS.length)];
      const ops = rand() < 0.55 ? [AddWinsSet.addOp(e)] : rep.view.removeOps(e);
      for (const op of ops) {
        rep.seq += 1;
        const id = opId(rep.id, rep.seq);
        const entry = { op, id, deps: new Set(rep.seen) };
        rep.view.apply(op, id);
        rep.seen.add(id);
        log.push(entry);
      }
    }
    const states = new Set();
    for (let k = 0; k < 4; k++) {
      const s = new AddWinsSet();
      const done = new Set();
      const pending = [...log];
      while (pending.length > 0) {
        const ready = pending.filter((x) => [...x.deps].every((d) => done.has(d)));
        const pick = ready[Math.floor(rand() * ready.length)];
        s.apply(pick.op, pick.id);
        done.add(pick.id);
        pending.splice(pending.indexOf(pick), 1);
      }
      states.add(s.encodeState());
    }
    assert.equal(states.size, 1, "seed " + seed);
  }
});
