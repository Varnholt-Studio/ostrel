// Loader and validator for the CRDT vector files described in README.md.
// A vector that breaks the format is a test failure, never a silently skipped case.

import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { compareKey } from '../../runtime/js/crdt/canon/canon.mjs';

export const VECTOR_ROOT = dirname(fileURLToPath(import.meta.url));

/** Strategy directories that hold op vectors. `canon/` has its own format. */
export const STRATEGIES = ['lww', 'set', 'map', 'rank'];

const OP_ID = /^[0-9a-f]{24}$/;
const HLC = /^[0-9a-f]{32}$/;
const ROW_ID = /^[0-9a-f]{32}$/;
const RANK_KEY = /^[0-9A-Za-z]{0,1023}[1-9A-Za-z]$/; // runtime/js/crdt/rank/rank.mjs, 1 to 1024 digits
const MAX_TAGS_PER_REMOVE = 64; // ARCHITECTURE 5.9
const VECTOR_FIELDS = ['deliveries', 'description', 'expect', 'ops', 'strategy'];

/** Reads and validates every vector of one strategy. Returns `[{ file, vector }]` in file order. */
export function loadVectors(strategy) {
  const dir = join(VECTOR_ROOT, strategy);
  const files = readdirSync(dir)
    .filter((name) => name.endsWith('.json'))
    .sort();
  return files.map((name) => {
    const file = `${strategy}/${name}`;
    const vector = JSON.parse(readFileSync(join(dir, name), 'utf8'));
    try {
      validateVector(vector, strategy);
    } catch (error) {
      throw new Error(`${file}: ${error.message}`);
    }
    return { file, vector };
  });
}

/** Throws with a readable reason when `vector` does not follow README.md. */
export function validateVector(vector, strategy) {
  const fields = Object.keys(vector).sort();
  if (fields.join() !== VECTOR_FIELDS.join()) {
    throw new Error(`fields must be exactly ${VECTOR_FIELDS.join(', ')}, got ${fields.join(', ')}`);
  }
  if (vector.strategy !== strategy) {
    throw new Error(`strategy "${vector.strategy}" does not match directory "${strategy}"`);
  }
  if (typeof vector.description !== 'string' || vector.description.length === 0) {
    throw new Error('description must be a non empty string');
  }
  checkOps(vector.ops);
  checkDeliveries(vector.deliveries, vector.ops.length);
  checkExpect(vector.expect);
  if (strategy === 'set') checkSetCausality(vector.ops, vector.deliveries);
  if (strategy === 'rank') checkRankOps(vector.ops);
}

function checkOps(ops) {
  if (!Array.isArray(ops) || ops.length === 0) throw new Error('ops must be a non empty array');
  const seenIds = new Set();
  const seenHlcs = new Set();
  const lastSeqByReplica = new Map();
  ops.forEach((entry, index) => {
    const where = `ops[${index}]`;
    if (Object.keys(entry).sort().join() !== 'hlc,id,op') {
      throw new Error(`${where} must have exactly the fields id, hlc, op`);
    }
    const { id, hlc } = entry;
    if (!OP_ID.test(id)) throw new Error(`${where}.id is not a 24 digit lowercase hex OpId`);
    if (!HLC.test(hlc)) throw new Error(`${where}.hlc is not a 32 digit lowercase hex Hlc`);
    if (seenIds.has(id)) throw new Error(`${where}.id ${id} is used twice`);
    if (seenHlcs.has(hlc)) throw new Error(`${where}.hlc ${hlc} is used twice`);
    seenIds.add(id);
    seenHlcs.add(hlc);

    const replica = opReplica(id);
    if (hlc.slice(16) !== replica) {
      throw new Error(`${where}: the replica in hlc and id differ`);
    }
    // seq is contiguous per replica (ARCHITECTURE 5.3), so a replica's ops are listed 1, 2, 3...
    const seq = parseInt(id.slice(16), 16);
    const expected = (lastSeqByReplica.get(replica) ?? 0) + 1;
    if (seq !== expected) throw new Error(`${where}: replica ${replica} must use seq ${expected}`);
    lastSeqByReplica.set(replica, seq);
  });
}

function checkDeliveries(deliveries, opCount) {
  if (!Array.isArray(deliveries) || deliveries.length === 0) {
    throw new Error('deliveries must be a non empty array');
  }
  deliveries.forEach((order, index) => {
    const where = `deliveries[${index}]`;
    if (!Array.isArray(order)) throw new Error(`${where} must be an array of op indexes`);
    for (const position of order) {
      if (!Number.isInteger(position) || position < 0 || position >= opCount) {
        throw new Error(`${where} has an invalid op index ${position}`);
      }
    }
    for (let op = 0; op < opCount; op += 1) {
      if (!order.includes(op)) throw new Error(`${where} never delivers ops[${op}]`);
    }
  });
}

