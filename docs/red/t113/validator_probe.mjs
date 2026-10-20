const root = process.argv[2];
const { validateVector } = await import(root + '/tests/crdt-vectors/format.mjs');
const v = { strategy: 'set', description: 'make tags', ops: [
 { id: '000000000000000700000001', hlc: '018bcfe5680000000000000000000007', op: { add: 'e1' } },
 { id: '000000000000000700000001', hlc: '018bcfe5680000010000000000000007', op: { add: 'e2' } }],
 deliveries: [[0, 1]], expect: { value: [], state: [] } };
try { validateVector(v, 'set'); console.log('accepted'); } catch (e) { console.log('REJECTED:', e.message); }
const w = { strategy: 'set', description: 'tag names a remove', ops: [
 { id: '000000000000000a00000001', hlc: '018bcfe568000000000000000000000a', op: { add: 'e1' } },
 { id: '000000000000000c00000001', hlc: '018bcfe568010000000000000000000c', op: { remove: 'e1', tags: ['000000000000000a00000001'] } },
 { id: '000000000000000c00000002', hlc: '018bcfe568010001000000000000000c', op: { remove: 'e1', tags: ['000000000000000c00000001'] } }],
 deliveries: [[0, 1, 2]], expect: { value: [], state: [] } };
try { validateVector(w, 'set'); console.log('tag-names-remove accepted'); } catch (e) { console.log('REJECTED:', e.message); }
