// Checks the shared Text vectors in this folder: strict format, and every expectation against two
// independent oracles taken from the rules (code point order, UTF-8 byte order, scalar count), so
// a wrong vector fails in the gate before any implementation runs it. It also checks that the
// file keeps the cases that separate the right rule from the usual wrong ones.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const raw = readFileSync(join(here, 'vectors.json'));
const doc = JSON.parse(raw.toString('utf8'));
const INT = [-(2n ** 53n - 1n), 2n ** 53n - 1n];

function keys(o, required, optional, where) {
  for (const k of required) assert.ok(k in o, `${where}: missing key ${k}`);
  for (const k of Object.keys(o)) {
    assert.ok(required.includes(k) || optional.includes(k), `${where}: unknown key ${k}`);
  }
}

function text(v, where) {
  assert.equal(typeof v, 'object', where);
  assert.equal(Object.keys(v).length, 1, where);
  let s;
  if ('text' in v) s = v.text;
  else if ('text_repeat' in v) {
    const [unit, count] = v.text_repeat;
    assert.equal(v.text_repeat.length, 2, where);
    assert.ok(typeof unit === 'string' && unit.length > 0, `${where}: empty unit`);
    assert.ok(Number.isInteger(count) && count > 0, `${where}: bad count`);
    s = unit.repeat(count);
  } else assert.fail(`${where}: not a text value`);
  assert.equal(typeof s, 'string', where);
  assert.ok(s.isWellFormed(), `${where}: lone surrogate`);
  return s;
}

function int(s, where) {
  assert.equal(typeof s, 'string', where);
  assert.match(s, /^-?(0|[1-9][0-9]*)$/, where);
  const n = BigInt(s);
  assert.ok(n >= INT[0] && n <= INT[1], `${where}: Int out of range`);
  return n;
}

// Oracle 1: code point by code point.
function byCodePoint(a, b) {
  const x = [...a].map((c) => c.codePointAt(0));
  const y = [...b].map((c) => c.codePointAt(0));
  for (let i = 0; i < Math.min(x.length, y.length); i++) {
    if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
  }
  return Math.sign(x.length - y.length);
}
// Oracle 2: UTF-8 bytes, as SQLite BINARY and PostgreSQL COLLATE "C" compare.
const byUtf8 = (a, b) => Math.sign(Buffer.compare(Buffer.from(a, 'utf8'), Buffer.from(b, 'utf8')));
// The usual wrong rules: UTF-16 units (JS `<`) and normalisation.
const byUtf16 = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
const scalars = (s) => [...s].length;

function compare(a, b, where) {
  const r = byCodePoint(a, b);
  assert.equal(byUtf8(a, b), r, `${where}: UTF-8 byte order disagrees with code point order`);
  return r;
}

test('file is ASCII and has the known sections', () => {
  for (const byte of raw) assert.ok(byte < 0x80, 'vectors.json must be ASCII, use JSON escapes');
  keys(doc, ['format', 'ac', 'doc', 'order', 'compare', 'len', 'range'], [], 'file');
  assert.equal(doc.format, 1);
  assert.equal(doc.ac, 'AC-29');
  const names = new Set();
  for (const section of ['order', 'compare', 'len', 'range']) {
    assert.ok(Array.isArray(doc[section]) && doc[section].length > 0, section);
    for (const c of doc[section]) {
      assert.match(c.name, /^[a-z0-9_]+$/, `${section}: name`);
      assert.ok(!names.has(c.name), `duplicate name ${c.name}`);
      names.add(c.name);
    }
  }
});

test('order: every list is strictly ascending by code point and by UTF-8 bytes', () => {
  for (const c of doc.order) {
    keys(c, ['name', 'sorted'], ['doc'], c.name);
    assert.ok(c.sorted.length >= 2, c.name);
    const v = c.sorted.map((x, i) => text(x, `${c.name}[${i}]`));
    for (let i = 0; i + 1 < v.length; i++) {
      for (let j = i + 1; j < v.length; j++) {
        assert.equal(compare(v[i], v[j], `${c.name}[${i}..${j}]`), -1, `${c.name}: ${i} before ${j}`);
      }
    }
    // Shuffled input sorts back to the given order (reverse is the hardest input).
    const sorted = [...v].reverse().sort(byCodePoint);
    assert.deepEqual(sorted, v, c.name);
  }
});

test('compare: expected sign matches both oracles and is antisymmetric', () => {
  for (const c of doc.compare) {
    keys(c, ['name', 'a', 'b', 'expect'], ['doc'], c.name);
    assert.ok([-1, 0, 1].includes(c.expect), c.name);
    const a = text(c.a, `${c.name}.a`);
    const b = text(c.b, `${c.name}.b`);
    assert.equal(compare(a, b, c.name), c.expect, c.name);
    assert.equal(compare(b, a, c.name), -c.expect || 0, `${c.name}: reversed`);
  }
});

