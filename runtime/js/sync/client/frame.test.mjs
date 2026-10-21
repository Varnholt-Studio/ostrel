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
import { ROOMS_SCHEMA } from './testing/fake_server.mjs';
import { compileSchema } from './value.mjs';

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

test('without the schema a snapshot, return or ops is unsupported; no open request is dropped', () => {
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
  assert.throws(() => encodeClient({ t: 'welcome' }), (e) => e.kind === 'Unsupported');
  assert.throws(() => encodeClient({ t: 'unsubscribe', sub: -1 }), (e) => e.kind === 'FieldRange');
});

// ---------------------------------------------------------------------------------------------
// Typed messages (D132), with the schema of the Rust codec tests

const schema = compileSchema(ROOMS_SCHEMA);
const PERSON_2 = '0000000003e90000' + REPLICA;
const PERSON_3 = '0000000003ea0000' + REPLICA;
const TAG_1 = REPLICA + '00000001';
const TAG_4 = REPLICA + '00000004';
const OPEN = { subscriptionModel: (sub) => (sub === 1 ? 'Room' : null), callFunction: (call) => ['count', 'touch', 'rename', 'names'][call] ?? null };
const typed = { schema, requests: OPEN };

function snapshotFrame(rows, sub = 1) {
  return `{"last":true,"rows":[${rows.join(',')}],"seq":"0000000000000005","sub":${sub},"t":"snapshot","v":0}`;
}

test('reads snapshot rows typed by the model of their subscription', () => {
  // Same as Rust (a_snapshot_row_groups_set_tags_by_element_and_hides_the_version), plus a map
  // entry, a removed key and the implicit field made.
  const row =
    `{"fields":{"made":1700000000000,"name":"general","topic":null},"id":"${ROW}",` +
    `"maps":{"roles":[["${PERSON_2}",{"hlc":"${HLC}","value":"admin"}],["${PERSON_3}",{"hlc":"${HLC}","removed":true}]]},` +
    `"sets":{"members":[["${PERSON_2}",["${TAG_1}","${TAG_4}"]],["${PERSON_3}",["${OP_2}"]]]}}`;
  assert.deepEqual(decodeServer(snapshotFrame([row]), typed), {
    t: 'snapshot',
    sub: 1,
    model: 'Room',
    rows: [
      {
        id: ROW,
        fields: { made: 1700000000000, name: 'general', topic: null },
        sets: { members: [{ elem: PERSON_2, tags: [TAG_1, TAG_4] }, { elem: PERSON_3, tags: [OP_2] }] },
        maps: { roles: [{ key: PERSON_2, hlc: HLC, value: 'admin' }, { key: PERSON_3, hlc: HLC, removed: true }] },
      },
    ],
    seq: '0000000000000005',
    last: true,
  });
  // No subscription 2: dropped, not fatal (D155).
  refuses(snapshotFrame([], 2), 'UnknownSubscription', undefined, typed);
});

test('a snapshot row that does not fit the schema closes the connection', () => {
  const rowWith = ({ fields = '{}', sets = '{}', maps = '{}' }) =>
    `{"fields":${fields},"id":"${ROW}","maps":${maps},"sets":${sets}}`;
  const cases = [
    [rowWith({ fields: '{"gone":1}' }), 'UnknownField'],
    [rowWith({ fields: '{"size":"1"}' }), 'Value'],
    [rowWith({ fields: `{"members":[]}` }), 'WrongType'],
    [rowWith({ sets: '{"name":[]}' }), 'WrongType'],
    // Elements in strictly increasing order, each with at least one tag in increasing order.
    [rowWith({ sets: `{"members":[["${PERSON_3}",["${TAG_1}"]],["${PERSON_2}",["${TAG_1}"]]]}` }), 'WrongType'],
    [rowWith({ sets: `{"members":[["${PERSON_2}",[]]]}` }), 'WrongType'],
    [rowWith({ sets: `{"members":[["${PERSON_2}",["${TAG_4}","${TAG_1}"]]]}` }), 'WrongType'],
    [rowWith({ maps: `{"roles":[["${PERSON_2}",{"hlc":"${HLC}","removed":false}]]}` }), 'WrongType'],
    [rowWith({ maps: `{"roles":[["${PERSON_2}",{"hlc":"${HLC}","value":"admin","x":1}]]}` }), 'UnknownKey'],
    [rowWith({ maps: `{"roles":[["${PERSON_2}",{"hlc":"${HLC}","value":"owner"}]]}` }), 'Value'],
    [`{"fields":{},"id":"${ROW}","maps":{},"sets":{},"version":1}`, 'UnknownKey'],
    [`{"fields":{},"id":"${ROW}","sets":{}}`, 'MissingKey'],
  ];
  for (const [row, kind] of cases) {
    assert.throws(
      () => decodeServer(snapshotFrame([row]), typed),
      (error) => error instanceof WireError && error.kind === kind && error.closesConnection,
      `${kind}: ${row}`,
    );
  }
});

