import assert from 'node:assert/strict';
const root = process.argv[2];
const { AddWinsSet } = await import(root + '/runtime/js/crdt/set/add_wins_set.mjs');
const id = (r, s) => r.toString(16).padStart(16, '0') + s.toString(16).padStart(8, '0');
const u = (n) => n.toString(16).padStart(32, '0');
// hijack one op
let s = new AddWinsSet();
for (let n = 1; n <= 4; n++) s.apply({ add: u(n) }, id(n, 1));
const all = s.values().flatMap((e) => s.tagsOf(e));
s.apply({ remove: u(1), tags: all }, id(10, 1));
assert.deepEqual(s.values(), [u(2), u(3), u(4)]);
// make tag shared (D106)
s = new AddWinsSet();
for (let n = 1; n <= 4; n++) s.apply({ add: u(n) }, id(7, 1));
s.apply({ remove: u(1), tags: [id(7, 1)] }, id(10, 1));
assert.deepEqual(s.values(), [u(2), u(3), u(4)]);
assert.equal(s.encodeState(), JSON.stringify([2,3,4].map((n) => [u(n), [id(7, 1)]])));
// seq boundary: ffffffff
s = new AddWinsSet();
s.apply({ add: 'x' }, id(1, 0xffffffff));
s.apply({ add: 'x' }, id(1, 0));
assert.deepEqual(s.tagsOf('x'), [id(1, 0xffffffff)]);
console.log('JS red cases OK');
