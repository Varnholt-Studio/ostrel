import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  MAX_COUNTER,
  MAX_SEQ,
  MAX_WALL_MS,
  decodeHlc,
  decodeOpId,
  encodeHlc,
  encodeOpId,
  initialClock,
  nextLocal,
  observe,
  resumeAfter,
} from './clock.mjs';

const REPLICA = '00000000000000a1';

test('first op of a replica gets seq 1 and the current time', () => {
  const next = nextLocal(initialClock(), 1000);
  assert.deepEqual(next.clock, { seq: 1, wallMs: 1000, counter: 0 });
  assert.equal(next.seq, 1);
});

test('ops in the same millisecond count up the HLC counter', () => {
  const first = nextLocal(initialClock(), 1000);
  const second = nextLocal(first.clock, 1000);
  assert.deepEqual(second.clock, { seq: 2, wallMs: 1000, counter: 1 });
});

test('a clock that went backwards never lowers the HLC', () => {
  const first = nextLocal(initialClock(), 5000);
  const second = nextLocal(first.clock, 4000);
  assert.deepEqual(second.clock, { seq: 2, wallMs: 5000, counter: 1 });
});

test('a full counter moves to the next millisecond', () => {
  const clock = { seq: 7, wallMs: 1000, counter: MAX_COUNTER };
  assert.deepEqual(nextLocal(clock, 1000).clock, { seq: 8, wallMs: 1001, counter: 0 });
});

test('seq and wall time exhaustion are errors, not wrap arounds', () => {
  assert.throws(() => nextLocal({ seq: MAX_SEQ, wallMs: 0, counter: 0 }, 1), RangeError);
  assert.throws(
    () => nextLocal({ seq: 1, wallMs: MAX_WALL_MS, counter: MAX_COUNTER }, MAX_WALL_MS),
    RangeError,
  );
});

test('invalid clock states and times are rejected', () => {
  assert.throws(() => nextLocal({ seq: -1, wallMs: 0, counter: 0 }, 1), RangeError);
  assert.throws(() => nextLocal({ seq: 0, wallMs: 0.5, counter: 0 }, 1), RangeError);
  assert.throws(() => nextLocal(initialClock(), Number.NaN), RangeError);
  assert.throws(() => nextLocal(null, 1), TypeError);
});

test('observe moves the clock past a later remote stamp', () => {
  const clock = { seq: 3, wallMs: 1000, counter: 2 };
  const after = observe(clock, { wallMs: 9000, counter: 5 }, 1500);
  assert.deepEqual(after, { seq: 3, wallMs: 9000, counter: 5 });
  const next = nextLocal(after, 1500);
  assert.deepEqual(next.clock, { seq: 4, wallMs: 9000, counter: 6 });
});

test('observe keeps the local clock when the remote stamp is older', () => {
  const clock = { seq: 3, wallMs: 1000, counter: 2 };
  assert.deepEqual(observe(clock, { wallMs: 900, counter: 9 }, 500), clock);
});

test('observe prefers the current time when it is later than both', () => {
  const clock = { seq: 3, wallMs: 1000, counter: 2 };
  assert.deepEqual(observe(clock, { wallMs: 1200, counter: 0 }, 2000), {
    seq: 3,
    wallMs: 2000,
    counter: 0,
  });
});

test('resumeAfter raises seq but never lowers it', () => {
  const clock = { seq: 3, wallMs: 1000, counter: 2 };
  assert.equal(resumeAfter(clock, 10).seq, 10);
  assert.equal(resumeAfter(clock, 1).seq, 3);
});

test('OpId wire form: replica then seq, fixed width, lowercase', () => {
  assert.equal(encodeOpId(REPLICA, 1), '00000000000000a100000001');
  assert.equal(encodeOpId(REPLICA, MAX_SEQ), '00000000000000a1ffffffff');
  assert.deepEqual(decodeOpId('00000000000000a10000002a'), { replica: REPLICA, seq: 42 });
  assert.throws(() => encodeOpId(REPLICA, 0), RangeError);
  assert.throws(() => encodeOpId('00000000000000A1', 1), TypeError);
  assert.throws(() => decodeOpId('00000000000000A100000001'), TypeError);
  assert.throws(() => decodeOpId('00000000000000a1000001'), TypeError);
});

test('Hlc wire form: wall 12, counter 4, replica 16', () => {
  const text = encodeHlc({ wallMs: 0x1234, counter: 3, replica: REPLICA });
  assert.equal(text, '000000001234' + '0003' + REPLICA);
  assert.deepEqual(decodeHlc(text), { wallMs: 0x1234, counter: 3, replica: REPLICA });
  assert.throws(() => decodeHlc(text.toUpperCase()), TypeError);
  assert.throws(() => decodeHlc(text + '0'), TypeError);
  assert.throws(() => encodeHlc({ wallMs: MAX_WALL_MS + 1, counter: 0, replica: REPLICA }));
});

test('string order of encodings equals value order', () => {
  const stamps = [
    { wallMs: 1, counter: 0 },
    { wallMs: 1, counter: 1 },
    { wallMs: 1, counter: MAX_COUNTER },
    { wallMs: 2, counter: 0 },
    { wallMs: 0x10000, counter: 0 },
  ].map((stamp) => encodeHlc({ ...stamp, replica: REPLICA }));
  assert.deepEqual([...stamps].sort(), stamps);
  const opIds = [1, 2, 15, 16, 255, 256, MAX_SEQ].map((seq) => encodeOpId(REPLICA, seq));
  assert.deepEqual([...opIds].sort(), opIds);
});