test('reads server ops with their log position, typed by model and field', () => {
  const op = (body, seq = '0000000000000005') =>
    `{"op":{"body":${body},"hlc":"${HLC}","id":"${OP_2}","model":"Room","row":"${ROW}"},"seq":"${seq}"}`;
  const frame = (...ops) => `{"ops":[${ops.join(',')}],"t":"ops","v":0}`;
  const head = { id: OP_2, hlc: HLC, model: 'Room', row: ROW };
  const msg = decodeServer(
    frame(
      op('{"set":{"field":"name","value":"lobby"}}'),
      op(`{"add":{"elem":"${PERSON_2}","field":"members"}}`, '0000000000000006'),
      op(`{"remove":{"elem":"${PERSON_2}","field":"members","tags":["${TAG_1}"]}}`),
      op(`{"put":{"field":"roles","key":"${PERSON_2}","value":"admin"}}`),
      op(`{"remove_key":{"field":"roles","key":"${PERSON_2}"}}`),
      op(`{"make":{"keys":[2,1],"name":"x","owner":"${PERSON_2}"}}`),
      op('{"drop":{}}'),
    ),
    typed,
  );
  assert.deepEqual(msg.ops.map((o) => o.seq).slice(0, 2), ['0000000000000005', '0000000000000006']);
  assert.deepEqual(msg.ops.map((o) => o.op.body), [
    { kind: 'set', field: 'name', value: 'lobby' },
    { kind: 'add', field: 'members', elem: PERSON_2 },
    { kind: 'remove', field: 'members', elem: PERSON_2, tags: [TAG_1] },
    { kind: 'put', field: 'roles', key: PERSON_2, value: 'admin' },
    { kind: 'remove_key', field: 'roles', key: PERSON_2 },
    { kind: 'make', fields: { keys: [2, 1], name: 'x', owner: PERSON_2 } },
    { kind: 'drop' },
  ]);
  assert.deepEqual({ ...msg.ops[0].op, body: undefined }, { ...head, body: undefined });
  assert.deepEqual(decodeServer(frame(), typed), { t: 'ops', ops: [] });

  // A server op that does not fit the schema is fatal (same as Rust).
  for (const body of [
    '{"drop":{},"make":{}}',
    '{"set":{"field":"gone","value":1}}',
    '{"set":{"field":"members","value":[]}}',
    '{"add":{"elem":"x","field":"members"}}',
    '{"put":{"field":"members","key":"x","value":1}}',
    `{"remove":{"elem":"${PERSON_2}","field":"members"}}`,
    `{"remove":{"elem":"${PERSON_2}","field":"members","tags":[]}}`,
    '{"make":{"made":1}}',
  ]) {
    refuses(frame(op(body)), 'ServerOp', undefined, typed);
  }
  refuses(frame(op('{"drop":{}}').replace('"Room"', '"Gone"')), 'ServerOp', undefined, typed);
  // A body that is not an object of known kinds is a malformed frame.
  refuses(frame(op('{"copy":{}}')), 'WrongType', 'body', typed);
  refuses(frame(op('{"drop":{"x":1}}')), 'UnknownKey', undefined, typed);
  refuses(frame(op('{}')), 'WrongType', 'body', typed);
});

