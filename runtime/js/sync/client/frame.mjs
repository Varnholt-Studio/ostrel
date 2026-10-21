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
// What this module reads in full: `welcome`, `ack`, `reject`, `defer`, `evict`, `reset` and
// `error`. The other server messages carry values whose type comes from the compiled schema
// (D132). Until the client has that schema, a `snapshot` or `return` for a request the client
// does not have is dropped as the protocol says (D155), and anything else of them is
// `Unsupported`, which closes the connection.
//
// What this module writes: `hello`, `unsubscribe`, `held` and `ops`. An op is passed in its
// wire form `{id, hlc, model, row, body}` (5.4), already carrying names, not ids.

import { encode } from '../../crdt/canon/canon.mjs';
import { JsonFault, JsonNumber, parseJson } from './json.mjs';

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
 * their own types on the wire. The default has no open subscription and no waiting call.
 */
export const NO_REQUESTS = Object.freeze({
  subscriptionModel: () => null,
  callFunction: () => null,
});

/**
 * Reads one server frame.
 *
 * @param {string} frame  the text of one WebSocket message
 * @param {object} [options]
 * @param {Iterable<string>} [options.models]  model names of the compiled schema
 * @param {{subscriptionModel: Function, callFunction: Function}} [options.requests]
 * @param {number} [options.frameBytes]  the frame limit
 * @returns {object} the message: `{ t, ...fields }` with the wire field names
 * @throws {WireError}
 */
export function decodeServer(frame, options = {}) {
  const {
    models = [],
    requests = NO_REQUESTS,
    frameBytes = DEFAULT_LIMITS.frame_bytes,
  } = options;
  const { name, fields } = open(frame, frameBytes);
  const reader = SERVER_READERS[name];
  if (reader === undefined) {
    throw new WireError('UnknownMessage', `unknown message ${quote(name)}`);
  }
  const msg = reader(fields, { models: new Set(models), requests });
  fields.finish();
  return msg;
}

/**
 * Writes one client message as a canonical frame.
 *
 * @param {object} msg  `{ t: 'hello' | 'unsubscribe' | 'held' | 'ops', ...fields }`
 * @param {object} [options]
 * @param {number} [options.frameBytes]  the frame limit
 * @param {number} [options.opBytes]  the op limit (D109)
 * @returns {string}
 * @throws {WireError}
 */
export function encodeClient(msg, options = {}) {
  const { frameBytes = DEFAULT_LIMITS.frame_bytes, opBytes = DEFAULT_LIMITS.op_bytes } = options;
  if (msg === null || typeof msg !== 'object') {
    throw new TypeError('msg must be an object');
  }
  const writer = CLIENT_WRITERS[msg.t];
  if (writer === undefined) {
    throw new WireError('Unsupported', `message ${quote(String(msg.t))} cannot be written`);
  }
  const frame = encode({ ...writer(msg, { opBytes }), v: PROTOCOL_VERSION, t: msg.t });
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
  evict: (f, { models }) => ({
    t: 'evict',
    sub: f.req('sub', uint32),
    model: f.req('model', (key, j) => knownModel(key, j, models)),
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
  snapshot: (f, { requests }) => {
    const sub = f.req('sub', uint32);
    f.req('rows', any);
    f.req('seq', hexOf('serverSeq'));
    f.req('last', boolean);
    f.finish();
    if (requests.subscriptionModel(sub) == null) {
      throw new WireError('UnknownSubscription', `no subscription ${sub}`);
    }
    throw unsupported('snapshot');
  },
  return: (f, { requests }) => {
    const call = f.req('call', uint32);
    f.req('result', any);
    f.finish();
    if (requests.callFunction(call) == null) {
      throw new WireError('UnknownCall', `no call ${call} is waiting`);
    }
    throw unsupported('return');
  },
  ops: () => {
    throw unsupported('ops');
  },
};

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

function unsupported(name) {
  return new WireError('Unsupported', `message ${quote(name)} is not supported by this codec yet`);
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
};

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
function knownModel(key, j, models) {
  const name = string(key, j);
  if (!models.has(name)) {
    throw new WireError('UnknownModel', `unknown model ${quote(clip(name))}`, key);
  }
  return name;
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
