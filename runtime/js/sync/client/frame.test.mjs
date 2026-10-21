// Tests for the client side frame codec. Literal frames marked "same as Rust" are copied from
// `crates/ostrel_sync/tests/wire_json.rs`, so both codecs read and write the same bytes.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  ERROR_CODES,
  LIMIT_KEYS,
  SESSION_CODES,
  WireError,
  decodeServer,
  encodeClient,
  opBytes,
} from './frame.mjs';

const REPLICA = '0123456789abcdef';
const OP_2 = REPLICA + '00000002';
const ROW = '0000000003e80000' + REPLICA;
const HLC = '0000000003e80001' + REPLICA;
const SEQ = '000000000000002a';

function limitsJson(overrides = {}) {
  const entries = LIMIT_KEYS.map((key) => `"${key}":${overrides[key] ?? 1000}`);
  return `{${entries.join(',')}}`;
}

function welcome(limits = limitsJson()) {
  return `{"limits":${limits},"replica":"${REPLICA}","replica_secret":null,"server_ms":1700000000000,"t":"welcome","v":0}`;
}

function errorFrame(sub, code, ms) {
  return `{"v":0,"t":"error","sub":${sub},"code":"${code}","retry_after_ms":${ms}}`;
}

// Asserts that decoding throws a WireError of `kind` (and `key`, if given).
function refuses(frame, kind, key, options) {
  assert.throws(
    () => decodeServer(frame, options),
    (error) => {
      assert.ok(error instanceof WireError, `${error}`);
      assert.equal(error.kind, kind, error.message);
      if (key !== undefined) assert.equal(error.key, key);
      return true;
    },
  );
}

test('reads the session control messages', () => {
  const w = decodeServer(welcome());
  assert.equal(w.t, 'welcome');
  assert.equal(w.replica, REPLICA);
  assert.equal(w.replica_secret, null);
  assert.equal(w.server_ms, 1700000000000);
  assert.deepEqual(Object.keys(w.limits), LIMIT_KEYS);

  assert.deepEqual(
    decodeServer(`{"op":"${OP_2}","restamped":[["${OP_2}","${HLC}"]],"seq":"${SEQ}","t":"ack","v":0}`),
    { t: 'ack', op: OP_2, seq: SEQ, restamped: [[OP_2, HLC]] },
  );
  // Same as Rust.
  assert.deepEqual(
    decodeServer('{"op":"0123456789abcdef00000002","reason":"Forged","t":"reject","v":0}'),
    { t: 'reject', op: OP_2, reason: 'Forged' },
  );
  assert.deepEqual(decodeServer(`{"op":"${OP_2}","retry_after_ms":250,"t":"defer","v":0}`), {
    t: 'defer',
    op: OP_2,
    retry_after_ms: 250,
  });
  assert.deepEqual(
    decodeServer(`{"model":"Issue","rows":["${ROW}"],"sub":3,"t":"evict","v":0}`, {
      models: ['Issue'],
    }),
    { t: 'evict', sub: 3, model: 'Issue', rows: [ROW] },
  );
  // Same as Rust.
  assert.deepEqual(decodeServer('{"sub":7,"t":"reset","v":0}'), { t: 'reset', sub: 7 });
});

test('error requires the key retry_after_ms like every other key (D159, dana #2591)', () => {
  // Same as Rust: the frames the server writes for the session limits.
  assert.deepEqual(
    decodeServer('{"code":"RateLimited","retry_after_ms":250,"sub":9,"t":"error","v":0}'),
    { t: 'error', sub: 9, code: 'RateLimited', retry_after_ms: 250 },
  );
  for (const code of SESSION_CODES) {
    assert.deepEqual(
      decodeServer(`{"code":"${code}","retry_after_ms":null,"sub":null,"t":"error","v":0}`),
      { t: 'error', sub: null, code, retry_after_ms: null },
    );
  }
  // Absent is not the same as null: the strict key rule of wire/mod.rs has no exception.
  refuses('{"code":"Protocol","sub":null,"t":"error","v":0}', 'MissingKey', 'retry_after_ms');
  refuses('{"code":"RateLimited","sub":9,"t":"error","v":0}', 'MissingKey', 'retry_after_ms');
  refuses('{"code":"Protocol","retry_after_ms":null,"t":"error","v":0}', 'MissingKey', 'sub');
  refuses(
    '{"code":"Protocol","retry_after_ms":null,"sub":null,"t":"error","v":0,"retry":1}',
    'UnknownKey',
    'retry',
  );
});