test('len: counts Unicode scalar values', () => {
  for (const c of doc.len) {
    keys(c, ['name', 'text', 'expect'], ['doc'], c.name);
    assert.ok(Number.isInteger(c.expect) && c.expect >= 0, c.name);
    assert.equal(scalars(text(c.text, c.name)), c.expect, c.name);
  }
});

test('range: bounds are inclusive on both ends', () => {
  for (const c of doc.range) {
    assert.equal(typeof c.expect, 'boolean', c.name);
    let v;
    let lo;
    let hi;
    if ('text' in c) {
      keys(c, ['name', 'text', 'lo', 'hi', 'expect'], ['doc'], c.name);
      for (const b of [c.lo, c.hi]) assert.ok(Number.isInteger(b) && b >= 0, `${c.name}: bound`);
      v = BigInt(scalars(text(c.text, c.name)));
      lo = BigInt(c.lo);
      hi = BigInt(c.hi);
    } else {
      keys(c, ['name', 'int', 'lo', 'hi', 'expect'], ['doc'], c.name);
      v = int(c.int, `${c.name}.int`);
      lo = int(c.lo, `${c.name}.lo`);
      hi = int(c.hi, `${c.name}.hi`);
    }
    // Empty ranges (lo > hi) are not defined by SYNTAX 4.2 and must not appear.
    assert.ok(lo <= hi, `${c.name}: lo > hi`);
    assert.equal(v >= lo && v <= hi, c.expect, c.name);
  }
});

// The vectors are only worth something if a wrong implementation fails them. These checks keep
// the separating cases from being dropped or weakened.
test('vectors separate code point order from UTF-16 unit order', () => {
  let found = 0;
  for (const c of doc.order) {
    const v = c.sorted.map((x) => text(x, c.name));
    for (let i = 0; i + 1 < v.length; i++) if (byUtf16(v[i], v[i + 1]) !== -1) found++;
  }
  for (const c of doc.compare) {
    if (byUtf16(text(c.a, c.name), text(c.b, c.name)) !== c.expect) found++;
  }
  assert.ok(found >= 2, 'need cases where UTF-16 unit order is wrong');
});

test('vectors separate scalar count from UTF-16 units, UTF-8 bytes and graphemes', () => {
  const lens = doc.len.map((c) => text(c.text, c.name));
  assert.ok(lens.some((s) => s.length !== scalars(s)), 'need a UTF-16 unit counter to fail');
  assert.ok(lens.some((s) => Buffer.byteLength(s) !== scalars(s)), 'need a byte counter to fail');
  const seg = new Intl.Segmenter('en', { granularity: 'grapheme' });
  const graphemes = (s) => [...seg.segment(s)].length;
  assert.ok(lens.some((s) => graphemes(s) !== scalars(s)), 'need a grapheme counter to fail');
  // A range check with the wrong counter must flip at least one range verdict.
  const flips = (count) =>
    doc.range.filter((c) => 'text' in c).some((c) => {
      const n = count(text(c.text, c.name));
      return (n >= c.lo && n <= c.hi) !== c.expect;
    });
  assert.ok(flips((s) => s.length), 'a UTF-16 unit len must fail a range vector');
  assert.ok(flips((s) => Buffer.byteLength(s)), 'a UTF-8 byte len must fail a range vector');
  assert.ok(flips(graphemes), 'a grapheme len must fail a range vector');
});

test('vectors separate inclusive from exclusive bounds', () => {
  const at = (c) => {
    const v = 'text' in c ? BigInt(scalars(text(c.text, c.name))) : BigInt(c.int);
    return { v, lo: BigInt(c.lo), hi: BigInt(c.hi) };
  };
  const r = doc.range.map(at);
  assert.ok(r.some((x, i) => doc.range[i].expect && x.v === x.lo), 'need a value on the lower bound');
  assert.ok(r.some((x, i) => doc.range[i].expect && x.v === x.hi), 'need a value on the upper bound');
  assert.ok(r.some((x, i) => !doc.range[i].expect && x.v === x.lo - 1n), 'need a value below');
  assert.ok(r.some((x, i) => !doc.range[i].expect && x.v === x.hi + 1n), 'need a value above');
});

test('vectors separate stored text from normalised text', () => {
  const pairs = doc.compare.map((c) => [text(c.a, c.name), text(c.b, c.name), c.expect]);
  assert.ok(
    pairs.some(([a, b, e]) => e !== 0 && a.normalize('NFC') === b.normalize('NFC')),
    'need two texts that are equal only after normalisation',
  );
});
