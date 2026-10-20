// Checks the conformance case files in this folder: strict format validation, and a run of every
// case against a small reference model of the driver contract (crates/ostrel_db/src/api.rs), so
// that each expected result follows from the contract and not from one driver's behaviour.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const files = readdirSync(here).filter((f) => f.endsWith('.json')).sort();
const ERRORS = new Set(['Conflict', 'VersionMismatch', 'NotFound', 'Invalid']);
const TYPES = new Set(['Text', 'Int', 'Bool']);
// Int range of ARCHITECTURE 7.4: the safe JavaScript integers.
const INT = [-(2n ** 53n - 1n), 2n ** 53n - 1n];
const U128 = 2n ** 128n - 1n;
const U64 = 2n ** 64n - 1n;
const U32 = 2 ** 32 - 1;
// Largest wall time of an Hlc (48 bits).
const WALL = 2 ** 48 - 1;

function keys(o, allowed, where) {
  for (const k of Object.keys(o)) assert.ok(allowed.includes(k), `${where}: unknown key ${k}`);
}

// Decodes a value; checks it against the field type.
function value(v, field, where) {
  if (v === null) {
    assert.ok(field.optional, `${where}: none in non optional field`);
    return null;
  }
  if (typeof v === 'boolean') {
    assert.equal(field.type, 'Bool', where);
    return v;
  }
  assert.equal(Object.keys(v).length, 1, where);
  if ('int' in v) {
    assert.equal(field.type, 'Int', where);
    assert.match(v.int, /^-?(0|[1-9][0-9]*)$/, where);
    const n = BigInt(v.int);
    assert.ok(n >= INT[0] && n <= INT[1], `${where}: Int out of range`);
    return n;
  }
  assert.equal(field.type, 'Text', where);
  let s;
  if ('text' in v) s = v.text;
  else {
    const [unit, count] = v.text_repeat;
    assert.ok(typeof unit === 'string' && unit.length > 0 && Number.isInteger(count) && count > 0, where);
    s = unit.repeat(count);
  }
  assert.equal(typeof s, 'string', where);
  assert.ok(s.isWellFormed(), `${where}: lone surrogate`);
  return s;
}

function fields(obj, model, where, all) {
  const out = new Map();
  for (const [k, v] of Object.entries(obj)) {
    const f = model.fields.get(Number(k));
    assert.ok(f && String(f.field) === k, `${where}: unknown field ${k}`);
    out.set(f.field, value(v, f, `${where}.${k}`));
  }
  if (all) assert.equal(out.size, model.fields.size, `${where}: insert must set every field`);
  return out;
}

function rowId(s, where) {
  assert.match(s, /^(0|[1-9][0-9]*)$/, where);
  assert.ok(BigInt(s) <= U128, where);
  return BigInt(s);
}

// Code point order (D50): UTF-16 unit order is wrong above U+FFFF.
function cmpText(a, b) {
  const x = [...a];
  const y = [...b];
  for (let i = 0; i < Math.min(x.length, y.length); i++) {
    const d = x[i].codePointAt(0) - y[i].codePointAt(0);
    if (d) return d;
  }
  return x.length - y.length;
}
function cmp(a, b) {
  if (typeof a === 'string') return cmpText(a, b);
  if (a === b) return 0;
  return a < b ? -1 : 1;
}

function replicaId(s, where) {
  assert.match(s, /^(0|[1-9][0-9]*)$/, where);
  assert.ok(BigInt(s) <= U64, where);
  return BigInt(s);
}

// An op id {replica, seq} as a map key.
function opKey(o, where) {
  assert.ok(Number.isInteger(o.seq) && o.seq >= 0 && o.seq <= U32, `${where}: seq`);
  return `${replicaId(o.replica, where)}/${o.seq}`;
}

// An op of the log; checked and returned in a canonical form for comparison.
function logOp(o, models, where) {
  keys(o, ['replica', 'seq', 'wall', 'model', 'row', 'body'], where);
  assert.ok(Number.isInteger(o.wall) && o.wall >= 0 && o.wall <= WALL, `${where}: wall`);
  assert.ok(models.has(o.model), `${where}: unknown model`);
  assert.equal(typeof o.body, 'string', where);
  return { key: opKey(o, where), wall: o.wall, model: o.model, row: rowId(o.row, where), body: o.body };
}

