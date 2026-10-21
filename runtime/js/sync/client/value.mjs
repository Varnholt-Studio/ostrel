// Typed values of the sync protocol on the client side (ARCHITECTURE 5.1, 5.4, D132).
//
// The JSON form of a value does not name its type: a string may be `Text`, an enum variant, a
// reference, `Bytes` or a `Rank` key. So a value is read against the type of its place, which
// the compiled schema supplies. This module mirrors `crates/ostrel_sync/src/wire/value.rs`:
// the same JSON forms, the same faults, the same limits.
//
// It also turns the compiled schema of the JS backend (`export const schema` of the app module,
// `crates/ostrel_codegen_js/src/schema.rs`) into the lookups the codec needs: a model by name,
// a field of a model by name with its type, a server function by name with its parameter and
// result types.
//
// Values in memory follow `runtime/js/api.d.ts` section 2: the wire form, except `Bytes`, which
// is a `Uint8Array`. A `Set` is an array and a `Map` an array of `[key, value]` pairs, both in
// `compareKey` order.

import { compareKey, encode } from '../../crdt/canon/canon.mjs';
import { JsonNumber } from './json.mjs';

/**
 * The value limits the codec checks, as `ostrel_sync::protocol::LIMITS` (the server reads with
 * the same constants, not with the table it sends in `Welcome`).
 */
export const VALUE_LIMITS = Object.freeze({
  textBytes: 256 << 10,
  nestingDepth: 32,
  rankDigits: 1024,
});

/**
 * Why a value does not fit its type. `kind` is the name of the matching
 * `ostrel_sync::protocol::ValueFault` variant.
 */
export class ValueFault extends Error {
  constructor(kind, message) {
    super(message);
    this.name = 'ValueFault';
    this.kind = kind;
  }
}

const SCALARS = new Set(['Bool', 'Int', 'Float', 'Text', 'Time', 'Bytes', 'Rank']);
const INT_MAX = Number.MAX_SAFE_INTEGER;
const ROW_ID = /^[0-9a-f]{32}$/;
const RANK_DIGIT = /^[0-9A-Za-z]$/;

// ---------------------------------------------------------------------------------------------
// Schema

/**
 * The lookups of one compiled schema.
 *
 * The schema form is the one of `schema.rs`: `{ app, enums, models }` plus `functions` (see
 * `compileSchema`). Types are resolved once into plain descriptors: `{ kind: 'Int' }`,
 * `{ kind: 'Enum', variants }`, `{ kind: 'Ref', model }`, `{ kind: 'Optional', inner }`,
 * `{ kind: 'Set', elem }`, `{ kind: 'List', elem }`, `{ kind: 'Map', key, value }` and, as the
 * result of a server function only, `{ kind: 'Unit' }` (D193).
 */
export class WireSchema {
  constructor(models, functions) {
    this.models = models;
    this.functions = functions;
    Object.freeze(this);
  }

  /** Model names, for the frame decoder. */
  modelNames() {
    return [...this.models.keys()];
  }

  hasModel(name) {
    return this.models.has(name);
  }

  /**
   * The type of a field a row may carry: a declared field, or one of the implicit fields
   * `made`, `changed` and (in an app with `auth`) `author` (D186). Null for any other name.
   */
  rowFieldType(model, field) {
    const entry = this.models.get(model);
    if (entry === undefined) return null;
    return entry.declared.get(field) ?? entry.implicit.get(field) ?? null;
  }

  /**
   * The type of a field an op may write: declared fields only. An op never writes an implicit
   * field; the server's decoder refuses one as an unknown field (D116, D186).
   */
  opFieldType(model, field) {
    return this.models.get(model)?.declared.get(field) ?? null;
  }

  /** `{ params, result }` of a server function, or null. */
  function(name) {
    return this.functions.get(name) ?? null;
  }
}

/**
 * Builds the lookups of a compiled schema.
 *
 * ASSUMPTION (T228, finn T235): server functions are listed as
 * `functions: [{ name, params: [type, ...], result: type | 'Unit' }]`, the name being the wire
 * name and only server source functions being listed (D193). A schema without `functions`
 * has no server function.
 *
 * @param {object} schema  `export const schema` of the app module
 * @returns {WireSchema}
 * @throws {TypeError} when the schema is not of that form (a defect of the build)
 */
