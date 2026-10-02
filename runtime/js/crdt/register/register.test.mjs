import { test } from "node:test";
import assert from "node:assert/strict";

import {
  EMPTY,
  HLC_HEX_LENGTH,
  compareHlc,
  createReplica,
  isValidHlc,
  merge,
  mergeAll,
  write,
} from "./register.mjs";

// Builds an encoded HLC from its three parts (ARCHITECTURE 5.3):
// wall_ms as 12 hex characters, counter as 4, replica as 16.
function hlc(wallMs, counter, replica) {
  return (
    wallMs.toString(16).padStart(12, "0") +
    counter.toString(16).padStart(4, "0") +
    replica.toString(16).padStart(16, "0")
  );
}

// Every ordering of `items`, used to check convergence regardless of arrival order.
function permutations(items) {
  if (items.length <= 1) return [items];
  return items.flatMap((item, index) => {
    const rest = [...items.slice(0, index), ...items.slice(index + 1)];
    return permutations(rest).map((tail) => [item, ...tail]);
  });
}

test("hlc helper produces the fixed width encoding", () => {
  assert.equal(hlc(1, 2, 3).length, HLC_HEX_LENGTH);
  assert.equal(hlc(1, 2, 3), "000000000001" + "0002" + "0000000000000003");
});

test("isValidHlc accepts only 32 lowercase hex characters", () => {
  assert.equal(isValidHlc(hlc(1, 0, 1)), true);
  assert.equal(isValidHlc(hlc(1, 10, 1)), true);
  assert.equal(isValidHlc(hlc(1, 10, 1).toUpperCase()), false);
  assert.equal(isValidHlc("A".repeat(32)), false);
  assert.equal(isValidHlc("g".repeat(32)), false);
  assert.equal(isValidHlc("0".repeat(31)), false);
  assert.equal(isValidHlc("0".repeat(33)), false);
  assert.equal(isValidHlc(12), false);
  assert.equal(isValidHlc(null), false);
});

test("compareHlc orders by wall time, then counter, then replica", () => {
  assert.ok(compareHlc(hlc(1, 9, 9), hlc(2, 0, 0)) < 0);
  assert.ok(compareHlc(hlc(5, 1, 9), hlc(5, 2, 0)) < 0);
  assert.ok(compareHlc(hlc(5, 1, 1), hlc(5, 1, 2)) < 0);
  assert.ok(compareHlc(hlc(5, 1, 2), hlc(5, 1, 1)) > 0);
  assert.equal(compareHlc(hlc(5, 1, 1), hlc(5, 1, 1)), 0);
});

test("compareHlc handles the full 48 bit wall time and 64 bit replica", () => {
  const maxWall = 2 ** 48 - 1;
  assert.ok(compareHlc(hlc(maxWall - 1, 0xffff, 0), hlc(maxWall, 0, 0)) < 0);
  const highReplica = "000000000001" + "0000" + "ffffffffffffffff";
  assert.ok(compareHlc(hlc(1, 0, 0), highReplica) < 0);
});

test("write rejects an invalid HLC", () => {
  assert.throws(() => write("x", "not a clock"), TypeError);
  assert.throws(() => write("x", null), TypeError);
});

test("write rejects undefined, which has no wire form", () => {
  assert.throws(() => write(undefined, hlc(1, 0, 1)), TypeError);
  assert.deepEqual(write(null, hlc(1, 0, 1)), { value: null, hlc: hlc(1, 0, 1) });
});

test("write returns a frozen register", () => {
  const register = write("hello", hlc(1, 0, 1));
  assert.deepEqual(register, { value: "hello", hlc: hlc(1, 0, 1) });
  assert.ok(Object.isFrozen(register));
});

test("EMPTY is the identity of merge", () => {
  const register = write(42, hlc(1, 0, 1));
  assert.equal(merge(EMPTY, register), register);
  assert.equal(merge(register, EMPTY), register);
  assert.deepEqual(merge(EMPTY, EMPTY), EMPTY);
});

