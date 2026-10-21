// Frames of the sync protocol on the client side (ARCHITECTURE 5.4, D106, D132).
//
// The client reads server frames with `decodeServer` and writes client frames with
// `encodeClient`. The rules are those of the server's codec (`crates/ostrel_sync/src/wire/`):
//
// * A frame is one JSON object: `v` is PROTOCOL_VERSION, `t` the message name, every other key
//   a field of the message in snake case.
// * Every key is required. An absent optional value is written as `null`, never left out, so
//   a missing key is a fault (`MissingKey`) and so is a key the message does not have
//   (`UnknownKey`). This holds for `retry_after_ms` of `error` exactly as for `sub` (D159).
// * Ids, stamps and log positions are fixed width lowercase hex strings (ARCHITECTURE 5.3);
//   counts and small ids are JSON integers in the safe range (D24).
// * A frame over the frame limit is refused before it is parsed (D121).
//
// Typed values (D132). The rows of a `snapshot`, the ops of a server `ops` message and the
// result of a `return` are read against the compiled schema (`value.mjs`). Two of these name
// no model or function on the wire: the rows of a `snapshot` belong to the model of its
// subscription and a `return` has the result type of its call's function. The caller says
// what it has open through `requests` (`subscribe.mjs`, `call.mjs`). A `snapshot` or `return`
// for a request the client does not have is dropped as the protocol says (D155); every other
// fault of a server message, a value that does not fit included, closes the connection,
// because client and server share one compiled schema. A caller that passes no schema, only
// model names, cannot read typed values: for it these three messages stay `Unsupported`.
//
// What this module writes: `hello`, `subscribe`, `unsubscribe`, `held`, `ops` and `call`. An
// op is passed in its wire form `{id, hlc, model, row, body}` (5.4), already carrying names,
// not ids. The query of a `subscribe` is passed in its wire form too (5.4 "Query"), built by
// the generated client; the arguments of a `call` are values in memory, typed by the
// function's parameters.

import { InvalidValue, compareKey, encode } from '../../crdt/canon/canon.mjs';
import { JsonFault, JsonNumber, parseJson } from './json.mjs';
import { ValueFault, compileSchema, readValue, writeValue } from './value.mjs';

export const PROTOCOL_VERSION = 0;

/** Longest key or name a WireError repeats from a received frame, in UTF-8 bytes. */
export const MAX_ECHO_BYTES = 64;

/**
 * The limits table of `Welcome`, in the order of `ostrel_sync::protocol::Limits`. A `welcome`
 * must carry exactly these keys.
 */
export const LIMIT_KEYS = Object.freeze([
  'frame_bytes',
  'http_body_bytes',
  'ops_per_batch',
  'ops_per_second',
  'ops_burst',
  'rows_per_hour',
  'op_bytes',
  'row_bytes',
  'nesting_depth',
  'text_bytes',
  'subscriptions_per_session',
  'filter_nodes',
  'rows_per_snapshot',
  'ids_per_page',
  'held_per_subscription',
  'outbox_before_ack',
  'calls_per_second',
  'calls_burst',
  'call_timeout_ms',
  'bulk_rows',
  'bulk_attempts',
  'clock_tolerance_ms',
  'tags_per_remove',
  'rank_digits',
]);

/**
 * The limits the client uses before the server sent its own in `Welcome`. The values are
 * those of `ostrel_sync::protocol::LIMITS`; only the ones the client needs are listed.
 */
export const DEFAULT_LIMITS = Object.freeze({
  frame_bytes: 1 << 20,
  ops_per_batch: 200,
  op_bytes: 512 << 10,
  ids_per_page: 1000,
});

export const REJECT_REASONS = Object.freeze(['Denied', 'Conflict', 'Forged', 'Invalid', 'Limit']);

export const ERROR_CODES = Object.freeze([
  'Protocol',
  'FrameTooLarge',
  'Unauthorized',
  'ReplicaExpired',
  'TooManySubscriptions',
  'FilterTooLarge',
  'FilterNotReadable',
  'Superseded',
  'TooManySessions',
  'Overloaded',
  'RateLimited',
]);

/** Error codes that end the whole session and never name a subscription (D156, D158). */
export const SESSION_CODES = Object.freeze(['Superseded', 'TooManySessions', 'Overloaded']);

/** Errors of a server function call that travel by name (`CallError` without `Runtime`). */
export const CALL_ERRORS = Object.freeze(['Denied', 'NotFound', 'Limit', 'Invalid', 'Unavailable']);

