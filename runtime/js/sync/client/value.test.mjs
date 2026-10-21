// Tests for typed values and the compiled schema lookups. The JSON forms and faults are those
// of `crates/ostrel_sync/src/wire/value.rs`.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { JsonNumber, parseJson } from './json.mjs';
import { ROOMS_SCHEMA } from './testing/fake_server.mjs';
import {
  VALUE_LIMITS,
  ValueFault,
  base64urlDecode,
  base64urlEncode,
  compileSchema,
  readValue,
  writeValue,
} from './value.mjs';

const schema = compileSchema(ROOMS_SCHEMA);
const ROW = '0000000003e80000' + '0123456789abcdef';
const ROW_2 = '0000000003e90000' + '0123456789abcdef';

const t = (kind, more = {}) => ({ kind, ...more });
const read = (type, text) => readValue(type, parseJson(text));

function faults(fn, kind) {
  assert.throws(fn, (error) => {
    assert.ok(error instanceof ValueFault, `${error}`);
    assert.equal(error.kind, kind, error.message);
    return true;
  });
}

test('the schema resolves declared fields, implicit fields and functions', () => {
  assert.deepEqual(schema.modelNames(), ['Room', 'Person']);
  assert.deepEqual(schema.rowFieldType('Room', 'roles'), {
    kind: 'Map',
    key: { kind: 'Ref', model: 1 },
    value: { kind: 'Enum', variants: ['member', 'admin'] },
  });
  // Rows carry made, changed and (with auth) author; ops never write them (D186).
  assert.deepEqual(schema.rowFieldType('Room', 'made'), { kind: 'Time' });
  assert.deepEqual(schema.rowFieldType('Room', 'author'), { kind: 'Ref', model: null });
  assert.equal(schema.opFieldType('Room', 'made'), null);
  assert.equal(schema.opFieldType('Room', 'author'), null);
  assert.equal(schema.rowFieldType('Room', 'nope'), null);
  assert.equal(schema.rowFieldType('Nope', 'name'), null);
  assert.deepEqual(schema.function('touch'), { params: [], result: { kind: 'Unit' } });
  assert.equal(schema.function('nope'), null);

  const withoutAuth = compileSchema({ ...ROOMS_SCHEMA, app: null, functions: undefined });
  assert.equal(withoutAuth.rowFieldType('Room', 'author'), null);
  assert.equal(withoutAuth.function('count'), null);
});

test('a schema that is not of the JS backend form is a defect of the build', () => {
  const bad = (models) => () => compileSchema({ app: null, enums: [], models });
  assert.throws(bad([{ name: 'A', fields: [{ name: 'x', type: 'Nope' }] }]), TypeError);
  assert.throws(bad([{ name: 'A', fields: [{ name: 'x', type: { ref: 3 } }] }]), TypeError);
  assert.throws(bad([{ name: 'A', fields: [{ name: 'x', type: { set: 'Int', list: 'Int' } }] }]), TypeError);
  assert.throws(bad([{ name: 'A', fields: [] }, { name: 'A', fields: [] }]), TypeError);
  assert.throws(bad([{ name: 'A', fields: [{ name: 'x', type: 'Int' }, { name: 'x', type: 'Int' }] }]), TypeError);
});

test('reads every scalar in its wire form', () => {
  assert.equal(read(t('Bool'), 'true'), true);
  assert.equal(read(t('Int'), '-9007199254740991'), -9007199254740991);
  assert.ok(Object.is(read(t('Int'), '-0'), 0));
  assert.equal(read(t('Time'), '1700000000000'), 1700000000000);
  // A Float place accepts any number token; the canonical writer drops a zero fraction.
  assert.equal(read(t('Float'), '2'), 2);
  assert.equal(read(t('Float'), '1.5e3'), 1500);
  assert.ok(Object.is(read(t('Float'), '-0.0'), 0));
  assert.equal(read(t('Text'), '"a\\u00e9"'), 'aé');
  assert.deepEqual(read(t('Bytes'), '"-_8"'), Uint8Array.from([0xfb, 0xff]));
  assert.equal(read(t('Enum', { variants: ['a', 'b'] }), '"b"'), 'b');
  assert.equal(read(t('Ref', { model: 0 }), `"${ROW}"`), ROW);
  assert.equal(read(t('Rank'), '"U"'), 'U');
  assert.equal(read(t('Optional', { inner: t('Int') }), 'null'), null);
  assert.equal(read(t('Optional', { inner: t('Int') }), '3'), 3);
  assert.equal(read(t('Unit'), 'null'), null);
});

