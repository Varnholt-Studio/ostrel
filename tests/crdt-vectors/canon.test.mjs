// Runs the shared canonical encoding cases in canon/cases.json against the JavaScript encoder.
// The id cases are for the Rust side (`crates/ostrel_core/tests/canon.rs`); here they are
// checked for internal consistency, so a wrong expectation is caught before Rust relies on it.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

import { encode, InvalidValue } from '../../runtime/js/crdt/canon/canon.mjs';

const { cases } = JSON.parse(readFileSync(new URL('canon/cases.json', import.meta.url), 'utf8'));

// Hex digits per field (ARCHITECTURE 5.3): big endian, in the order the fields compare.
const WIDTH = { replica: 16, serverSeq: 16, seq: 8, wallMs: 12, counter: 4 };
const MAX = { replica: 2n ** 64n - 1n, serverSeq: 2n ** 64n - 1n, seq: 2n ** 32n - 1n,
  wallMs: 2n ** 48n - 1n, counter: 2n ** 16n - 1n };

function hex(field, decimal) {
  const value = BigInt(decimal);
  assert.ok(value >= 0n && value <= MAX[field], `${field} ${decimal} is out of range`);
  return value.toString(16).padStart(WIDTH[field], '0');
}

// Hlc and RowId share one layout: wall_ms, counter, replica.
const clockLayout = ({ wall_ms, counter, replica }) =>
  hex('wallMs', wall_ms) + hex('counter', counter) + hex('replica', replica);

const ID_KINDS = {
  replica_id: (input) => hex('replica', input),
  server_seq: (input) => hex('serverSeq', input),
  op_id: ({ replica, seq }) => hex('replica', replica) + hex('seq', seq),
  hlc: clockLayout,
  row_id: clockLayout,
};

test('case names are unique', () => {
  const names = cases.map((entry) => entry.name);
  assert.equal(new Set(names).size, names.length);
});

for (const entry of cases) {
  test(`canon ${entry.name}`, () => {
    if (entry.kind === 'value') {
      assert.equal(encode(entry.input), entry.canonical);
    } else if (entry.kind === 'utf16') {
      const text = String.fromCharCode(...entry.input);
      if (entry.error === 'Invalid') {
        assert.throws(() => encode(text), InvalidValue);
      } else {
        assert.equal(encode(text), entry.canonical);
      }
    } else if (entry.kind in ID_KINDS) {
      assert.equal(ID_KINDS[entry.kind](entry.input), entry.canonical);
    } else {
      assert.fail(`unknown case kind ${entry.kind}`);
    }
  });
}