/** Most tags one `remove` may name (`tags_per_remove` of the limits table). */
export const TAGS_PER_REMOVE = 64;

const UINT32_MAX = 0xffffffff;
const HEX = {
  replica: /^[0-9a-f]{16}$/,
  serverSeq: /^[0-9a-f]{16}$/,
  opId: /^[0-9a-f]{24}$/,
  rowId: /^[0-9a-f]{32}$/,
  hlc: /^[0-9a-f]{32}$/,
};

// Kinds that concern one request only and keep the connection open (wire/mod.rs).
const KEEPS_CONNECTION = new Set(['UnknownSubscription', 'UnknownCall']);

/**
 * Why a frame could not be read or written. `kind` is the name of the matching
 * `ostrel_sync::wire::WireError` variant, `key` the field it concerns (if any).
 */
export class WireError extends Error {
  constructor(kind, message, key = null) {
    super(message);
    this.name = 'WireError';
    this.kind = kind;
    this.key = key;
  }

  /** False for a fault that concerns one request only; the client drops that frame. */
  get closesConnection() {
    return !KEEPS_CONNECTION.has(this.kind);
  }
}

/**
 * What the client knows about its own open requests, for the messages that name none of
 * their own types on the wire: the model name of an open subscription and the function name
 * of a call that waits for its `return`, or null. The default has no open request.
 */
export const NO_REQUESTS = Object.freeze({
  subscriptionModel: () => null,
  callFunction: () => null,
});

const EMPTY_SCHEMA = compileSchema({ app: null, enums: [], models: [] });

/**
 * Reads one server frame.
 *
 * @param {string} frame  the text of one WebSocket message
 * @param {object} [options]
 * @param {import('./value.mjs').WireSchema} [options.schema]  the compiled schema
 * @param {Iterable<string>} [options.models]  model names only, for a caller without the
 *   schema: enough for `evict`; `snapshot`, `return` and `ops` are then `Unsupported`
 * @param {{subscriptionModel: Function, callFunction: Function}} [options.requests]
 * @param {number} [options.frameBytes]  the frame limit
 * @returns {object} the message: `{ t, ...fields }` with the wire field names
 * @throws {WireError}
 */
export function decodeServer(frame, options = {}) {
  const {
    schema = null,
    models = [],
    requests = NO_REQUESTS,
    frameBytes = DEFAULT_LIMITS.frame_bytes,
  } = options;
  const { name, fields } = open(frame, frameBytes);
  const reader = SERVER_READERS[name];
  if (reader === undefined) {
    throw new WireError('UnknownMessage', `unknown message ${quote(name)}`);
  }
  const names = schema === null ? new Set(models) : null;
  const hasModel = (name) => (schema === null ? names.has(name) : schema.hasModel(name));
  const msg = reader(fields, { schema, hasModel, requests });
  fields.finish();
  return msg;
}

/**
 * Writes one client message as a canonical frame.
 *
 * @param {object} msg  `{ t: 'hello' | 'subscribe' | 'unsubscribe' | 'held' | 'ops' | 'call',
 *   ...fields }`
 * @param {object} [options]
 * @param {import('./value.mjs').WireSchema} [options.schema]  the compiled schema, for the
 *   model of a `subscribe` and the function and arguments of a `call`
 * @param {number} [options.frameBytes]  the frame limit
 * @param {number} [options.opBytes]  the op limit (D109)
 * @returns {string}
 * @throws {WireError}
 */
export function encodeClient(msg, options = {}) {
  const {
    schema = EMPTY_SCHEMA,
    frameBytes = DEFAULT_LIMITS.frame_bytes,
    opBytes = DEFAULT_LIMITS.op_bytes,
  } = options;
  if (msg === null || typeof msg !== 'object') {
    throw new TypeError('msg must be an object');
  }
  const writer = CLIENT_WRITERS[msg.t];
  if (writer === undefined) {
    throw new WireError('Unsupported', `message ${quote(String(msg.t))} cannot be written`);
  }
  const fields = writer(msg, { opBytes, schema });
  let frame;
  try {
    frame = encode({ ...fields, v: PROTOCOL_VERSION, t: msg.t });
  } catch (error) {
    // A query or op body that holds a value the canonical encoder refuses.
    if (error instanceof InvalidValue) throw new WireError('WrongType', error.message);
    throw error;
  }
  const bytes = utf8Length(frame);
  if (bytes > frameBytes) {
    throw new WireError('FrameTooLarge', `frame of ${bytes} bytes is over the frame limit`);
  }
  return frame;
}