test('a value that does not fit its type names the fault of the Rust reader', () => {
  faults(() => read(t('Int'), '"1"'), 'WrongType');
  faults(() => read(t('Int'), '1.0'), 'WrongType');
  faults(() => read(t('Int'), '9007199254740992'), 'IntRange');
  faults(() => read(t('Time'), '1e300'), 'IntRange');
  // The frame reader already refuses `1e400`; the value reader is a second guard (D185).
  faults(() => readValue(t('Float'), new JsonNumber(Infinity, false)), 'WrongType');
  faults(() => read(t('Bool'), '0'), 'WrongType');
  faults(() => read(t('Text'), 'null'), 'WrongType');
  faults(() => read(t('Text'), JSON.stringify('x'.repeat(VALUE_LIMITS.textBytes + 1))), 'TextTooLong');
  faults(() => read(t('Text'), JSON.stringify('é'.repeat(VALUE_LIMITS.textBytes / 2 + 1))), 'TextTooLong');
  faults(() => read(t('Bytes'), '"AA=="'), 'InvalidBytes');
  faults(() => read(t('Enum', { variants: ['a'] }), '"b"'), 'UnknownVariant');
  faults(() => read(t('Ref', { model: 0 }), `"${ROW.toUpperCase()}"`), 'InvalidRef');
  faults(() => read(t('Rank'), '"a0"'), 'InvalidRank');
  faults(() => read(t('Rank'), JSON.stringify('z'.repeat(1025))), 'InvalidRank');
  faults(() => read(t('Unit'), 'false'), 'WrongType');
  faults(() => read(t('Map', { key: t('Int'), value: t('Int') }), '[[1]]'), 'WrongType');
});

test('sets and maps come in compareKey order and refuse a repeated element or key', () => {
  const ints = t('Set', { elem: t('Int') });
  assert.deepEqual(read(ints, '[10,9,-1]'), [-1, 9, 10]);
  faults(() => read(ints, '[0,-0]'), 'Repeated');
  const map = t('Map', { key: t('Text'), value: t('Bool') });
  assert.deepEqual(read(map, '[["b",true],["a",false]]'), [['a', false], ['b', true]]);
  faults(() => read(map, '[["a",true],["a",false]]'), 'Repeated');
  assert.deepEqual(read(t('List', { elem: t('Int') }), '[2,1,2]'), [2, 1, 2]);
});

test('collections nest at most nesting_depth levels; scalars and optionals add none', () => {
  let type = t('Optional', { inner: t('Int') });
  let deepest = '1';
  for (let i = 0; i < VALUE_LIMITS.nestingDepth; i += 1) {
    type = t('List', { elem: type });
    deepest = `[${deepest}]`;
  }
  assert.doesNotThrow(() => read(type, deepest));
  faults(() => read(t('List', { elem: type }), `[${deepest}]`), 'TooDeep');
});

test('base64url is strict and reads back what it writes', () => {
  for (let len = 0; len < 40; len += 1) {
    const bytes = Uint8Array.from({ length: len }, (_, i) => (i * 37 + 251) % 256);
    assert.deepEqual(base64urlDecode(base64urlEncode(bytes)), bytes);
  }
  for (const text of ['AA==', 'AB', 'A', 'AAAAA', '+/', 'AA AA']) {
    assert.equal(base64urlDecode(text), null, text);
  }
  assert.equal(base64urlEncode(Uint8Array.from([0xfb, 0xff])), '-_8');
});

test('writes values in memory in their wire form, checked against the type', () => {
  assert.equal(writeValue(t('Bytes'), Uint8Array.from([0xfb, 0xff])), '-_8');
  assert.deepEqual(writeValue(t('Set', { elem: t('Text') }), ['b', 'a']), ['a', 'b']);
  assert.deepEqual(
    writeValue(t('Map', { key: t('Int'), value: t('Optional', { inner: t('Text') }) }), [[2, null], [1, 'x']]),
    [[1, 'x'], [2, null]],
  );
  assert.ok(Object.is(writeValue(t('Float'), -0), 0));
  assert.equal(writeValue(t('Unit'), null), null);
  faults(() => writeValue(t('Int'), 1.5), 'WrongType');
  faults(() => writeValue(t('Int'), 2 ** 53), 'IntRange');
  faults(() => writeValue(t('Float'), Number.NaN), 'WrongType');
  faults(() => writeValue(t('Float'), Infinity), 'WrongType');
  faults(() => writeValue(t('Bytes'), 'AA'), 'WrongType');
  faults(() => writeValue(t('Enum', { variants: ['a'] }), 'b'), 'UnknownVariant');
  faults(() => writeValue(t('Ref', { model: 0 }), 'x'), 'InvalidRef');
  faults(() => writeValue(t('Set', { elem: t('Int') }), [1, 1]), 'Repeated');
  faults(() => writeValue(t('Text'), 'x'.repeat(VALUE_LIMITS.textBytes + 1)), 'TextTooLong');
  assert.throws(() => writeValue(t('Text'), '\ud800'), /surrogate/);
  // Every value written reads back to the same value.
  const type = schema.rowFieldType('Room', 'roles');
  const roles = [[ROW_2, 'admin'], [ROW, 'member']];
  assert.deepEqual(readValue(type, parseJson(JSON.stringify(writeValue(type, roles)))), [...roles].reverse());
});
