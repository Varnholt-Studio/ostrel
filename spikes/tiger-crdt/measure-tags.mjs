#!/usr/bin/env node
// Cost of large `Set` tag lists (risk 7 of ARCHITECTURE 16): decode and merge time of set
// ops on one element that holds T live tags (T replicas added it concurrently), and of
// reading a set with many elements in iteration order.
//   node measure-tags.mjs [--rounds N] [--tags 1,8,64,256] [--elements 1000]
// A remove names at most 64 tags (5.9), so removing T tags takes ceil(T / 64) ops in one
// batch. Numbers are development numbers of a spike, not KPI values (R0.7).
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { decodeOps } from './src/decode.mjs';
import { hex, opId } from './src/ids.mjs';
import { OrSet } from './src/orset.mjs';
import { MAX_REMOVE_TAGS } from './src/schema.mjs';
import { summary } from './src/stats.mjs';
import { Store } from './src/store.mjs';
import { Editor, makeDataset, message, SEED_HLC, SEED_REPLICA } from './src/workload.mjs';

function args(argv) {
  const o = { rounds: 300, tags: [1, 8, 64, 256], elements: 1000 };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--rounds') o.rounds = Number(argv[++i]);
    else if (a === '--tags') o.tags = argv[++i].split(',').map(Number);
    else if (a === '--elements') o.elements = Number(argv[++i]);
  }
  return o;
}

function timed(fn) {
  const t0 = performance.now();
  const out = fn();
  return [performance.now() - t0, out];
}

// One element, T replicas: every round all T replicas add it, then one replica that saw
// every tag removes it (split into ceil(T / 64) ops), then one replica re-adds it while
// the other T - 1 tags are live.
export function measureTags(t, rounds) {
  const store = new Store();
  store.load(makeDataset(7, 1), SEED_HLC, SEED_REPLICA);
  const row = [...store.rows.values()][0];
  const eds = Array.from({ length: t }, (_, i) => new Editor(i + 2));
  const remover = eds[0];
  const s = { addBatchDecode: [], addBatchMerge: [], removeDecode: [], removeMerge: [], readdMerge: [] };
  let wall = remover.wall;
  for (let round = 0; round < rounds; round++) {
    wall += 1;
    const adds = message(eds.map((ed) => ed.op(wall, row.id, 'labels', 'sadd', { e: 'hot' })));
    const [d1, addOps] = timed(() => decodeOps(adds));
    const [m1] = timed(() => store.apply(addOps));
    if (row.labels.observed('hot').length !== t) throw new Error('expected ' + t + ' live tags');

    // Re-add by the last replica while the other tags are live (replaces its own tag).
    const readd = decodeOps(message([eds[t - 1].op(wall, row.id, 'labels', 'sadd', { e: 'hot' })]));
    const [m2] = timed(() => store.apply(readd));

    const tags = row.labels.observed('hot');
    const removes = [];
    for (let i = 0; i < tags.length; i += MAX_REMOVE_TAGS) {
      const part = tags.slice(i, i + MAX_REMOVE_TAGS);
      removes.push(remover.op(wall, row.id, 'labels', 'srem', { e: 'hot', tags: part }));
    }
    const [d3, remOps] = timed(() => decodeOps(message(removes)));
    const [m3] = timed(() => store.apply(remOps));
    if (row.labels.has('hot')) throw new Error('element survived a remove of all tags');

    s.addBatchDecode.push(d1);
    s.addBatchMerge.push(m1);
    s.readdMerge.push(m2);
    s.removeDecode.push(d3);
    s.removeMerge.push(m3);
  }
  return Object.fromEntries(Object.entries(s).map(([k, v]) => [k, summary(v)]));
}

// Reading a set of n elements in code point order (D61), the cost a view pays per render.
export function measureValues(n, rounds) {
  const set = new OrSet();
  const replica = hex(9, 16);
  for (let i = 0; i < n; i++) set.add('label-' + ((i * 7919) % n), opId(replica, i + 1));
  const samples = [];
  for (let i = 0; i < rounds; i++) samples.push(timed(() => set.values())[0]);
  return summary(samples);
}

function main() {
  const o = args(process.argv.slice(2));
  const fmt = (x) => `p50 ${x.p50} p99 ${x.p99} max ${x.max}`;
  console.log(`rounds ${o.rounds} (ms per batch; merge budget 2, decode budget 1)`);
  for (const t of o.tags) {
    const r = measureTags(t, o.rounds);
    console.log(`tags ${t}: ops per remove ${Math.ceil(t / MAX_REMOVE_TAGS)}`);
    for (const [k, v] of Object.entries(r)) console.log(`  ${k.padEnd(15)} ${fmt(v)}`);
  }
  console.log(`values of ${o.elements} elements: ${fmt(measureValues(o.elements, o.rounds))}`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) main();