/** The canonical encoding of one op as it stands in an `ops` array, in UTF-8 bytes (D109). */
export function opBytes(op) {
  return utf8Length(encode(wireOp(op)));
}

// ---------------------------------------------------------------------------------------------
// Server messages

const SERVER_READERS = {
  welcome: (f) => ({
    t: 'welcome',
    replica: f.req('replica', hexOf('replica')),
    replica_secret: f.opt('replica_secret', string),
    server_ms: f.req('server_ms', safeUint),
    limits: f.req('limits', limits),
  }),
  ack: (f) => ({
    t: 'ack',
    op: f.req('op', hexOf('opId')),
    seq: f.req('seq', hexOf('serverSeq')),
    restamped: f.req('restamped', listOf(restamp)),
  }),
  reject: (f) => ({
    t: 'reject',
    op: f.req('op', hexOf('opId')),
    reason: f.req('reason', nameIn(REJECT_REASONS)),
  }),
  defer: (f) => ({
    t: 'defer',
    op: f.req('op', hexOf('opId')),
    retry_after_ms: f.req('retry_after_ms', uint32),
  }),
  evict: (f, { hasModel }) => ({
    t: 'evict',
    sub: f.req('sub', uint32),
    model: f.req('model', (key, j) => knownModel(key, j, hasModel)),
    rows: f.req('rows', listOf(hexOf('rowId'))),
  }),
  reset: (f) => ({ t: 'reset', sub: f.req('sub', uint32) }),
  error: (f) => {
    const error = {
      t: 'error',
      sub: f.opt('sub', uint32),
      code: f.req('code', nameIn(ERROR_CODES)),
      // Required like every key; its value is null except for RateLimited (D159).
      retry_after_ms: f.opt('retry_after_ms', uint32),
    };
    if (!isWellFormedError(error)) {
      throw new WireError('IllFormedError', `error fields do not fit the code ${error.code}`);
    }
    return error;
  },
  // The rows belong to the model of the subscription, so they are read only once the
  // subscription is known to be open.
  snapshot: (f, { schema, requests }) => {
    const sub = f.req('sub', uint32);
    const rows = f.req('rows', any);
    const seq = f.req('seq', hexOf('serverSeq'));
    const last = f.req('last', boolean);
    f.finish();
    const model = requests.subscriptionModel(sub);
    if (model == null) {
      throw new WireError('UnknownSubscription', `no subscription ${sub}`);
    }
    if (schema === null) throw unsupported('snapshot');
    if (!schema.hasModel(model)) {
      throw new WireError('UnknownModel', `unknown model ${quote(clip(model))}`, 'rows');
    }
    return {
      t: 'snapshot',
      sub,
      model,
      rows: listOf((key, row) => readRow(row, model, schema))('rows', rows),
      seq,
      last,
    };
  },
  return: (f, { schema, requests }) => {
    const call = f.req('call', uint32);
    const result = f.req('result', any);
    f.finish();
    const name = requests.callFunction(call);
    if (name == null) {
      throw new WireError('UnknownCall', `no call ${call} is waiting`);
    }
    if (schema === null) throw unsupported('return');
    return { t: 'return', call, result: readCallResult(result, name, schema) };
  },
  ops: (f, { schema }) => {
    if (schema === null) throw unsupported('ops');
    return { t: 'ops', ops: f.req('ops', listOf((key, logged) => readLoggedOp(logged, schema))) };
  },
};

function unsupported(name) {
  return new WireError('Unsupported', `message ${quote(name)} needs the compiled schema`);
}

/**
 * Whether the fields of an `error` fit its code, as `ProtocolError::is_well_formed`:
 * `RateLimited` names its subscription and carries `retry_after_ms`, no other code carries
 * `retry_after_ms`, and the session codes never name a subscription.
 */
export function isWellFormedError({ sub, code, retry_after_ms: retryAfterMs }) {
  if (code === 'RateLimited') return sub !== null && retryAfterMs !== null;
  if (SESSION_CODES.includes(code)) return sub === null && retryAfterMs === null;
  return retryAfterMs === null;
}

// ---------------------------------------------------------------------------------------------
// Typed parts of server messages (D132)