function checkExpect(expect) {
  if (typeof expect !== 'object' || expect === null) throw new Error('expect must be an object');
  if (Object.keys(expect).sort().join() !== 'state,value') {
    throw new Error('expect must have exactly the fields value and state');
  }
}

// The server log delivers a set remove only after the adds it names (ARCHITECTURE 6.2), and
// there are no tag tombstones, so a vector may not deliver a named add after its remove.
// A make (D107) tags each of its initial elements with its own id, like one add per element.
function checkSetCausality(ops, deliveries) {
  const indexById = new Map(ops.map((entry, index) => [entry.id, index]));
  let makeIndex;
  ops.forEach(({ op }, index) => {
    const shape = Object.keys(op).sort().join();
    if (shape !== 'add' && shape !== 'make' && shape !== 'remove,tags') {
      const shapes = '{ "add": e } or { "make": [e, ...] } or { "remove": e, "tags": [...] }';
      throw new Error(`ops[${index}] must be ${shapes}`);
    }
    if ('make' in op) {
      checkMake(op.make, index, makeIndex);
      makeIndex = index;
      return;
    }
    if (!('remove' in op)) return;
    if (!Array.isArray(op.tags) || op.tags.length === 0) {
      throw new Error(`ops[${index}]: a remove names at least one tag`);
    }
    if (op.tags.length > MAX_TAGS_PER_REMOVE) {
      throw new Error(`ops[${index}]: a remove names at most ${MAX_TAGS_PER_REMOVE} tags`);
    }
    for (const tag of op.tags) {
      const added = indexById.get(tag);
      if (added === undefined) continue; // a tag the replica never saw is ignored
      // A tag is the OpId of an add or a make. Naming a remove (or the remove itself) is an
      // authoring error in the vector, not a tag a replica could hold, so it is refused.
      const named = ops[added].op;
      if (!('add' in named) && !('make' in named)) {
        const which = `which is the id of the remove ops[${added}]`;
        throw new Error(`ops[${index}] names tag ${tag}, ${which}`);
      }
      // A tag counts only for the element it was created for (D96): a tag of another element,
      // also a make tag of another initial element, is ignored like an unknown one, so its op
      // may also come later or again.
      if (!tagsElement(named, op.remove)) continue;
      deliveries.forEach((order, number) => {
        if (order.lastIndexOf(added) > order.indexOf(index)) {
          const late = `ops[${added}] after its remove ops[${index}]`;
          throw new Error(`deliveries[${number}] delivers ${late}`);
        }
      });
    }
  });
}

// One make per vector (the vector is one row), its elements pairwise different by wire value
// (compareKey, D61); an empty list is allowed (D107).
function checkMake(elements, index, earlier) {
  if (earlier !== undefined) {
    throw new Error(`ops[${index}] is a second make after ops[${earlier}], a vector has one`);
  }
  if (!Array.isArray(elements)) throw new Error(`ops[${index}]: make takes an array of elements`);
  elements.forEach((element, position) => {
    const same = elements.findIndex((other) => compareKey(other, element) === 0);
    if (same !== position) {
      throw new Error(`ops[${index}]: make lists element ${position} twice (also at ${same})`);
    }
  });
}

// Whether the add or make `op` created a tag for `element`.
function tagsElement(op, element) {
  if ('add' in op) return compareKey(op.add, element) === 0;
  return op.make.some((initial) => compareKey(initial, element) === 0);
}

// A rank op writes the key of one row: `{ "set": [row, key] }` with a RowId and a valid key.
function checkRankOps(ops) {
  ops.forEach(({ op }, index) => {
    const where = `ops[${index}]`;
    if (Object.keys(op).join() !== 'set' || !Array.isArray(op.set) || op.set.length !== 2) {
      throw new Error(`${where} must be { "set": [row, key] }`);
    }
    const [row, key] = op.set;
    if (typeof row !== 'string' || !ROW_ID.test(row)) {
      throw new Error(`${where}: row is not a 32 digit lowercase hex RowId`);
    }
    if (typeof key !== 'string' || !RANK_KEY.test(key)) {
      throw new Error(`${where}: "${key}" is not a rank key (1 to 1024 base 62 digits, no trailing 0)`);
    }
  });
}

function opReplica(id) {
  return id.slice(0, 16);
}