export function compileSchema(schema) {
  if (schema === null || typeof schema !== 'object') throw new TypeError('schema must be an object');
  const enums = list(schema.enums, 'schema.enums');
  const modelList = list(schema.models, 'schema.models');
  const resolve = (type) => resolveType(type, enums, modelList);

  const auth = schema.app != null && schema.app.auth != null;
  const userIndex = modelList.findIndex((m) => m.name === 'User');
  const models = new Map();
  modelList.forEach((model, index) => {
    const declared = new Map();
    for (const field of list(model.fields, `fields of ${model.name}`)) {
      if (declared.has(field.name)) throw new TypeError(`field ${field.name} is declared twice`);
      declared.set(field.name, resolve(field.type));
    }
    const implicit = new Map([
      ['made', { kind: 'Time' }],
      ['changed', { kind: 'Time' }],
    ]);
    // The std `User` may not be listed among the models; a reference reads as a row id
    // whatever its target, so the target is informative only (D165).
    if (auth) implicit.set('author', { kind: 'Ref', model: userIndex >= 0 ? userIndex : null });
    if (models.has(model.name)) throw new TypeError(`model ${model.name} is declared twice`);
    models.set(model.name, Object.freeze({ index, declared, implicit }));
  });

  const functions = new Map();
  for (const fn of list(schema.functions ?? [], 'schema.functions')) {
    if (functions.has(fn.name)) throw new TypeError(`function ${fn.name} is declared twice`);
    const params = list(fn.params, `params of ${fn.name}`).map(resolve);
    const result = fn.result === 'Unit' ? { kind: 'Unit' } : resolve(fn.result);
    functions.set(fn.name, Object.freeze({ params, result }));
  }
  return new WireSchema(models, functions);
}

function list(value, what) {
  if (!Array.isArray(value)) throw new TypeError(`${what} must be an array`);
  return value;
}

function resolveType(type, enums, models) {
  if (typeof type === 'string') {
    if (!SCALARS.has(type)) throw new TypeError(`unknown type ${type}`);
    return Object.freeze({ kind: type });
  }
  if (type === null || typeof type !== 'object') throw new TypeError('a type is a name or an object');
  const [key, ...more] = Object.keys(type);
  if (more.length > 0) throw new TypeError('a type object has exactly one key');
  const inner = (t) => resolveType(t, enums, models);
  switch (key) {
    case 'enum': {
      const e = enums[type.enum];
      if (e === undefined) throw new TypeError(`no enum ${type.enum}`);
      return Object.freeze({ kind: 'Enum', variants: Object.freeze([...e.variants]) });
    }
    case 'ref':
      if (models[type.ref] === undefined) throw new TypeError(`no model ${type.ref}`);
      return Object.freeze({ kind: 'Ref', model: type.ref });
    case 'optional':
      return Object.freeze({ kind: 'Optional', inner: inner(type.optional) });
    case 'set':
      return Object.freeze({ kind: 'Set', elem: inner(type.set) });
    case 'list':
      return Object.freeze({ kind: 'List', elem: inner(type.list) });
    case 'map':
      return Object.freeze({ kind: 'Map', key: inner(type.map[0]), value: inner(type.map[1]) });
    default:
      throw new TypeError(`unknown type form ${key}`);
  }
}

// ---------------------------------------------------------------------------------------------
// Reading

/**
 * Reads one value of `type` from a node of `parseJson` (a `JsonNumber` for numbers, an array,
 * a `Map` for objects). Nesting depth counts collections, as on the server.
 *
 * @throws {ValueFault}
 */
export function readValue(type, json) {
  return read(type, json, 0);
}