// A row of a `snapshot`: `{"id", "fields", "sets", "maps"}` (ARCHITECTURE 5.4 "Row"). In
// memory: `{ id, fields: {name: value}, sets: {name: [{elem, tags}]}, maps: {name: [{key, hlc,
// value} or {key, hlc, removed: true}]} }`, collection entries in the order of the server.
function readRow(json, model, schema) {
  const f = objectFields('rows', json);
  const id = f.req('id', hexOf('rowId'));
  const fields = f.req('fields', namedFields(model, schema, readColumn));
  const sets = f.req('sets', namedFields(model, schema, readSetState));
  const maps = f.req('maps', namedFields(model, schema, readMapState));
  f.finish();
  return { id, fields, sets, maps };
}

// Reads an object of field name to state; every name must be a field of the row's model.
function namedFields(model, schema, readState) {
  return (key, json) => {
    if (!(json instanceof Map)) throw wrongForm(key);
    const out = {};
    for (const [name, state] of json) {
      const type = schema.rowFieldType(model, name);
      if (type === null) {
        throw new WireError('UnknownField', `unknown field ${quote(clip(name))}`, key);
      }
      // `Object.defineProperty`, so a field name never reaches a prototype setter.
      Object.defineProperty(out, name, {
        value: readState(key, type, state),
        enumerable: true,
      });
    }
    return out;
  };
}

function readColumn(key, type, json) {
  if (type.kind === 'Set' || type.kind === 'Map') throw wrongForm(key);
  return typedValue(key, type, json);
}

// `[[elem, [tag, ...]], ...]`: elements in strictly increasing order, each with at least one
// tag, the tags of one element strictly increasing.
function readSetState(key, type, json) {
  if (type.kind !== 'Set') throw wrongForm(key);
  const groups = [];
  for (const group of items(key, json)) {
    const [elemJson, tagsJson] = pair(key, group);
    const elem = typedValue(key, type.elem, elemJson);
    const tags = items(key, tagsJson).map((tag) => hexOf('opId')(key, tag));
    const previous = groups.at(-1);
    if (tags.length === 0 || (previous !== undefined && compareKey(previous.elem, elem) >= 0)) {
      throw wrongForm(key);
    }
    for (let i = 1; i < tags.length; i += 1) {
      if (tags[i - 1] >= tags[i]) throw wrongForm(key);
    }
    groups.push({ elem, tags });
  }
  return groups;
}

// `[[key, {"hlc", "value"}] or [key, {"hlc", "removed": true}], ...]`, keys strictly
// increasing.
function readMapState(key, type, json) {
  if (type.kind !== 'Map') throw wrongForm(key);
  const entries = [];
  for (const item of items(key, json)) {
    const [keyJson, stateJson] = pair(key, item);
    const state = objectFields(key, stateJson);
    const hlc = state.req('hlc', hexOf('hlc'));
    let entry;
    if (state.has('removed')) {
      if (state.req('removed', boolean) !== true) throw wrongForm('removed');
      entry = { hlc, removed: true };
    } else {
      entry = { hlc, value: state.req('value', (k, j) => typedValue(k, type.value, j)) };
    }
    state.finish();
    const mapKey = typedValue(key, type.key, keyJson);
    const previous = entries.at(-1);
    if (previous !== undefined && compareKey(previous.key, mapKey) >= 0) throw wrongForm(key);
    entries.push({ key: mapKey, ...entry });
  }
  return entries;
}

// `{"seq", "op"}`: one op of a server `ops` message with its log position. An op that does not
// fit the schema is fatal (`ServerOp`): client and server share one compiled schema.
function readLoggedOp(json, schema) {
  const f = objectFields('ops', json);
  const seq = f.req('seq', hexOf('serverSeq'));
  const op = f.req('op', (key, j) => readServerOp(j, schema));
  f.finish();
  return { seq, op };
}

// `{"id", "hlc", "model", "row", "body"}`, the body typed as `{ kind, ... }` with the wire kind
// name: `make` `{fields}`, `drop`, `set` `{field, value}`, `add` `{field, elem}`, `remove`
// `{field, elem, tags}`, `put` `{field, key, value}`, `remove_key` `{field, key}`.
function readServerOp(json, schema) {
  const f = objectFields('ops', json);
  const id = f.req('id', hexOf('opId'));
  const hlc = f.req('hlc', hexOf('hlc'));
  const row = f.req('row', hexOf('rowId'));
  const model = f.req('model', string);
  const raw = f.req('body', rawBody);
  f.finish();
  const fault = (why) => new WireError('ServerOp', `server op ${id} does not fit the schema: ${why}`);
  if (raw === SEVERAL_KINDS) throw fault('several kinds in one body');
  if (!schema.hasModel(model)) throw fault(`unknown model ${quote(clip(model))}`);
  return { id, hlc, model, row, body: typedBody(raw, model, schema, fault) };
}

