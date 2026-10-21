// Bounded, strict JSON reader for frames the client receives (ARCHITECTURE 5.4, D77).
//
// `JSON.parse` is not strict enough for the wire: it keeps the last copy of a repeated key,
// has no depth limit and turns every number into a float. This reader follows the rules of
// the server's reader (`crates/ostrel_sync/src/wire/json.rs`):
//
// * no object repeats a key, because two parsers may keep different copies of it (D77);
// * arrays and objects nest at most MAX_DEPTH levels deep;
// * strings hold no unpaired surrogate (ARCHITECTURE 5.3);
// * a number keeps whether its token had a fraction or an exponent, so the typed readers in
//   `frame.mjs` can refuse `7.0` in an integer field while a float place accepts it.
//
// Objects become `Map`s, so a key such as `__proto__` is plain data and the order of the
// frame is kept. Every fault throws a `JsonFault` with the kind name of the Rust reader.

/** Deepest nesting of arrays and objects in one frame, as on the server. */
export const MAX_DEPTH = 100;

/** A fault of the reader. `kind` is `Malformed`, `TooDeep` or `DuplicateKey`. */
export class JsonFault extends Error {
  constructor(kind, message, key = null) {
    super(message);
    this.name = 'JsonFault';
    this.kind = kind;
    this.key = key;
  }
}

/** One number token: its value and whether it was written without fraction and exponent. */
export class JsonNumber {
  constructor(value, isIntegerToken) {
    this.value = value;
    this.isIntegerToken = isIntegerToken;
    Object.freeze(this);
  }
}

const NUMBER_TOKEN = /-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/y;
const SIMPLE_ESCAPES = new Map([
  ['"', '"'],
  ['\\', '\\'],
  ['/', '/'],
  ['b', '\b'],
  ['f', '\f'],
  ['n', '\n'],
  ['r', '\r'],
  ['t', '\t'],
]);

/**
 * Parses one complete JSON text. Returns `null`, a boolean, a string, a `JsonNumber`, an
 * array or a `Map`. Trailing content after the value is malformed.
 */
export function parseJson(text) {
  if (typeof text !== 'string') {
    throw new TypeError('parseJson expects a string');
  }
  const reader = new Reader(text);
  reader.skipSpace();
  const value = reader.value(0);
  reader.skipSpace();
  if (reader.at < text.length) {
    reader.fail('trailing content after the value');
  }
  return value;
}

class Reader {
  constructor(text) {
    this.text = text;
    this.at = 0;
  }

  fail(why) {
    throw new JsonFault('Malformed', `${why} at offset ${this.at}`);
  }

  skipSpace() {
    const { text } = this;
    while (this.at < text.length) {
      const c = text[this.at];
      if (c !== ' ' && c !== '\t' && c !== '\n' && c !== '\r') return;
      this.at += 1;
    }
  }

  // `depth` counts the arrays and objects around this value, as in the Rust reader.
  value(depth) {
    const c = this.text[this.at];
    if (c === '{') return this.object(depth);
    if (c === '[') return this.array(depth);
    if (c === '"') return this.string();
    if (c === '-' || (c >= '0' && c <= '9')) return this.number();
    if (this.text.startsWith('true', this.at)) return this.literal('true', true);
    if (this.text.startsWith('false', this.at)) return this.literal('false', false);
    if (this.text.startsWith('null', this.at)) return this.literal('null', null);
    return this.fail(c === undefined ? 'unexpected end of input' : 'unexpected character');
  }

  literal(word, value) {
    this.at += word.length;
    return value;
  }

  enter(depth) {
    if (depth >= MAX_DEPTH) {
      throw new JsonFault('TooDeep', `JSON nested deeper than ${MAX_DEPTH} levels`);
    }
    this.at += 1;
    this.skipSpace();
  }

  array(depth) {
    this.enter(depth);
    const items = [];
    if (this.text[this.at] === ']') {
      this.at += 1;
      return items;
    }
    for (;;) {
      items.push(this.value(depth + 1));
      if (this.endOfList(']')) return items;
    }
  }

  object(depth) {
    this.enter(depth);
    const entries = new Map();
    if (this.text[this.at] === '}') {
      this.at += 1;
      return entries;
    }
    for (;;) {
      if (this.text[this.at] !== '"') this.fail('expected a key');
      const key = this.string();
      if (entries.has(key)) {
        throw new JsonFault('DuplicateKey', 'object repeats a key', key);
      }
      this.skipSpace();
      if (this.text[this.at] !== ':') this.fail('expected a colon');
      this.at += 1;
      this.skipSpace();
      entries.set(key, this.value(depth + 1));
      if (this.endOfList('}')) return entries;
    }
  }

  // After an item: true at the closer, false after a comma; anything else is malformed.
  endOfList(closer) {
    this.skipSpace();
    const c = this.text[this.at];
    this.at += 1;
    if (c === closer) return true;
    if (c !== ',') {
      this.at -= 1;
      this.fail(`expected a comma or ${closer}`);
    }
    this.skipSpace();
    return false;
  }

  number() {
    NUMBER_TOKEN.lastIndex = this.at;
    const match = NUMBER_TOKEN.exec(this.text);
    if (match === null) this.fail('malformed number');
    const token = match[0];
    const value = Number(token);
    if (!Number.isFinite(value)) this.fail('number outside the float range');
    this.at += token.length;
    return new JsonNumber(value, !/[.eE]/.test(token));
  }

  string() {
    const { text } = this;
    this.at += 1;
    let out = '';
    let start = this.at;
    for (;;) {
      if (this.at >= text.length) this.fail('unterminated string');
      const c = text[this.at];
      if (c === '"') break;
      if (c < ' ') this.fail('control character in a string');
      if (c !== '\\') {
        this.at += 1;
        continue;
      }
      out += text.slice(start, this.at);
      out += this.escape();
      start = this.at;
    }
    out += text.slice(start, this.at);
    this.at += 1;
    if (!out.isWellFormed()) {
      this.at -= 1;
      this.fail('unpaired surrogate in a string');
    }
    return out;
  }

  // Reads one escape sequence starting at the backslash and returns its text.
  escape() {
    const c = this.text[this.at + 1];
    const simple = SIMPLE_ESCAPES.get(c);
    if (simple !== undefined) {
      this.at += 2;
      return simple;
    }
    if (c !== 'u') this.fail('invalid escape');
    const digits = this.text.slice(this.at + 2, this.at + 6);
    if (!/^[0-9a-fA-F]{4}$/.test(digits)) this.fail('invalid unicode escape');
    this.at += 6;
    return String.fromCharCode(Number.parseInt(digits, 16));
  }
}
