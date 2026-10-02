import { hex, hlc, opId } from './ids.mjs';
import { initialRank, rankBetween } from './rank.mjs';
import { PRIORITIES, STATUSES } from './schema.mjs';

// Seeded generator (mulberry32), R0.3: all workload randomness uses a recorded seed.
export function rng(seed) {
  let a = seed >>> 0;
  const next = () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  next.int = (n) => Math.floor(next() * n);
  next.pick = (xs) => xs[next.int(xs.length)];
  return next;
}

const WORDS = (
  'sync merge replica offline cache query index render patch board column issue label ' +
  'status owner review deploy server client token schema migrate rollback latency budget ' +
  'cursor snapshot outbox leader tab socket frame limit quota rule scope evict reset'
).split(' ');
const LABELS = ['bug', 'feature', 'perf', 'docs', 'security', 'ux', 'infra', 'chore'];
export const SEED_REPLICA = hex(1, 16);
export const SEED_WALL = 1_700_000_000_000;
export const SEED_HLC = hlc(SEED_WALL, 0, SEED_REPLICA);

function sentence(r, min, max) {
  const target = min + r.int(max - min + 1);
  let s = '';
  while (s.length < target) s += (s ? ' ' : '') + r.pick(WORDS);
  return s.slice(0, target);
}

// MEASUREMENT 2.3: titles 20 to 80 chars, descriptions 200 to 2000 chars, 50 users,
// 0 to 5 labels. Comments are not modelled (no probe touches them).
export function makeDataset(seed, count) {
  const r = rng(seed);
  const issues = [];
  let tagSeq = 0;
  for (let i = 0; i < count; i++) {
    const labels = [];
    const n = r.int(6);
    for (let k = 0; k < n; k++) {
      const e = r.pick(LABELS);
      if (!labels.some((l) => l.e === e)) labels.push({ e, tag: opId(SEED_REPLICA, ++tagSeq) });
    }
    issues.push({
      id: hex(SEED_WALL, 12) + hex(0, 4) + hex(i + 1, 16),
      title: sentence(r, 20, 80),
      status: STATUSES[i % STATUSES.length],
      priority: r.pick(PRIORITIES),
      assignee: 'user' + r.int(50),
      rank: initialRank(i),
      labels,
      desc: sentence(r, 200, 2000),
    });
  }
  return issues;
}

// One remote editor. Ops carry contiguous seq numbers and a monotonic HLC.
export class Editor {
  constructor(replicaNumber) {
    this.replica = hex(replicaNumber, 16);
    this.seq = 0;
    this.wall = SEED_WALL + 1000;
    this.counter = 0;
  }

  stamp(wall) {
    if (wall > this.wall) {
      this.wall = wall;
      this.counter = 0;
    } else {
      this.counter++;
    }
    return { id: opId(this.replica, ++this.seq), hlc: hlc(this.wall, this.counter, this.replica) };
  }

  op(wall, row, f, k, extra) {
    return { ...this.stamp(wall), m: 'Issue', row, f, k, ...extra };
  }
}

// Stand in for the server op log: every op the relay forwards for the first time gets the
// next log position `ss` (ServerSeq). An op that already carries one keeps it, which is how
// tests model a reconnect overlap redelivering an op (D62).
let serverSeq = 0;

export function relay(ops) {
  return ops.map((op) => (op.ss === undefined ? { ...op, ss: hex(++serverSeq, 16) } : op));
}

export function message(ops) {
  return JSON.stringify({ v: 0, t: 'Ops', ops: relay(ops) });
}

// Text edit against the current state of `row.desc`: delete `del` visible chars at `pos`,
// then insert `ins` at `pos`. Counters start above everything this replica has seen.
export function textEdit(editor, wall, row, pos, del, ins) {
  const ids = row.desc.visibleIds();
  const ops = [];
  for (let i = 0; i < del && pos + i < ids.length; i++) {
    ops.push(editor.op(wall, row.id, 'desc', 'del', { at: ids[pos + i] }));
  }
  let after = pos > 0 ? ids[pos - 1] : null;
  let c = row.desc.maxCounter;
  for (const ch of ins) {
    c++;
    ops.push(editor.op(wall, row.id, 'desc', 'ins', { after, c, s: ch }));
    after = hex(c, 8) + editor.replica;
  }
  return ops;
}

function neighbourRank(query, row, r) {
  const items = query.items;
  const at = items.indexOf(row);
  let to = Math.max(0, Math.min(items.length - 1, at + r.int(21) - 10));
  if (to === at) to = at === 0 ? 1 : at - 1;
  const lo = to < at ? (to > 0 ? items[to - 1].rank.v : '') : items[to].rank.v;
  const hi = to < at ? items[to].rank.v : to + 1 < items.length ? items[to + 1].rank.v : null;
  return rankBetween(lo, hi);
}

// Conflict probes of MEASUREMENT 2.4: editors A and B change the same target within the
// same 10 ms window, neither having seen the other's change. Returns both editors' ops;
// which one carries the higher stamp is random, and so is the delivery order (harness).
export function probe(kind, r, a, b, wall, row, query) {
  const wa = wall + r.int(10);
  const wb = wall + r.int(10);
  if (kind === 'scalar') {
    const others = STATUSES.filter((s) => s !== row.status.v);
    const va = r.pick(others);
    const vb = r.pick(others.filter((s) => s !== va));
    return {
      a: [a.op(wa, row.id, 'status', 'set', { v: va })],
      b: [b.op(wb, row.id, 'status', 'set', { v: vb })],
    };
  }
  if (kind === 'text') {
    const len = row.desc.text().length;
    const pos = r.int(Math.max(1, len - 20));
    const k = 4 + r.int(12);
    // Half of the probes insert at the same origin (tie order), half inside A's range.
    const at = r.int(2) ? pos : pos + (k >> 1);
    return {
      a: textEdit(a, wa, row, pos, k, sentence(r, 3, 12)),
      b: textEdit(b, wb, row, at, 0, sentence(r, 3, 12)),
    };
  }
  return {
    a: [a.op(wa, row.id, 'rank', 'set', { v: neighbourRank(query, row, r) })],
    b: [b.op(wb, row.id, 'rank', 'set', { v: neighbourRank(query, row, r) })],
  };
}

// Background load: one ordinary edit on a random issue.
export function backgroundEdit(r, editor, wall, store, rows, queries) {
  const row = r.pick(rows);
  switch (r.int(6)) {
    case 0:
      return [editor.op(wall, row.id, 'title', 'set', { v: sentence(r, 20, 80) })];
    case 1:
      return [editor.op(wall, row.id, 'status', 'set', { v: r.pick(STATUSES) })];
    case 2: {
      const e = r.pick(LABELS);
      const tags = row.labels.observed(e);
      if (tags.length) return [editor.op(wall, row.id, 'labels', 'srem', { e, tags })];
      return [editor.op(wall, row.id, 'labels', 'sadd', { e })];
    }
    case 3: {
      const q = queries.find((x) => x.filterValue === row.status.v);
      return [editor.op(wall, row.id, 'rank', 'set', { v: neighbourRank(q, row, r) })];
    }
    case 4:
      return [editor.op(wall, row.id, 'assignee', 'set', { v: 'user' + r.int(50) })];
    default: {
      const len = row.desc.text().length;
      return textEdit(editor, wall, row, r.int(len), r.int(3), sentence(r, 1, 8));
    }
  }
}