const SEVERAL_KINDS = Symbol('several kinds');
const BODY_KEYS = {
  drop: [],
  set: ['field', 'value'],
  add: ['field', 'elem'],
  remove: ['field', 'elem', 'tags'],
  put: ['field', 'key', 'value'],
  remove_key: ['field', 'key'],
};

// The form of a body: one known kind whose object has exactly its keys. An unknown or
// malformed kind is fatal; two well formed kinds are SEVERAL_KINDS (D147).
function rawBody(key, json) {
  if (!(json instanceof Map) || json.size === 0) throw wrongForm(key);
  const bodies = [...json].map(([kind, inner]) => {
    if (!(inner instanceof Map)) throw wrongForm(key);
    if (kind === 'make') return { kind, fields: inner };
    if (!Object.hasOwn(BODY_KEYS, kind)) throw wrongForm(key);
    const f = new Fields(inner);
    const body = { kind };
    for (const name of BODY_KEYS[kind]) {
      // Absent `tags` make only the op invalid, like a value that does not fit (D135).
      if (name === 'tags' && !f.has('tags')) body.tags = null;
      else body[name] = name === 'field' ? f.req(name, string) : f.req(name, any);
    }
    f.finish();
    return body;
  });
  return bodies.length > 1 ? SEVERAL_KINDS : bodies[0];
}

function typedBody(raw, model, schema, fault) {
  const field = (name) => {
    const type = schema.opFieldType(model, name);
    if (type === null) throw fault(`unknown field ${quote(clip(name))}`);
    return type;
  };
  const value = (name, type, json) => {
    try {
      return readValue(type, json);
    } catch (error) {
      if (error instanceof ValueFault) throw fault(`field ${quote(clip(name))}: ${error.message}`);
      throw error;
    }
  };
  const collection = (name, kind) => {
    const type = field(name);
    if (type.kind !== kind) throw fault(`field ${quote(clip(name))} has another body`);
    return type;
  };
  switch (raw.kind) {
    case 'make': {
      const fields = {};
      for (const [name, json] of raw.fields) {
        Object.defineProperty(fields, name, { value: value(name, field(name), json), enumerable: true });
      }
      return { kind: 'make', fields };
    }
    case 'drop':
      return { kind: 'drop' };
    case 'set': {
      const type = field(raw.field);
      if (type.kind === 'Set' || type.kind === 'Map') {
        throw fault(`field ${quote(clip(raw.field))} has another body`);
      }
      return { kind: 'set', field: raw.field, value: value(raw.field, type, raw.value) };
    }
    case 'add': {
      const type = collection(raw.field, 'Set');
      return { kind: 'add', field: raw.field, elem: value(raw.field, type.elem, raw.elem) };
    }
    case 'remove': {
      const type = collection(raw.field, 'Set');
      const tags = Array.isArray(raw.tags) ? raw.tags : [];
      if (tags.length === 0 || tags.length > TAGS_PER_REMOVE) throw fault('tags out of range');
      if (!tags.every((tag) => typeof tag === 'string' && HEX.opId.test(tag))) {
        throw fault('tags are not op ids');
      }
      return {
        kind: 'remove',
        field: raw.field,
        elem: value(raw.field, type.elem, raw.elem),
        tags,
      };
    }
    case 'put': {
      const type = collection(raw.field, 'Map');
      return {
        kind: 'put',
        field: raw.field,
        key: value(raw.field, type.key, raw.key),
        value: value(raw.field, type.value, raw.value),
      };
    }
    case 'remove_key': {
      const type = collection(raw.field, 'Map');
      return { kind: 'remove_key', field: raw.field, key: value(raw.field, type.key, raw.key) };
    }
    default:
      throw new TypeError(`unknown body kind ${raw.kind}`);
  }
}