function read(type, json, depth) {
  switch (type.kind) {
    case 'Unit':
      if (json !== null) throw wrongType();
      return null;
    case 'Optional':
      return json === null ? null : read(type.inner, json, depth);
    case 'Bool':
      if (typeof json !== 'boolean') throw wrongType();
      return json;
    case 'Int':
    case 'Time':
      return safeInteger(json);
    case 'Float':
      // Any number token: the canonical encoder writes integral floats without a fraction. A
      // token that overflows to an infinity is no `Float` (D185).
      if (!(json instanceof JsonNumber) || !Number.isFinite(json.value)) throw wrongType();
      return json.value + 0;
    case 'Text':
      if (typeof json !== 'string') throw wrongType();
      if (utf8Length(json) > VALUE_LIMITS.textBytes) {
        throw new ValueFault('TextTooLong', 'text is over the text limit');
      }
      return json;
    case 'Bytes': {
      if (typeof json !== 'string') throw wrongType();
      const bytes = base64urlDecode(json);
      if (bytes === null) throw new ValueFault('InvalidBytes', 'bytes are not unpadded base64url');
      return bytes;
    }
    case 'Enum':
      if (typeof json !== 'string') throw wrongType();
      if (!type.variants.includes(json)) throw new ValueFault('UnknownVariant', 'unknown variant');
      return json;
    case 'Ref':
      if (typeof json !== 'string') throw wrongType();
      if (!ROW_ID.test(json)) throw new ValueFault('InvalidRef', 'a reference is a row id');
      return json;
    case 'Rank':
      if (typeof json !== 'string') throw wrongType();
      if (!isValidRank(json)) throw new ValueFault('InvalidRank', 'not a rank key');
      return json;
    case 'Set': {
      if (!Array.isArray(json)) throw wrongType();
      const inner = enter(depth);
      return inOrder(json.map((item) => read(type.elem, item, inner)), (e) => e);
    }
    case 'List': {
      if (!Array.isArray(json)) throw wrongType();
      const inner = enter(depth);
      return json.map((item) => read(type.elem, item, inner));
    }
    case 'Map': {
      if (!Array.isArray(json)) throw wrongType();
      const inner = enter(depth);
      const entries = json.map((item) => {
        if (!Array.isArray(item) || item.length !== 2) throw wrongType();
        return [read(type.key, item[0], inner), read(type.value, item[1], inner)];
      });
      return inOrder(entries, (entry) => entry[0]);
    }
    default:
      throw new TypeError(`unknown type kind ${type.kind}`);
  }
}

function safeInteger(json) {
  if (!(json instanceof JsonNumber)) throw wrongType();
  if (Math.abs(json.value) > INT_MAX) throw new ValueFault('IntRange', 'integer out of range');
  if (!json.isIntegerToken) throw wrongType();
  return json.value + 0;
}

function enter(depth) {
  if (depth >= VALUE_LIMITS.nestingDepth) {
    throw new ValueFault('TooDeep', 'collections nested too deep');
  }
  return depth + 1;
}

// Sorts set elements or map entries by `compareKey` of `keyOf`; two equal keys are a fault,
// because such a value would have two encodings for one content.
function inOrder(items, keyOf) {
  const sorted = [...items].sort((a, b) => compareKey(keyOf(a), keyOf(b)));
  for (let i = 1; i < sorted.length; i += 1) {
    if (compareKey(keyOf(sorted[i - 1]), keyOf(sorted[i])) === 0) {
      throw new ValueFault('Repeated', 'an element or key appears twice');
    }
  }
  return sorted;
}

function wrongType() {
  return new ValueFault('WrongType', 'value has the wrong JSON form for its type');
}

// ---------------------------------------------------------------------------------------------
// Writing

/**
 * The wire form of a value in memory, checked against `type`, ready for the canonical encoder.
 * Used for the arguments of a `call`; generated code passes typed values, so a fault here is a
 * defect of the caller.
 *
 * @throws {ValueFault}
 */
export function writeValue(type, value) {
  return write(type, value, 0);
}

