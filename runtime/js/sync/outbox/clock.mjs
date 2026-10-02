// Replica clock for the outbox: per replica op sequence and hybrid logical clock (HLC).
//
// Layout and wire forms follow ARCHITECTURE 5.3:
//   ReplicaId  u64, 16 lowercase hex digits
//   OpId       { replica, seq: u32 }, 24 hex digits (replica 16, then seq 8)
//   Hlc        { wall_ms: u48, counter: u16, replica }, 32 hex digits (wall 12, counter 4, replica 16)
// All fields are big endian in comparison order, so comparing two encodings as strings
// compares the values.
//
// Everything in this module is pure: no IndexedDB, no clock reads. The outbox calls these
// functions inside its IndexedDB transaction, which is what makes allocation atomic.

export const MAX_SEQ = 0xffffffff;
export const MAX_WALL_MS = 2 ** 48 - 1;
export const MAX_COUNTER = 0xffff;

const REPLICA_HEX = /^[0-9a-f]{16}$/;
const OP_ID_HEX = /^[0-9a-f]{24}$/;
const HLC_HEX = /^[0-9a-f]{32}$/;

/** The clock state of a replica before its first op. */
export function initialClock() {
  return { seq: 0, wallMs: 0, counter: 0 };
}

/** Throws unless `replica` is a ReplicaId in its wire form. */
export function checkReplica(replica) {
  if (typeof replica !== 'string' || !REPLICA_HEX.test(replica)) {
    throw new TypeError('replica must be 16 lowercase hex digits');
  }
}

/** Encodes an OpId. */
export function encodeOpId(replica, seq) {
  checkReplica(replica);
  checkInteger(seq, 1, MAX_SEQ, 'seq');
  return replica + hex(seq, 8);
}

/** Decodes an OpId; throws on any other width, uppercase or non hex character. */
export function decodeOpId(text) {
  if (typeof text !== 'string' || !OP_ID_HEX.test(text)) {
    throw new TypeError('OpId must be 24 lowercase hex digits');
  }
  return { replica: text.slice(0, 16), seq: Number.parseInt(text.slice(16), 16) };
}

/** Encodes an Hlc. */
export function encodeHlc({ wallMs, counter, replica }) {
  checkInteger(wallMs, 0, MAX_WALL_MS, 'wallMs');
  checkInteger(counter, 0, MAX_COUNTER, 'counter');
  checkReplica(replica);
  return hex(wallMs, 12) + hex(counter, 4) + replica;
}

/** Decodes an Hlc; throws on any other width, uppercase or non hex character. */
export function decodeHlc(text) {
  if (typeof text !== 'string' || !HLC_HEX.test(text)) {
    throw new TypeError('Hlc must be 32 lowercase hex digits');
  }
  return {
    wallMs: Number.parseInt(text.slice(0, 12), 16),
    counter: Number.parseInt(text.slice(12, 16), 16),
    replica: text.slice(16),
  };
}

/**
 * Allocates the next local op: the next seq and an HLC that is strictly greater than every
 * HLC this replica issued or observed before.
 * Returns the new clock state and the stamp of the op.
 */
export function nextLocal(clock, nowMs) {
  checkClock(clock);
  checkInteger(nowMs, 0, MAX_WALL_MS, 'nowMs');
  if (clock.seq >= MAX_SEQ) {
    throw new RangeError('seq space of this replica is exhausted');
  }
  const time = tick(clock.wallMs, clock.counter, nowMs);
  const next = { seq: clock.seq + 1, wallMs: time.wallMs, counter: time.counter };
  return { clock: next, seq: next.seq, wallMs: next.wallMs, counter: next.counter };
}

/**
 * Advances the clock past a stamp seen from elsewhere (a remote op or a re-stamped Ack), so
 * later local ops sort after it. The seq is not touched.
 */
export function observe(clock, stamp, nowMs) {
  checkClock(clock);
  checkInteger(stamp.wallMs, 0, MAX_WALL_MS, 'stamp.wallMs');
  checkInteger(stamp.counter, 0, MAX_COUNTER, 'stamp.counter');
  checkInteger(nowMs, 0, MAX_WALL_MS, 'nowMs');
  const later =
    stamp.wallMs > clock.wallMs ||
    (stamp.wallMs === clock.wallMs && stamp.counter > clock.counter);
  const base = later ? stamp : clock;
  if (nowMs > base.wallMs) {
    return { seq: clock.seq, wallMs: nowMs, counter: 0 };
  }
  return { seq: clock.seq, wallMs: base.wallMs, counter: base.counter };
}

/**
 * Raises the seq to at least `lastSeq`, for example after the server reports the last seq it
 * accepted from this replica. Never lowers it.
 */
export function resumeAfter(clock, lastSeq) {
  checkClock(clock);
  checkInteger(lastSeq, 0, MAX_SEQ, 'lastSeq');
  return { seq: Math.max(clock.seq, lastSeq), wallMs: clock.wallMs, counter: clock.counter };
}

// The smallest (wallMs, counter) strictly after the given one, preferring the current time.
function tick(wallMs, counter, nowMs) {
  if (nowMs > wallMs) {
    return { wallMs: nowMs, counter: 0 };
  }
  if (counter < MAX_COUNTER) {
    return { wallMs, counter: counter + 1 };
  }
  if (wallMs >= MAX_WALL_MS) {
    throw new RangeError('HLC wall time is exhausted');
  }
  return { wallMs: wallMs + 1, counter: 0 };
}

function checkClock(clock) {
  if (clock === null || typeof clock !== 'object') {
    throw new TypeError('clock must be an object');
  }
  checkInteger(clock.seq, 0, MAX_SEQ, 'clock.seq');
  checkInteger(clock.wallMs, 0, MAX_WALL_MS, 'clock.wallMs');
  checkInteger(clock.counter, 0, MAX_COUNTER, 'clock.counter');
}

function checkInteger(value, min, max, name) {
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    throw new RangeError(`${name} must be an integer from ${min} to ${max}`);
  }
}

function hex(value, width) {
  return value.toString(16).padStart(width, '0');
}