// `{"ok": value}`, `{"err": name}` or `{"runtime": kind}`, as `{ ok: true, value }` or
// `{ ok: false, kind }`. The value has the result type of the call's function (a `Unit`
// function returns `null`, D193).
function readCallResult(json, name, schema) {
  const fn = schema.function(name);
  if (fn === null) throw new WireError('NotInSchema', 'function is not in the schema', 'result');
  if (!(json instanceof Map) || json.size !== 1) throw wrongForm('result');
  const [[kind, content]] = json;
  switch (kind) {
    case 'ok':
      return { ok: true, value: typedValue('result', fn.result, content) };
    case 'err': {
      const error = string('result', content);
      if (!CALL_ERRORS.includes(error)) throw wrongForm('result');
      return { ok: false, kind: error };
    }
    case 'runtime': {
      const runtimeKind = string('result', content);
      // 1 to 64 ASCII letters, as `is_runtime_kind`: the list of kinds belongs to the VM.
      if (!/^[A-Za-z]{1,64}$/.test(runtimeKind)) throw wrongForm('result');
      return { ok: false, kind: runtimeKind };
    }
    default:
      throw wrongForm('result');
  }
}

function typedValue(key, type, json) {
  try {
    return readValue(type, json);
  } catch (error) {
    if (error instanceof ValueFault) {
      const wire = new WireError('Value', `key ${quote(key)}: ${error.message}`, key);
      wire.fault = error.kind;
      throw wire;
    }
    throw error;
  }
}

// ---------------------------------------------------------------------------------------------
// Client messages

const CLIENT_WRITERS = {
  hello: (msg) => {
    const fields = {
      token: optional(msg.token, 'token', checkString),
      replica: optional(msg.replica, 'replica', checkHex('replica')),
      replica_secret: optional(msg.replica_secret, 'replica_secret', checkString),
      last_server_seq: optional(msg.last_server_seq, 'last_server_seq', checkHex('serverSeq')),
    };
    // The server refuses a secret without a replica as a fatal Protocol error (D122).
    if (fields.replica_secret !== null && fields.replica === null) {
      throw new WireError('WrongType', 'replica_secret is only sent with a replica', 'replica_secret');
    }
    return fields;
  },
  subscribe: (msg, { schema }) => ({
    sub: checkUint32(msg.sub, 'sub'),
    query: wireQuery(msg.query, schema),
  }),
  unsubscribe: (msg) => ({ sub: checkUint32(msg.sub, 'sub') }),
  held: (msg) => {
    if (!Array.isArray(msg.rows)) {
      throw new WireError('WrongType', 'key "rows" has the wrong form', 'rows');
    }
    if (typeof msg.last !== 'boolean') {
      throw new WireError('WrongType', 'key "last" has the wrong form', 'last');
    }
    return {
      sub: checkUint32(msg.sub, 'sub'),
      rows: msg.rows.map((row) => checkHex('rowId')(row, 'rows')),
      last: msg.last,
    };
  },
  ops: (msg, { opBytes: limit }) => {
    if (!Array.isArray(msg.ops)) {
      throw new WireError('WrongType', 'key "ops" has the wrong form', 'ops');
    }
    return {
      ops: msg.ops.map((op) => {
        const wire = wireOp(op);
        const bytes = utf8Length(encode(wire));
        if (bytes > limit) {
          throw new WireError('OpTooLarge', `op ${wire.id} of ${bytes} bytes is over the op limit`);
        }
        return wire;
      }),
    };
  },
  call: (msg, { schema }) => {
    const call = checkUint32(msg.call, 'call');
    const name = checkString(msg.function, 'function');
    const fn = schema.function(name);
    if (fn === null) throw new WireError('NotInSchema', 'function is not in the schema', 'function');
    if (!Array.isArray(msg.args) || msg.args.length !== fn.params.length) {
      throw new WireError('WrongType', 'key "args" has the wrong form', 'args');
    }
    const args = msg.args.map((arg, i) => {
      try {
        return writeValue(fn.params[i], arg);
      } catch (error) {
        if (error instanceof ValueFault) {
          throw new WireError('Value', `argument ${i + 1}: ${error.message}`, 'args');
        }
        throw error;
      }
    });
    return { call, function: name, args };
  },
};

// The query of a `subscribe` in its wire form (ARCHITECTURE 5.4 "Query"): exactly the keys
// `model`, `filter`, `order`, `limit` and `after`. The client checks the model and the shape
// of the head; the server checks the filter and answers a bad one with `error` for this
// subscription, which keeps the connection (D106).
function wireQuery(query, schema) {
  if (query === null || typeof query !== 'object' || Array.isArray(query)) {
    throw new WireError('WrongType', 'key "query" has the wrong form', 'query');
  }
  const keys = Object.keys(query).sort();
  if (keys.join() !== 'after,filter,limit,model,order') {
    throw new WireError('WrongType', 'a query has exactly model, filter, order, limit, after', 'query');
  }
  const model = checkString(query.model, 'query');
  if (!schema.hasModel(model)) throw new WireError('NotInSchema', 'model is not in the schema', 'query');
  if (!Array.isArray(query.order)) throw new WireError('WrongType', 'order is an array', 'query');
  const limit = query.limit === null ? null : checkUint32(query.limit, 'query');
  return { model, filter: query.filter, order: query.order, limit, after: query.after };
}