// Reject reasons (ostrel_db::api::check_reject_reason): 1 to 64 bytes of ASCII letters, digits
// and "_" (D120).
function validReason(r) {
  return typeof r === 'string' && /^[A-Za-z0-9_]{1,64}$/.test(r);
}

// Reference model: live rows, the set of ids ever used, and the final outcomes of ops (D110).
function newDb() {
  return { rows: new Map(), taken: new Set(), log: [], rejects: new Map(), last: new Map() };
}
function clone(db) {
  const rows = new Map();
  for (const [id, r] of db.rows) rows.set(id, { ...r, fields: new Map(r.fields) });
  return {
    rows,
    taken: new Set(db.taken),
    log: [...db.log],
    rejects: new Map(db.rejects),
    last: new Map(db.last),
  };
}
function raise(db, key) {
  const [replica, seq] = key.split('/');
  db.last.set(replica, Math.max(db.last.get(replica) ?? 0, Number(seq)));
}
function decided(db, key) {
  return db.rejects.has(key) || db.log.some((o) => o.key === key);
}
function outcome(db, key) {
  if (db.rejects.has(key)) return { rejected: db.rejects.get(key) };
  const i = db.log.findIndex((o) => o.key === key);
  return i < 0 ? null : { position: i + 1, op: db.log[i] };
}
function apply(db, w, models, where) {
  const [op] = Object.keys(w);
  assert.equal(Object.keys(w).length, 1, where);
  if (op === 'append_ops') {
    assert.ok(Array.isArray(w.append_ops), where);
    const ops = w.append_ops.map((o) => logOp(o, models, where));
    const seen = new Set();
    for (const o of ops) {
      if (decided(db, o.key) || seen.has(o.key)) return 'Conflict';
      seen.add(o.key);
    }
    for (const o of ops) {
      db.log.push(o);
      raise(db, o.key);
    }
    return null;
  }
  if (op === 'reject') {
    keys(w.reject, ['replica', 'seq', 'reason'], where);
    const key = opKey(w.reject, where);
    assert.equal(typeof w.reject.reason, 'string', where);
    if (!validReason(w.reject.reason)) return 'Invalid';
    if (decided(db, key)) return 'Conflict';
    db.rejects.set(key, w.reject.reason);
    raise(db, key);
    return null;
  }
  assert.ok(['insert', 'update', 'delete'].includes(op), `${where}: unknown write ${op}`);
  const a = w[op];
  keys(a, ['model', 'row', 'fields', 'expect_version'], where);
  const model = models.get(a.model);
  assert.ok(model, `${where}: unknown model`);
  const id = rowId(a.row, where);
  const r = db.rows.get(id);
  if (op === 'insert') {
    const f = fields(a.fields, model, where, true);
    if (db.taken.has(id)) return 'Conflict';
    db.taken.add(id);
    db.rows.set(id, { model: a.model, version: 1, fields: f });
    return null;
  }
  assert.ok(Number.isInteger(a.expect_version) && a.expect_version > 0, where);
  const f = op === 'update' ? fields(a.fields, model, where, false) : null;
  if (!r || r.model !== a.model) return 'NotFound';
  if (r.version !== a.expect_version) return 'VersionMismatch';
  if (op === 'delete') db.rows.delete(id);
  else {
    for (const [k, v] of f) r.fields.set(k, v);
    r.version += 1;
  }
  return null;
}
function query(db, q, models, where) {
  keys(q, ['model', 'order', 'limit'], where);
  const model = models.get(q.model);
  assert.ok(model, `${where}: unknown model`);
  for (const [f, dir] of q.order) {
    const field = model.fields.get(f);
    assert.ok(field && !field.optional, `${where}: sort on unknown or optional field`);
    assert.ok(dir === 'asc' || dir === 'desc', where);
  }
  assert.ok(q.limit === null || (Number.isInteger(q.limit) && q.limit >= 0), where);
  const lastDesc = q.order.length > 0 && q.order.at(-1)[1] === 'desc';
  const out = [...db.rows].filter(([, r]) => r.model === q.model);
  out.sort(([ia, ra], [ib, rb]) => {
    for (const [f, dir] of q.order) {
      const d = cmp(ra.fields.get(f), rb.fields.get(f));
      if (d) return dir === 'desc' ? -d : d;
    }
    const d = cmp(ia, ib);
    return lastDesc ? -d : d;
  });
  return q.limit === null ? out : out.slice(0, q.limit);
}