test('reads the result of a return by the result type of its function', () => {
  const ret = (call, result) => `{"call":${call},"result":${result},"t":"return","v":0}`;
  // Same as Rust: {"call":8,"result":{"ok":-3},...}, here for `count`.
  assert.deepEqual(decodeServer(ret(0, '{"ok":-3}'), typed), { t: 'return', call: 0, result: { ok: true, value: -3 } });
  assert.deepEqual(decodeServer(ret(1, '{"ok":null}'), typed).result, { ok: true, value: null });
  assert.deepEqual(decodeServer(ret(3, '{"ok":["b","a"]}'), typed).result, { ok: true, value: ['b', 'a'] });
  for (const kind of ['Denied', 'NotFound', 'Limit', 'Invalid', 'Unavailable']) {
    assert.deepEqual(decodeServer(ret(2, `{"err":"${kind}"}`), typed).result, { ok: false, kind });
  }
  assert.deepEqual(decodeServer(ret(2, '{"runtime":"DivisionByZero"}'), typed).result, {
    ok: false,
    kind: 'DivisionByZero',
  });
  refuses(ret(0, '{"ok":"3"}'), 'Value', 'result', typed);
  refuses(ret(1, '{"ok":false}'), 'Value', 'result', typed);
  refuses(ret(2, '{"err":"Offline"}'), 'WrongType', 'result', typed);
  refuses(ret(2, '{"runtime":"Division by zero"}'), 'WrongType', 'result', typed);
  refuses(ret(2, `{"runtime":"${'A'.repeat(65)}"}`), 'WrongType', 'result', typed);
  refuses(ret(2, '{"err":"Denied","ok":true}'), 'WrongType', 'result', typed);
  refuses(ret(2, '{}'), 'WrongType', 'result', typed);
  refuses(ret(9, '{"ok":1}'), 'UnknownCall', undefined, typed);
});

test('writes subscribe with its query and call with typed arguments', () => {
  // Same as Rust (wire_messages.rs, subscribe of sub 3).
  const query = {
    model: 'Person',
    filter: { cmp: ['eq', { field: { field: 'name', hops: [['home', 'Room']] } }, { const: 'x' }] },
    order: [['age', 'asc']],
    limit: null,
    after: null,
  };
  assert.equal(
    encodeClient({ t: 'subscribe', sub: 3, query }, { schema }),
    '{"query":{"after":null,"filter":{"cmp":["eq",{"field":{"field":"name","hops":[["home","Room"]]}},{"const":"x"}]},"limit":null,"model":"Person","order":[["age","asc"]]},"sub":3,"t":"subscribe","v":0}',
  );
  const refusesToWrite = (msg, kind) =>
    assert.throws(() => encodeClient(msg, { schema }), (e) => e instanceof WireError && e.kind === kind, kind);
  refusesToWrite({ t: 'subscribe', sub: 3, query: { ...query, model: 'Nope' } }, 'NotInSchema');
  refusesToWrite({ t: 'subscribe', sub: 3, query: { model: 'Person' } }, 'WrongType');
  refusesToWrite({ t: 'subscribe', sub: 3, query: { ...query, extra: 1 } }, 'WrongType');
  refusesToWrite({ t: 'subscribe', sub: 3, query: { ...query, filter: { const: Uint8Array.of(1) } } }, 'WrongType');
  refusesToWrite({ t: 'subscribe', sub: -1, query }, 'FieldRange');

  // Same as Rust (call_round_trips_with_typed_arguments).
  assert.equal(
    encodeClient({ t: 'call', call: 7, function: 'rename', args: [ROW, 'lobby'] }, { schema }),
    `{"args":["${ROW}","lobby"],"call":7,"function":"rename","t":"call","v":0}`,
  );
  assert.equal(
    encodeClient({ t: 'call', call: 0, function: 'count', args: [] }, { schema }),
    '{"args":[],"call":0,"function":"count","t":"call","v":0}',
  );
  refusesToWrite({ t: 'call', call: 1, function: 'nope', args: [] }, 'NotInSchema');
  refusesToWrite({ t: 'call', call: 1, function: 'rename', args: [ROW] }, 'WrongType');
  refusesToWrite({ t: 'call', call: 1, function: 'rename', args: [ROW, 3] }, 'Value');
});