test('error fields must fit the code, as ProtocolError::is_well_formed', () => {
  // Same as Rust (an_error_whose_fields_do_not_fit_its_code_is_refused).
  refuses(errorFrame('3', 'Protocol', '10'), 'IllFormedError');
  refuses(errorFrame('null', 'RateLimited', '10'), 'IllFormedError');
  refuses(errorFrame('3', 'RateLimited', 'null'), 'IllFormedError');
  refuses(errorFrame('3', 'Overloaded', 'null'), 'IllFormedError');
  refuses(errorFrame('3', 'RateLimited', '-1'), 'FieldRange', 'retry_after_ms');
  assert.equal(decodeServer(errorFrame('3', 'RateLimited', '0')).retry_after_ms, 0);
  // Every code without a delay is well formed with and without a subscription, except the
  // session codes, which never name one.
  for (const code of ERROR_CODES.filter((c) => c !== 'RateLimited')) {
    assert.equal(decodeServer(errorFrame('null', code, 'null')).code, code);
    if (!SESSION_CODES.includes(code)) {
      assert.equal(decodeServer(errorFrame('4', code, 'null')).sub, 4);
    }
  }
  refuses(errorFrame('null', 'NoSuchCode', 'null'), 'WrongType', 'code');
});

test('a faulty envelope is refused and closes the connection', () => {
  // Same as Rust.
  refuses('{"t":"reset","sub":7}', 'MissingKey', 'v');
  refuses('{"v":1,"t":"reset","sub":7}', 'UnsupportedVersion');
  refuses('{"v":"0","t":"reset","sub":7}', 'WrongType', 'v');
  refuses('{"v":1,"t":"future"}', 'UnsupportedVersion');
  refuses('{"v":0,"sub":7}', 'MissingKey', 't');
  refuses('{"v":0,"t":7,"sub":7}', 'WrongType', 't');
  refuses('{"v":0,"t":"unsubscribe","sub":7}', 'UnknownMessage');
  refuses('{"v":0,"t":"Reset","sub":7}', 'UnknownMessage');
  refuses('{"v":0,"t":"reset","sub":7,"extra":true}', 'UnknownKey', 'extra');
  refuses('{"sub":7,"sub":8,"t":"reset","v":0}', 'DuplicateKey', 'sub');
  refuses('{"v":0,"v":0,"t":"reset","sub":7}', 'DuplicateKey', 'v');
  refuses('[1]', 'NotAnObject');
  for (const malformed of [
    '{"sub":7,"t":"reset","v":0} x',
    '{"sub":7,"t":"reset","v":0,}',
    '{"sub":07,"t":"reset","v":0}',
    "{'sub':7}",
    '{"sub":NaN,"t":"reset","v":0}',
    '{"sub":1e400,"t":"reset","v":0}',
  ]) {
    refuses(malformed, 'Malformed');
  }
  const error = (() => {
    try {
      decodeServer('{"v":1}');
    } catch (e) {
      return e;
    }
    return null;
  })();
  assert.equal(error.closesConnection, true);
});

test('integer fields follow the safe range and the integer form (D24)', () => {
  assert.equal(decodeServer('{"sub":4294967295,"t":"reset","v":0}').sub, 4294967295);
  refuses('{"sub":4294967296,"t":"reset","v":0}', 'FieldRange', 'sub');
  refuses('{"sub":-1,"t":"reset","v":0}', 'FieldRange', 'sub');
  refuses('{"sub":7.0,"t":"reset","v":0}', 'WrongType', 'sub');
  refuses('{"sub":7e0,"t":"reset","v":0}', 'WrongType', 'sub');
  refuses('{"sub":1e20,"t":"reset","v":0}', 'NumberOutOfRange');
  refuses('{"sub":9007199254740992,"t":"reset","v":0}', 'NumberOutOfRange');
  assert.equal(decodeServer('{"sub":-0,"t":"reset","v":0}').sub, 0);
});

test('ids are strict lowercase hex of their width', () => {
  refuses(`{"op":"${OP_2.toUpperCase()}","reason":"Denied","t":"reject","v":0}`, 'InvalidId', 'op');
  refuses(`{"op":"${OP_2}0","reason":"Denied","t":"reject","v":0}`, 'InvalidId', 'op');
  refuses(`{"op":"${OP_2}","restamped":[["${OP_2}"]],"seq":"${SEQ}","t":"ack","v":0}`, 'WrongType');
  refuses(`{"op":"${OP_2}","reason":"Nope","t":"reject","v":0}`, 'WrongType', 'reason');
});

