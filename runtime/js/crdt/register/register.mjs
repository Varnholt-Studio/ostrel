// Last writer wins (LWW) register.
//
// Every scalar field, reference, enum, `Rank` and whole value `List` merges with
// this register (ARCHITECTURE 5.1, G6). A register is a plain frozen object:
//
//   { value, hlc }
//
// `value` is the field's wire value (JSON: null, boolean, number, string or array).
// `hlc` is the hybrid logical clock of the write that produced the value, encoded
// as the 32 character lowercase hex string of ARCHITECTURE 5.3, or `null` for a
// register that has never been written. A register without an HLC therefore always
// holds `null`: `{ value: 5, hlc: null }` is not a register and is rejected.
//
// A write op and a register state have the same shape, so applying an op and
// merging two replicas are the same function: `merge`. The write with the higher
// HLC wins. HLCs are totally ordered (wall time, then counter, then replica), so no
// further tie breaker is needed.
//
// All functions are pure: they never modify their arguments and always return a
// frozen register.

/** Length of an encoded HLC: wall_ms (12) + counter (4) + replica (16). */
export const HLC_HEX_LENGTH = 32;

const HLC_PATTERN = /^[0-9a-f]{32}$/;

/** The state of a register that no write has reached yet. */
export const EMPTY = Object.freeze({ value: null, hlc: null });

/**
 * Returns true when `hlc` is a correctly encoded HLC: exactly 32 lowercase hex
 * characters. Any other width, uppercase or non hex character is invalid.
 */
export function isValidHlc(hlc) {
  return typeof hlc === "string" && HLC_PATTERN.test(hlc);
}

/**
 * Compares two encoded HLCs and returns a negative number, zero or a positive
 * number. The encoding is fixed width and big endian in comparison order, so the
 * order of the strings is the order of the clocks. Both strings are ASCII, where
 * UTF-16 order and code point order agree (D50 concerns `Text` values, not ids).
 */
export function compareHlc(a, b) {
  if (a === b) return 0;
  return a < b ? -1 : 1;
}

/**
 * Creates the register state written by one op. Throws a TypeError when the HLC
 * is not a valid encoding, because an op without a valid clock can never be merged
 * deterministically, and when the value is `undefined`, which has no wire form
 * (JSON drops the field, so the register would not survive a round trip).
 */
export function write(value, hlc) {
  if (value === undefined) {
    throw new TypeError("invalid value: undefined is not a wire value, use null");
  }
  if (!isValidHlc(hlc)) {
    throw new TypeError(`invalid HLC: expected ${HLC_HEX_LENGTH} lowercase hex characters`);
  }
  return Object.freeze({ value, hlc });
}

/**
 * Merges two registers (or a register and a write op) and returns the winner.
 *
 * The merge is commutative, associative and idempotent, so every replica that
 * received the same writes in any order ends with the same register.
 *
 * Two different values under the same HLC cannot come from a correct replica: an
 * HLC names exactly one op of exactly one replica. Picking either value would let
 * replicas diverge silently, so this case throws a RangeError instead.
 */
export function merge(a, b) {
  checkRegister(a, "first");
  checkRegister(b, "second");

  if (a.hlc === null) return freeze(b);
  if (b.hlc === null) return freeze(a);

  const order = compareHlc(a.hlc, b.hlc);
  if (order > 0) return freeze(a);
  if (order < 0) return freeze(b);

  if (!sameValue(a.value, b.value)) {
    throw new RangeError(`conflicting values for the same HLC ${a.hlc}`);
  }
  return freeze(a);
}

/** Merges any number of registers or writes, starting from `EMPTY`. */
export function mergeAll(registers) {
  let result = EMPTY;
  for (const register of registers) {
    result = merge(result, register);
  }
  return result;
}

/**
 * Creates a replica in the model shape of the shared CRDT vectors
 * (tests/crdt-vectors/README.md): `apply` takes one op envelope
 * `{ id, hlc, op: { set: value } }` as it travels on the wire, `value` returns what
 * a program reads and `state` the full register `{ hlc, value }`.
 *
 * A malformed envelope throws a TypeError and leaves the replica unchanged.
 */
export function createReplica() {
  let current = EMPTY;
  return {
    apply(envelope) {
      const { hlc, op } = checkEnvelope(envelope);
      current = merge(current, write(op.set, hlc));
    },
    value: () => current.value,
    state: () => ({ hlc: current.hlc, value: current.value }),
  };
}

const OP_ID_PATTERN = /^[0-9a-f]{24}$/;

function checkEnvelope(envelope) {
  if (!isPlainObject(envelope)) throw new TypeError("an op must be an object");
  const { id, hlc, op } = envelope;
  if (typeof id !== "string" || !OP_ID_PATTERN.test(id)) {
    throw new TypeError("invalid op id: expected 24 lowercase hex characters");
  }
  if (!isValidHlc(hlc)) {
    throw new TypeError(`invalid HLC: expected ${HLC_HEX_LENGTH} lowercase hex characters`);
  }
  const fields = isPlainObject(op) ? Object.keys(op) : [];
  if (fields.length !== 1 || fields[0] !== "set") {
    throw new TypeError('a register op body is exactly { "set": value }');
  }
  return { hlc, op };
}

function isPlainObject(candidate) {
  return candidate !== null && typeof candidate === "object" && !Array.isArray(candidate);
}

// A register is valid when it has both fields, a valid HLC or none, and a value
// that can travel on the wire. An unwritten register (no HLC) must hold null:
// otherwise `merge(x, EMPTY)` and `merge(EMPTY, x)` would disagree and replicas
// holding such a state would diverge silently instead of failing.
function checkRegister(register, position) {
  const isObject = register !== null && typeof register === "object";
  if (!isObject || !("value" in register) || !("hlc" in register)) {
    throw new TypeError(`${position} argument is not a register`);
  }
  if (register.value === undefined) {
    throw new TypeError(`${position} argument has the value undefined, which is not a wire value`);
  }
  if (register.hlc === null) {
    if (register.value !== null) {
      throw new TypeError(`${position} argument has a value but no HLC`);
    }
    return;
  }
  if (!isValidHlc(register.hlc)) {
    throw new TypeError(`${position} argument has an invalid HLC`);
  }
}

function freeze(register) {
  if (Object.isFrozen(register)) return register;
  return Object.freeze({ value: register.value, hlc: register.hlc });
}

// Structural equality for wire values. Objects do not occur as register values
// (a `Map` travels as an array of pairs), so arrays and primitives are enough.
function sameValue(a, b) {
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((item, index) => sameValue(item, b[index]));
  }
  return a === b;
}