// Checks the head of one op and returns exactly its five wire keys.
function wireOp(op) {
  if (op === null || typeof op !== 'object' || Array.isArray(op)) {
    throw new WireError('WrongType', 'an op must be an object', 'ops');
  }
  const { id, hlc, model, row, body } = op;
  if (body === null || typeof body !== 'object' || Array.isArray(body)) {
    throw new WireError('WrongType', 'the body of an op must be an object', 'ops');
  }
  return {
    id: checkHex('opId')(id, 'ops'),
    hlc: checkHex('hlc')(hlc, 'ops'),
    model: checkString(model, 'ops'),
    row: checkHex('rowId')(row, 'ops'),
    body,
  };
}

function optional(value, key, check) {
  return value === null || value === undefined ? null : check(value, key);
}

function checkString(value, key) {
  if (typeof value !== 'string') {
    throw new WireError('WrongType', `key ${quote(key)} has the wrong form`, key);
  }
  return value;
}

function checkHex(kind) {
  return (value, key) => {
    if (typeof value !== 'string' || !HEX[kind].test(value)) {
      throw new WireError('InvalidId', `key ${quote(key)} is not a valid ${kind}`, key);
    }
    return value;
  };
}

function checkUint32(value, key) {
  if (!Number.isSafeInteger(value) || value < 0 || value > UINT32_MAX) {
    throw new WireError('FieldRange', `key ${quote(key)} is out of range`, key);
  }
  return value;
}

// ---------------------------------------------------------------------------------------------
// Envelope

// Checks size, JSON and envelope of a received frame; returns the name and the other fields.
function open(frame, frameBytes) {
  if (typeof frame !== 'string') {
    throw new WireError('Malformed', 'a frame must be a text message');
  }
  // Every UTF-16 unit is at least one UTF-8 byte, so a long text is too large without counting.
  const bytes = frame.length > frameBytes ? frame.length : utf8Length(frame);
  if (bytes > frameBytes) {
    throw new WireError('FrameTooLarge', `frame of at least ${bytes} bytes is over the frame limit`);
  }
  let json;
  try {
    json = parseJson(frame);
  } catch (error) {
    if (error instanceof JsonFault) {
      const key = error.key === null ? null : clip(error.key);
      throw new WireError(error.kind, error.message, key);
    }
    throw error;
  }
  if (!(json instanceof Map)) {
    throw new WireError('NotAnObject', 'frame is not a JSON object');
  }
  const fields = new Fields(json);
  // The version comes first: a frame of another version may name messages we do not know.
  if (fields.req('v', safeInt) !== PROTOCOL_VERSION) {
    throw new WireError('UnsupportedVersion', 'unsupported protocol version');
  }
  return { name: fields.req('t', string), fields };
}

/** The keys of one received object that have not been read yet. */
class Fields {
  constructor(entries) {
    this.entries = entries;
  }

  /** Takes a required key and reads it with `read(key, json)`. */
  req(key, read) {
    if (!this.entries.has(key)) {
      throw new WireError('MissingKey', `missing key ${quote(key)}`, key);
    }
    const value = this.entries.get(key);
    this.entries.delete(key);
    return read(key, value);
  }

  /** Whether the key is there and not read yet. */
  has(key) {
    return this.entries.has(key);
  }

  /** Takes a required key whose value may be `null`. */
  opt(key, read) {
    return this.req(key, (k, j) => (j === null ? null : read(k, j)));
  }

  /** Fails on the first key no field has read. */
  finish() {
    for (const key of this.entries.keys()) {
      throw new WireError('UnknownKey', `unknown key ${quote(clip(key))}`, clip(key));
    }
  }
}

// ---------------------------------------------------------------------------------------------
// Readers: one per field form. Each gets the key for its error and the JSON node.

function any(_key, j) {
  return j;
}

function string(key, j) {
  if (typeof j !== 'string') {
    throw new WireError('WrongType', `key ${quote(key)} has the wrong form`, key);
  }
  return j;
}

function boolean(key, j) {
  if (typeof j !== 'boolean') {
    throw new WireError('WrongType', `key ${quote(key)} has the wrong form`, key);
  }
  return j;
}

