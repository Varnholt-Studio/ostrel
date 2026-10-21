// Application module of an Ostrel program, written by `ostrel build`; edits are overwritten.
import { compareText } from './runtime/crdt/canon/canon.mjs';

const INT_MAX = 9007199254740991;
const TEXT_LIMIT = 16777216;
const MESSAGES = Object.freeze({
  IntOverflow: '`Int` result is outside the range of -9007199254740991 to 9007199254740991',
  DivisionByZero: 'division or remainder by zero',
  TextLimit: '`Text` value would be longer than 16777216 bytes',
});

/** Runtime error of generated code; `kind` is one of `RuntimeErrorKind` (runtime/js/api.d.ts). */
export class OstrelError extends Error {
  constructor(kind, message) {
    super(message);
    this.name = 'OstrelError';
    this.kind = kind;
  }
}

function fail(kind) {
  throw new OstrelError(kind, MESSAGES[kind]);
}

// Int arithmetic (ARCHITECTURE 7.4). The exact result of `+`, `-` and `*` on two safe integers
// is outside the range exactly when the rounded one is, so one check after the operation
// suffices. `+ 0` turns -0 into 0.
function int(r) {
  if (r > INT_MAX || r < -INT_MAX) fail('IntOverflow');
  return r + 0;
}

// `/` truncates toward zero. `a % b` is exact for doubles, so `a - a % b` is an exact multiple
// of `b` and the division below is exact as well.
function idiv(a, b) {
  if (b === 0) fail('DivisionByZero');
  return int((a - (a % b)) / b);
}

// `%` takes the sign of the dividend.
function irem(a, b) {
  if (b === 0) fail('DivisionByZero');
  return (a % b) + 0;
}

// TextLimit counts UTF-8 bytes; a string of n UTF-16 units has at most 3n of them. The parts are
// measured before they are joined: a join above the limit can exceed the longest string the
// engine builds and throw a `RangeError` instead of `TextLimit`.
function concat(parts) {
  let units = 0;
  for (const s of parts) units += s.length;
  if (units > TEXT_LIMIT / 3) {
    let bytes = 0;
    for (const s of parts) {
      bytes += utf8Length(s);
      if (bytes > TEXT_LIMIT) fail('TextLimit');
    }
  }
  return parts.join('');
}

function utf8Length(s) {
  let n = 0;
  for (const ch of s) {
    const c = ch.codePointAt(0);
    n += c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4;
  }
  return n;
}

function freeze(value) {
  if (value !== null && typeof value === 'object') {
    for (const inner of Object.values(value)) freeze(inner);
    Object.freeze(value);
  }
  return value;
}