function write(type, value, depth) {
  switch (type.kind) {
    case 'Unit':
      if (value !== null) throw wrongType();
      return null;
    case 'Optional':
      return value === null ? null : write(type.inner, value, depth);
    case 'Bool':
      if (typeof value !== 'boolean') throw wrongType();
      return value;
    case 'Int':
    case 'Time':
      if (!Number.isSafeInteger(value)) {
        throw typeof value === 'number' && Number.isInteger(value)
          ? new ValueFault('IntRange', 'integer out of range')
          : wrongType();
      }
      return value + 0;
    case 'Float':
      if (typeof value !== 'number' || !Number.isFinite(value)) throw wrongType();
      return value + 0;
    case 'Text':
      if (typeof value !== 'string') throw wrongType();
      encode(value); // refuses an unpaired surrogate
      if (utf8Length(value) > VALUE_LIMITS.textBytes) {
        throw new ValueFault('TextTooLong', 'text is over the text limit');
      }
      return value;
    case 'Bytes':
      if (!(value instanceof Uint8Array)) throw wrongType();
      return base64urlEncode(value);
    case 'Enum':
    case 'Ref':
    case 'Rank':
      // In memory these are the wire strings themselves, so the reader's checks apply as is.
      if (typeof value !== 'string') throw wrongType();
      return read(type, value, depth);
    case 'Set': {
      if (!Array.isArray(value)) throw wrongType();
      const inner = enter(depth);
      return inOrder(
        value.map((item) => write(type.elem, item, inner)),
        (e) => e,
      );
    }
    case 'List': {
      if (!Array.isArray(value)) throw wrongType();
      const inner = enter(depth);
      return value.map((item) => write(type.elem, item, inner));
    }
    case 'Map': {
      if (!Array.isArray(value)) throw wrongType();
      const inner = enter(depth);
      const entries = value.map((entry) => {
        if (!Array.isArray(entry) || entry.length !== 2) throw wrongType();
        return [write(type.key, entry[0], inner), write(type.value, entry[1], inner)];
      });
      return inOrder(entries, (entry) => entry[0]);
    }
    default:
      throw new TypeError(`unknown type kind ${type.kind}`);
  }
}

// ---------------------------------------------------------------------------------------------
// Helpers

/** The rule of `isValidRank` in `runtime/js/crdt/rank/rank.mjs`, with the limit of the codec. */
function isValidRank(key) {
  if (key.length === 0 || key.length > VALUE_LIMITS.rankDigits) return false;
  for (const ch of key) if (!RANK_DIGIT.test(ch)) return false;
  return !key.endsWith('0');
}

const BASE64URL = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';

/** Unpadded base64url, as `ostrel_core::value::base64url`. */
export function base64urlEncode(bytes) {
  let out = '';
  for (let i = 0; i < bytes.length; i += 3) {
    const chunk = bytes.subarray(i, i + 3);
    let n = 0;
    for (const b of chunk) n = (n << 8) | b;
    const digits = chunk.length + 1;
    n <<= digits * 6 - chunk.length * 8;
    for (let d = digits - 1; d >= 0; d -= 1) out += BASE64URL[(n >> (6 * d)) & 63];
  }
  return out;
}

/**
 * Strict unpadded base64url: unused low bits of the last digit must be zero, so every byte
 * string has exactly one accepted text. Null for any other text.
 */
export function base64urlDecode(text) {
  const out = [];
  for (let i = 0; i < text.length; i += 4) {
    const chunk = text.slice(i, i + 4);
    const bytes = chunk.length - 1;
    if (bytes === 0) return null;
    let n = 0;
    for (const ch of chunk) {
      const digit = BASE64URL.indexOf(ch);
      if (digit < 0) return null;
      n = n * 64 + digit;
    }
    const spare = chunk.length * 6 - bytes * 8;
    if (n % 2 ** spare !== 0) return null;
    n = Math.floor(n / 2 ** spare);
    for (let b = bytes - 1; b >= 0; b -= 1) out.push(Math.floor(n / 256 ** b) % 256);
  }
  return Uint8Array.from(out);
}

function utf8Length(text) {
  let bytes = 0;
  for (const ch of text) {
    const code = ch.codePointAt(0);
    bytes += code < 0x80 ? 1 : code < 0x800 ? 2 : code < 0x10000 ? 3 : 4;
  }
  return bytes;
}
