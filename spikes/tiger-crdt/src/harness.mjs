import { seqId } from './ids.mjs';
import { handleMessage } from './pipeline.mjs';
import { STATUSES } from './schema.mjs';
import { summary } from './stats.mjs';
import { Store } from './store.mjs';
import { Board } from './view.mjs';
import {
  backgroundEdit,
  Editor,
  makeDataset,
  message,
  probe,
  rng,
  SEED_HLC,
  SEED_REPLICA,
} from './workload.mjs';

export const DEFAULTS = {
  seed: 42,
  count: 10000,
  probes: 1000,
  warmup: 100,
  background: 2, // background messages before every probe
  windowSize: 30,
  pauseMs: 0,
};

const MIX = ['scalar', 'scalar', 'text', 'text', 'reorder']; // 40 / 40 / 20

// Expected state computed independently of the receiving replica: scalar and rank by
// the HLC rule, text by replaying the two editors' ops in the opposite delivery order on
// a copy.
function oracle(kind, first, second, row) {
  if (kind !== 'text') {
    const x = first[0];
    const y = second[0];
    return x.hlc > y.hlc ? x.v : y.v;
  }
  const copy = row.desc.clone();
  for (const op of [...second, ...first]) {
    if (op.k === 'ins') copy.insert(op.after, seqId(op.c, op.id.slice(0, 16)), op.s);
    else copy.remove(op.at);
  }
  return copy.text();
}

function sortedAround(query, row) {
  const items = query.items;
  const at = items.indexOf(row);
  if (at < 0) return false;
  const ok = (i) => query.compare(items[i].rank.v, items[i].id, items[i + 1].rank.v, items[i + 1].id) < 0;
  return (at === 0 || ok(at - 1)) && (at === items.length - 1 || ok(at));
}

// Runs the spike workload. `env` supplies the DOM and the clock:
// { doc, root, now, nextFrame, sleep }.
export async function runSpike(env, options = {}) {
  const o = { ...DEFAULTS, ...options };
  const r = rng(o.seed);
  const tSetup = env.now();
  const store = new Store();
  store.load(makeDataset(o.seed, o.count), SEED_HLC, SEED_REPLICA);
  const board = new Board(env.doc, env.root, store, STATUSES, o.windowSize);
  const setupMs = env.now() - tSetup;
  const rows = [...store.rows.values()];
  const queries = board.columns.map((c) => c.query);
  const editors = [new Editor(2), new Editor(3), new Editor(4)];
  let wall = editors[0].wall;
  const steps = ['decode', 'merge', 'query', 'view', 'frame', 'total'];
  const samples = Object.fromEntries(steps.map((s) => [s, []]));
  const byKind = { scalar: [], text: [], reorder: [] };
  const failures = [];

  for (let i = 0; i < o.warmup + o.probes; i++) {
    for (let b = 0; b < o.background; b++) {
      wall += 5;
      const ed = editors[r.int(3)];
      handleMessage(message(backgroundEdit(r, ed, wall, store, rows, queries)), store, board, env.now);
    }

    // Pick a target inside a column's viewport and show it in the detail panel.
    const col = board.columns[r.int(board.columns.length)];
    col.scrollTo(r.int(Math.max(1, col.query.items.length - o.windowSize)));
    const visible = col.visible();
    const row = visible[r.int(visible.length)];
    board.detail.show(row);

    const kind = MIX[r.int(MIX.length)];
    wall += 5;
    const p = probe(kind, r, editors[0], editors[1], wall, row, col.query);
    const [first, second] = r.int(2) ? [p.a, p.b] : [p.b, p.a];
    const expected = oracle(kind, first, second, row);
    const text = message([...first, ...second]);
    if (o.pauseMs) await env.sleep(o.pauseMs);

    const t0 = env.now();
    const s = handleMessage(text, store, board, env.now);
    await env.nextFrame();
    const total = env.now() - t0;

    const field = kind === 'scalar' ? 'status' : kind === 'text' ? 'desc' : 'rank';
    const shown = field === 'rank' ? row.rank.v : board.detail.rendered(field);
    const inOrder = sortedAround(queries[STATUSES.indexOf(row.status.v)], row);
    if (shown !== expected || !inOrder) {
      failures.push({ probe: i, kind, row: row.id, field, inOrder });
    }
    if (i < o.warmup) continue;
    samples.decode.push(s.decode);
    samples.merge.push(s.merge);
    samples.query.push(s.query);
    samples.view.push(s.view);
    samples.frame.push(total - s.decode - s.merge - s.query - s.view);
    samples.total.push(total);
    byKind[kind].push(total);
  }

  return {
    options: o,
    setupMs: Math.round(setupMs),
    failures,
    steps: Object.fromEntries(steps.map((k) => [k, summary(samples[k])])),
    kinds: Object.fromEntries(Object.keys(byKind).map((k) => [k, summary(byKind[k])])),
    raw: samples,
  };
}