test("the later write wins regardless of argument order", () => {
  const older = write("old", hlc(100, 0, 1));
  const newer = write("new", hlc(200, 0, 1));
  assert.equal(merge(older, newer).value, "new");
  assert.equal(merge(newer, older).value, "new");
});

test("an older write arriving late does not overwrite a newer one", () => {
  let state = EMPTY;
  state = merge(state, write("new", hlc(200, 0, 2)));
  state = merge(state, write("old", hlc(100, 0, 1)));
  assert.equal(state.value, "new");
});

test("the counter decides between writes in the same millisecond", () => {
  const first = write("first", hlc(100, 1, 9));
  const second = write("second", hlc(100, 2, 1));
  assert.equal(merge(first, second).value, "second");
  assert.equal(merge(second, first).value, "second");
});

test("the replica breaks a tie of wall time and counter", () => {
  const fromLow = write("low", hlc(100, 1, 1));
  const fromHigh = write("high", hlc(100, 1, 2));
  assert.equal(merge(fromLow, fromHigh).value, "high");
  assert.equal(merge(fromHigh, fromLow).value, "high");
});

test("merging a register with itself is idempotent", () => {
  const register = write(["a", "b"], hlc(7, 0, 3));
  assert.deepEqual(merge(register, register), register);
});

test("redelivery of the same write with an equal copy of the value is accepted", () => {
  const original = write(["a", "b"], hlc(7, 0, 3));
  const copy = write(["a", "b"], hlc(7, 0, 3));
  assert.deepEqual(merge(original, copy), original);
});

test("different values under the same HLC are rejected, not resolved silently", () => {
  const a = write("a", hlc(7, 0, 3));
  const b = write("b", hlc(7, 0, 3));
  assert.throws(() => merge(a, b), RangeError);
  assert.throws(() => merge(b, a), RangeError);
  const listA = write([1, 2], hlc(7, 0, 3));
  const listB = write([1, 3], hlc(7, 0, 3));
  assert.throws(() => merge(listA, listB), RangeError);
});

test("null is a value that can win, distinct from an unwritten register", () => {
  const set = write("text", hlc(1, 0, 1));
  const cleared = write(null, hlc(2, 0, 1));
  const state = merge(set, cleared);
  assert.equal(state.value, null);
  assert.equal(state.hlc, hlc(2, 0, 1));
});

test("merge does not modify its arguments", () => {
  const a = { value: "a", hlc: hlc(1, 0, 1) };
  const b = { value: "b", hlc: hlc(2, 0, 1) };
  const result = merge(a, b);
  assert.deepEqual(a, { value: "a", hlc: hlc(1, 0, 1) });
  assert.deepEqual(b, { value: "b", hlc: hlc(2, 0, 1) });
  assert.ok(Object.isFrozen(result));
  assert.notEqual(result, b);
});

test("merge rejects arguments that are not registers", () => {
  const register = write(1, hlc(1, 0, 1));
  assert.throws(() => merge(register, null), TypeError);
  assert.throws(() => merge(register, { value: 1 }), TypeError);
  assert.throws(() => merge({ hlc: hlc(1, 0, 1) }, register), TypeError);
  assert.throws(() => merge(register, { value: 1, hlc: "XYZ" }), TypeError);
});

test("merge rejects a register that holds a value but has no HLC", () => {
  // Accepting it would break commutativity: merge(x, EMPTY) would keep 5 while
  // merge(EMPTY, x) would return EMPTY, so replicas would diverge silently.
  const broken = { value: 5, hlc: null };
  assert.throws(() => merge(broken, EMPTY), TypeError);
  assert.throws(() => merge(EMPTY, broken), TypeError);
  assert.throws(() => merge(broken, { value: 7, hlc: null }), TypeError);
  assert.throws(() => merge(write(1, hlc(1, 0, 1)), broken), TypeError);
});

test("merge rejects a register whose value is undefined", () => {
  const register = write(1, hlc(1, 0, 1));
  assert.throws(() => merge(register, { value: undefined, hlc: hlc(2, 0, 1) }), TypeError);
  assert.throws(() => merge({ value: undefined, hlc: null }, register), TypeError);
});

