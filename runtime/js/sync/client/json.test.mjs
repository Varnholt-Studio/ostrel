import { test } from 'node:test';
import assert from 'node:assert/strict';

import { JsonFault, JsonNumber, MAX_DEPTH, parseJson } from './json.mjs';

function fault(text) {
  try {
    parseJson(text);
  } catch (error) {
    assert.ok(error instanceof JsonFault, `${error}`);
    return error;
  }
  return assert.fail(`parsed ${text}`);
}

test('reads values, objects as Maps and numbers with their token form', () => {
  const value = parseJson(' {"a":[1,2.5,-3e2,true,false,null],"b":"x\\n\\u00e9\\ud83d\\ude00"} ');
  assert.ok(value instanceof Map);
  assert.deepEqual([...value.keys()], ['a', 'b']);
  const [one, half, big, ...rest] = value.get('a');
  assert.deepEqual(one, new JsonNumber(1, true));
  assert.deepEqual(half, new JsonNumber(2.5, false));
  assert.deepEqual(big, new JsonNumber(-300, false));
  assert.deepEqual(rest, [true, false, null]);
  assert.equal(value.get('b'), 'x\né😀');
});

test('a key such as __proto__ is plain data', () => {
  const value = parseJson('{"__proto__":{"polluted":true}}');
  assert.equal(value.get('__proto__').get('polluted'), true);
  assert.equal({}.polluted, undefined);
});

test('a repeated key is refused, also in nested objects (D77)', () => {
  assert.equal(fault('{"a":1,"a":1}').kind, 'DuplicateKey');
  assert.equal(fault('{"x":{"b":1,"b":2}}').key, 'b');
});

test('nesting is limited to MAX_DEPTH levels', () => {
  const ok = '['.repeat(MAX_DEPTH) + ']'.repeat(MAX_DEPTH);
  assert.ok(Array.isArray(parseJson(ok)));
  const deep = '['.repeat(MAX_DEPTH + 1) + ']'.repeat(MAX_DEPTH + 1);
  assert.equal(fault(deep).kind, 'TooDeep');
  // Deeper than any stack would allow, still a typed fault.
  assert.equal(fault('{"a":'.repeat(100000)).kind, 'TooDeep');
});

test('malformed texts are refused', () => {
  for (const text of [
    '',
    '{',
    '{"a"}',
    '{"a":1,}',
    '[1,]',
    '[1 2]',
    '01',
    '1.',
    '.5',
    '+1',
    'tru',
    'nul',
    '"a',
    '"\t"',
    '"\\x"',
    '"\\u12"',
    '1e999',
    '{} {}',
    "{'a':1}",
  ]) {
    assert.equal(fault(text).kind, 'Malformed', text);
  }
});

test('an unpaired surrogate is refused, escaped or raw (ARCHITECTURE 5.3)', () => {
  assert.equal(fault('"\\ud800"').kind, 'Malformed');
  assert.equal(fault('"\\ude00x"').kind, 'Malformed');
  assert.equal(fault('"\ud800"').kind, 'Malformed');
});
