// Canonical JSON encoding of replica state (ARCHITECTURE 5.3).
//
// Two replicas have converged when the canonical encodings of their states are equal, so the
// Rust and the JavaScript implementation must write exactly the same bytes for the same value:
//
// * object keys are sorted by their UTF-8 bytes, which is the same as Unicode code point order;
// * no whitespace;
// * strings escape only `"`, `\` and control characters below U+0020 (`\b \f \n \r \t`,
//   otherwise `\u00xx` in lowercase); every other character is written as is;
// * numbers are written as ECMAScript `Number.prototype.toString` writes them, `-0` as `0`;
// * NaN, infinities and strings with an unpaired surrogate are rejected with `InvalidValue`.
//
// The shared test cases live in `tests/crdt-vectors/canon/`.

export class InvalidValue extends Error {
  constructor(message) {
    super(message);
    this.name = 'InvalidValue';
  }
}

/** Returns the canonical encoding of null, a boolean, number, string, array or plain object. */
export function encode(value) {
  if (value === null) return 'null';
  switch (typeof value) {
    case 'boolean':
      return value ? 'true' : 'false';
    case 'number':
      return encodeNumber(value);
    case 'string':
      return encodeString(value);
    case 'object':
      return Array.isArray(value) ? encodeArray(value) : encodeObject(value);
    default:
      throw new InvalidValue(`cannot encode a value of type ${typeof value}`);
  }
}

/**
 * Compares two strings by Unicode code point, which equals the order of their UTF-8 bytes (D50).
 * Use it for `Text` and `Rank` instead of `<`, `localeCompare` or a sort without comparator,
 * which compare UTF-16 units and put U+1F600 before U+FF01. Returns -1, 0 or 1.
 */
export function compareText(a, b) {
  const left = codePoints(a);
  const right = codePoints(b);
  const shared = Math.min(left.length, right.length);
  for (let i = 0; i < shared; i += 1) {
    if (left[i] !== right[i]) return left[i] < right[i] ? -1 : 1;
  }
  return Math.sign(left.length - right.length);
}

// Kinds of `Set` elements and `Map` keys in the order of D61.
const KEY_KIND_ORDER = { boolean: 0, number: 1, string: 2 };

/**
 * Orders `Set` elements and `Map` keys by their wire value (D61, ARCHITECTURE 6.2): booleans
 * before numbers before strings, `false` before `true`, numbers numerically (`-0` equals `0`),
 * strings by code point (`compareText`). Returns -1, 0 or 1.
 *
 * This is deliberately not the order of the canonical encoding, which would put 10 before 9 and
 * escaped characters such as `"` or U+0001 after `A`. Any other kind of value throws
 * `InvalidValue`, because it cannot be an element or a key.
 */
export function compareKey(a, b) {
  const kindA = keyKind(a);
  const kindB = keyKind(b);
  if (kindA !== kindB) return kindA < kindB ? -1 : 1;
  if (typeof a === 'string') return compareText(a, b);
  if (typeof a === 'number' && (!Number.isFinite(a) || !Number.isFinite(b))) {
    throw new InvalidValue('NaN and infinities are not keys');
  }
  if (a === b) return 0; // also true for -0 and 0
  return a < b ? -1 : 1;
}

function keyKind(value) {
  const kind = KEY_KIND_ORDER[typeof value];
  if (kind === undefined) {
    throw new InvalidValue(`a set element or map key is a boolean, number or string, not ${value}`);
  }
  return kind;
}

function encodeNumber(number) {
  if (!Number.isFinite(number)) {
    throw new InvalidValue(`NaN and infinities have no canonical form: ${number}`);
  }
  // String(-0) is already "0", and safe integers never take an exponent.
  return String(number);
}

const SHORT_ESCAPES = new Map([
  ['"', '\\"'],
  ['\\', '\\\\'],
  ['\b', '\\b'],
  ['\f', '\\f'],
  ['\n', '\\n'],
  ['\r', '\\r'],
  ['\t', '\\t'],
]);

function encodeString(text) {
  let out = '"';
  for (const char of text) {
    const short = SHORT_ESCAPES.get(char);
    const code = char.codePointAt(0);
    if (short !== undefined) {
      out += short;
    } else if (code < 0x20) {
      out += `\\u${code.toString(16).padStart(4, '0')}`;
    } else if (code >= 0xd800 && code <= 0xdfff) {
      // Iterating a string yields an unpaired surrogate as a single character.
      throw new InvalidValue(`unpaired surrogate U+${code.toString(16).toUpperCase()}`);
    } else {
      out += char;
    }
  }
  return `${out}"`;
}

function encodeArray(items) {
  return `[${items.map(encode).join(',')}]`;
}

function encodeObject(object) {
  const prototype = Object.getPrototypeOf(object);
  if (prototype !== Object.prototype && prototype !== null) {
    throw new InvalidValue('only plain objects can be encoded');
  }
  const keys = Object.keys(object).sort(compareText);
  const fields = keys.map((key) => `${encodeString(key)}:${encode(object[key])}`);
  return `{${fields.join(',')}}`;
}

function codePoints(text) {
  return Array.from(text, (char) => char.codePointAt(0));
}