// An integer of a message field. Outside the safe range is NumberOutOfRange whatever its form
// (D24); in range with a fraction or an exponent is the wrong form.
function safeInt(key, j) {
  if (!(j instanceof JsonNumber)) {
    throw new WireError('WrongType', `key ${quote(key)} has the wrong form`, key);
  }
  if (Math.abs(j.value) > Number.MAX_SAFE_INTEGER) {
    throw new WireError('NumberOutOfRange', 'number outside the safe integer range', key);
  }
  if (!j.isIntegerToken) {
    throw new WireError('WrongType', `key ${quote(key)} has the wrong form`, key);
  }
  return j.value === 0 ? 0 : j.value;
}

function safeUint(key, j) {
  const n = safeInt(key, j);
  if (n < 0) throw new WireError('FieldRange', `key ${quote(key)} is out of range`, key);
  return n;
}

function uint32(key, j) {
  const n = safeInt(key, j);
  if (n < 0 || n > UINT32_MAX) {
    throw new WireError('FieldRange', `key ${quote(key)} is out of range`, key);
  }
  return n;
}

function hexOf(kind) {
  return (key, j) => {
    const text = string(key, j);
    if (!HEX[kind].test(text)) {
      throw new WireError('InvalidId', `key ${quote(key)} is not a valid ${kind}`, key);
    }
    return text;
  };
}

function listOf(item) {
  return (key, j) => {
    if (!Array.isArray(j)) {
      throw new WireError('WrongType', `key ${quote(key)} has the wrong form`, key);
    }
    return j.map((element) => item(key, element));
  };
}

function nameIn(names) {
  return (key, j) => {
    const name = string(key, j);
    if (!names.includes(name)) {
      throw new WireError('WrongType', `key ${quote(key)} has the wrong form`, key);
    }
    return name;
  };
}

// `[op, hlc]`: an op the server re-stamped, or the accepted stamp of a replay.
function restamp(key, j) {
  if (!Array.isArray(j) || j.length !== 2) {
    throw new WireError('WrongType', `key ${quote(key)} has the wrong form`, key);
  }
  return [hexOf('opId')(key, j[0]), hexOf('hlc')(key, j[1])];
}

// Client and server share one compiled schema, so an unknown model is a fatal mismatch.
function knownModel(key, j, hasModel) {
  const name = string(key, j);
  if (!hasModel(name)) {
    throw new WireError('UnknownModel', `unknown model ${quote(clip(name))}`, key);
  }
  return name;
}

function objectFields(key, j) {
  if (!(j instanceof Map)) throw wrongForm(key);
  return new Fields(j);
}

function items(key, j) {
  if (!Array.isArray(j)) throw wrongForm(key);
  return j;
}

function pair(key, j) {
  const list = items(key, j);
  if (list.length !== 2) throw wrongForm(key);
  return list;
}

function wrongForm(key) {
  return new WireError('WrongType', `key ${quote(key)} has the wrong form`, key);
}

function limits(key, j) {
  if (!(j instanceof Map)) {
    throw new WireError('WrongType', `key ${quote(key)} has the wrong form`, key);
  }
  const fields = new Fields(j);
  const table = {};
  for (const name of LIMIT_KEYS) {
    table[name] = fields.req(name, uint32);
  }
  fields.finish();
  return Object.freeze(table);
}

// ---------------------------------------------------------------------------------------------
// Helpers

function quote(text) {
  return JSON.stringify(text);
}

/** `text` cut to at most MAX_ECHO_BYTES UTF-8 bytes on a character boundary. */
function clip(text) {
  let bytes = 0;
  let out = '';
  for (const ch of text) {
    bytes += utf8Length(ch);
    if (bytes > MAX_ECHO_BYTES) break;
    out += ch;
  }
  return out;
}

/** Length of `text` in UTF-8 bytes. An unpaired surrogate counts as U+FFFD (3 bytes). */
export function utf8Length(text) {
  let bytes = 0;
  for (let i = 0; i < text.length; i += 1) {
    const unit = text.charCodeAt(i);
    if (unit < 0x80) bytes += 1;
    else if (unit < 0x800) bytes += 2;
    else if (unit >= 0xd800 && unit <= 0xdbff && isLowSurrogate(text.charCodeAt(i + 1))) {
      bytes += 4;
      i += 1;
    } else bytes += 3;
  }
  return bytes;
}

function isLowSurrogate(unit) {
  return unit >= 0xdc00 && unit <= 0xdfff;
}