test("merge is associative", () => {
  const a = write("a", hlc(3, 0, 1));
  const b = write("b", hlc(3, 1, 1));
  const c = write("c", hlc(2, 9, 2));
  assert.deepEqual(merge(merge(a, b), c), merge(a, merge(b, c)));
});

test("three editors converge on the same register in every arrival order", () => {
  // Concurrent edits by three replicas, including a same millisecond conflict.
  const writes = [
    write("draft", hlc(1000, 0, 1)),
    write("review", hlc(1005, 0, 2)),
    write("done", hlc(1005, 0, 3)),
    write("blocked", hlc(1004, 7, 1)),
  ];
  const expected = write("done", hlc(1005, 0, 3));
  for (const order of permutations(writes)) {
    assert.deepEqual(mergeAll(order), expected);
  }
});

test("mergeAll of nothing is EMPTY", () => {
  assert.deepEqual(mergeAll([]), EMPTY);
});

// Op envelope as in tests/crdt-vectors: `id` is replica (16 hex) then seq (8 hex).
function op(replica, seq, clock, value) {
  const id = replica.toString(16).padStart(16, "0") + seq.toString(16).padStart(8, "0");
  return { id, hlc: clock, op: { set: value } };
}

test("createReplica starts empty", () => {
  const replica = createReplica();
  assert.equal(replica.value(), null);
  assert.deepEqual(replica.state(), { hlc: null, value: null });
});

test("createReplica converges in every delivery order, replays included", () => {
  const ops = [
    op(0xa, 1, hlc(100, 0, 0xa), "a"),
    op(0xb, 1, hlc(100, 0, 0xb), "b"),
    op(0xa, 2, hlc(90, 3, 0xa), "late"),
  ];
  const deliveries = [...permutations(ops), [ops[1], ops[0], ops[1], ops[2], ops[0]]];
  for (const delivery of deliveries) {
    const replica = createReplica();
    for (const envelope of delivery) replica.apply(envelope);
    assert.equal(replica.value(), "b");
    assert.deepEqual(replica.state(), { hlc: hlc(100, 0, 0xb), value: "b" });
  }
});

test("createReplica treats a null write like any other value", () => {
  const replica = createReplica();
  replica.apply(op(0xb, 1, hlc(2, 0, 0xb), null));
  replica.apply(op(0xa, 1, hlc(1, 0, 0xa), "assigned"));
  assert.deepEqual(replica.state(), { hlc: hlc(2, 0, 0xb), value: null });
});

test("createReplica rejects malformed ops and keeps its state", () => {
  const replica = createReplica();
  replica.apply(op(1, 1, hlc(5, 0, 1), "kept"));
  const malformed = [
    null,
    [],
    { ...op(1, 2, hlc(6, 0, 1), "x"), id: "XYZ" },
    { ...op(1, 2, hlc(6, 0, 1), "x"), hlc: "not a clock" },
    { ...op(1, 2, hlc(6, 0, 1), "x"), op: { put: ["k", "v"] } },
    { ...op(1, 2, hlc(6, 0, 1), "x"), op: { set: "x", extra: 1 } },
    { ...op(1, 2, hlc(6, 0, 1), "x"), op: { set: undefined } },
    { ...op(1, 2, hlc(6, 0, 1), "x"), op: null },
  ];
  for (const envelope of malformed) {
    assert.throws(() => replica.apply(envelope), TypeError);
  }
  assert.deepEqual(replica.state(), { hlc: hlc(5, 0, 1), value: "kept" });
});

test("createReplica rejects a conflicting value under a known HLC and keeps its state", () => {
  const replica = createReplica();
  replica.apply(op(1, 1, hlc(5, 0, 1), "first"));
  assert.throws(() => replica.apply(op(1, 1, hlc(5, 0, 1), "other")), RangeError);
  assert.equal(replica.value(), "first");
});