test('the limits table of welcome has exactly its keys', () => {
  const missing = limitsJson().replace('"rank_digits":1000', '"x":1');
  refuses(welcome(missing), 'MissingKey', 'rank_digits');
  const extra = limitsJson().replace('}', ',"more":1}');
  refuses(welcome(extra), 'UnknownKey', 'more');
  refuses(welcome(limitsJson({ frame_bytes: -1 })), 'FieldRange', 'frame_bytes');
});

test('an evict for a model the schema does not have is fatal', () => {
  refuses(`{"model":"Nope","rows":[],"sub":3,"t":"evict","v":0}`, 'UnknownModel', 'model', {
    models: ['Issue'],
  });
});

test('a snapshot or return for no open request is dropped, the rest is unsupported', () => {
  const snapshot = `{"last":true,"rows":[],"seq":"${SEQ}","sub":5,"t":"snapshot","v":0}`;
  const ret = '{"call":2,"result":{"ok":1},"t":"return","v":0}';
  for (const [frame, kind] of [
    [snapshot, 'UnknownSubscription'],
    [ret, 'UnknownCall'],
  ]) {
    try {
      decodeServer(frame);
      assert.fail('expected a WireError');
    } catch (error) {
      assert.equal(error.kind, kind);
      assert.equal(error.closesConnection, false);
    }
  }
  const open = { subscriptionModel: () => 'Issue', callFunction: () => 'f' };
  refuses(snapshot, 'Unsupported', undefined, { requests: open });
  refuses(ret, 'Unsupported', undefined, { requests: open });
  refuses('{"ops":[],"t":"ops","v":0}', 'Unsupported');
  // The keys are still checked first.
  refuses('{"call":2,"t":"return","v":0}', 'MissingKey', 'result');
});

test('a frame over the frame limit is refused before parsing (D121)', () => {
  refuses('{"sub":7,"t":"reset","v":0}', 'FrameTooLarge', undefined, { frameBytes: 10 });
  // Bytes, not UTF-16 units: four two byte characters are eight bytes.
  const text = `{"sub":7,"t":"reset","v":0,"x":"${'é'.repeat(4)}"}`;
  refuses(text, 'FrameTooLarge', undefined, { frameBytes: text.length + 3 });
});

test('writes hello canonically, like the Rust encoder', () => {
  // Same as Rust.
  assert.equal(
    encodeClient({ t: 'hello', token: null, replica: REPLICA, replica_secret: null, last_server_seq: null }),
    '{"last_server_seq":null,"replica":"0123456789abcdef","replica_secret":null,"t":"hello","token":null,"v":0}',
  );
  assert.throws(
    () => encodeClient({ t: 'hello', replica_secret: 'ab'.repeat(16) }),
    (e) => e instanceof WireError && e.key === 'replica_secret',
  );
  assert.throws(() => encodeClient({ t: 'hello', replica: 'ABC' }), /replica/);
});

test('writes unsubscribe, held and ops', () => {
  assert.equal(encodeClient({ t: 'unsubscribe', sub: 7 }), '{"sub":7,"t":"unsubscribe","v":0}');
  assert.equal(
    encodeClient({ t: 'held', sub: 1, rows: [ROW], last: true }),
    `{"last":true,"rows":["${ROW}"],"sub":1,"t":"held","v":0}`,
  );
  const op = { id: OP_2, hlc: HLC, model: 'Issue', row: ROW, body: { set: { field: 'title', value: 'a' } } };
  assert.equal(
    encodeClient({ t: 'ops', ops: [op] }),
    `{"ops":[{"body":{"set":{"field":"title","value":"a"}},"hlc":"${HLC}","id":"${OP_2}","model":"Issue","row":"${ROW}"}],"t":"ops","v":0}`,
  );
  assert.equal(opBytes(op), JSON.stringify(JSON.parse(encodeClient({ t: 'ops', ops: [op] })).ops[0]).length);
  assert.throws(
    () => encodeClient({ t: 'ops', ops: [op] }, { opBytes: opBytes(op) - 1 }),
    (e) => e.kind === 'OpTooLarge',
  );
  assert.throws(
    () => encodeClient({ t: 'ops', ops: [op] }, { frameBytes: 50 }),
    (e) => e.kind === 'FrameTooLarge',
  );
  assert.throws(() => encodeClient({ t: 'ops', ops: [{ ...op, id: 'x' }] }), (e) => e.kind === 'InvalidId');
  assert.throws(() => encodeClient({ t: 'subscribe', sub: 1 }), (e) => e.kind === 'Unsupported');
  assert.throws(() => encodeClient({ t: 'unsubscribe', sub: -1 }), (e) => e.kind === 'FieldRange');
});