function runCase(c, models) {
  keys(c, ['name', 'doc', 'steps'], c.name);
  assert.ok(c.doc.length > 0, `${c.name}: doc`);
  let db = newDb();
  c.steps.forEach((s, i) => {
    const where = `${c.name} step ${i}`;
    keys(s, ['tx', 'query', 'last_seq', 'replay', 'expect'], where);
    if ('last_seq' in s) {
      const replica = replicaId(s.last_seq, where);
      assert.equal(s.expect, db.last.get(String(replica)) ?? 0, `${where}: last_seq`);
      return;
    }
    if ('replay' in s) {
      keys(s.replay, ['replica', 'seq'], where);
      const got = outcome(db, opKey(s.replay, where));
      const e = s.expect;
      if (e === null || 'rejected' in e) {
        assert.deepEqual(e, got, `${where}: replay`);
        return;
      }
      keys(e, ['accepted'], where);
      keys(e.accepted, ['position', 'op'], where);
      assert.ok(got && 'position' in got, `${where}: replay expects an accepted op`);
      assert.equal(e.accepted.position, got.position, `${where}: position`);
      assert.deepEqual(logOp(e.accepted.op, models, where), got.op, `${where}: op`);
      return;
    }
    if ('tx' in s) {
      assert.ok(Array.isArray(s.tx) && s.tx.length > 0, where);
      const work = clone(db);
      let err = null;
      for (const w of s.tx) if ((err = apply(work, w, models, where))) break;
      if (s.expect === 'ok') {
        assert.equal(err, null, where);
        db = work;
      } else {
        keys(s.expect, ['error'], where);
        assert.ok(ERRORS.has(s.expect.error), where);
        assert.equal(err, s.expect.error, where);
      }
      return;
    }
    const model = models.get(s.query.model);
    const got = query(db, s.query, models, where);
    assert.equal(s.expect.length, got.length, `${where}: row count`);
    s.expect.forEach((e, j) => {
      keys(e, ['id', 'version', 'fields'], where);
      const [id, r] = got[j];
      assert.equal(rowId(e.id, where), id, `${where}: row ${j} id`);
      assert.equal(e.version, r.version, `${where}: row ${j} version`);
      assert.deepEqual(fields(e.fields, model, where, true), r.fields, `${where}: row ${j}`);
    });
  });
}

function loadSchema(schema) {
  const models = new Map();
  schema.forEach((m, i) => {
    keys(m, ['model', 'name', 'fields'], 'schema');
    assert.equal(m.model, i, 'schema: models are numbered from 0');
    const fs = new Map();
    m.fields.forEach((f, j) => {
      keys(f, ['field', 'name', 'type', 'optional'], 'schema');
      assert.equal(f.field, j, 'schema: fields are numbered from 0');
      assert.ok(TYPES.has(f.type), `schema: type ${f.type}`);
      fs.set(j, { ...f, optional: f.optional === true });
    });
    models.set(i, { name: m.name, fields: fs });
  });
  return models;
}

const names = new Set();
for (const file of files) {
  const data = JSON.parse(readFileSync(join(here, file), 'utf8'));
  keys(data, ['format', 'ac', 'schema', 'cases'], file);
  assert.equal(data.format, 1, file);
  const models = loadSchema(data.schema);
  const prefix = `ac_${data.ac.slice(3)}_`;
  for (const c of data.cases) {
    test(c.name, () => {
      assert.ok(c.name.startsWith(prefix) && /^[a-z0-9_]+$/.test(c.name), `${c.name}: name`);
      assert.ok(!names.has(c.name), `${c.name}: duplicate`);
      names.add(c.name);
      runCase(c, models);
    });
  }
}

test('reference model refuses a case with a wrong expectation', () => {
  const models = loadSchema([{ model: 0, name: 'M', fields: [{ field: 0, name: 'f', type: 'Text' }] }]);
  const ins = { insert: { model: 0, row: '1', fields: { 0: { text: 'a' } } } };
  const bad = { name: 'ac_31_x', doc: 'x', steps: [{ tx: [ins] }, { tx: [ins], expect: 'ok' }] };
  bad.steps[0].expect = 'ok';
  assert.throws(() => runCase(bad, models));
});

test('text order is by code point, not by UTF-16 unit', () => {
  assert.ok(cmpText('\u{ff01}', '\u{1f600}') < 0);
  assert.ok(cmpText('', '\u0000') < 0 && cmpText('\u0000\u0001', '\u0001') < 0);
});
