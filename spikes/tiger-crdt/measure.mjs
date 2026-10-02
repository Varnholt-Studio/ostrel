#!/usr/bin/env node
// Runs the KPI A budget spike and prints p50, p95, p99 per budget step (ARCHITECTURE 5.8).
//   node measure.mjs                    Node only, DOM replaced by a stub (steps 1 to 3)
//   node measure.mjs --browser          headless Chromium over the DevTools protocol
// Options: --probes N --count N --seed N --pause MS --out FILE --chrome PATH
// Numbers from this script are development numbers of a spike, not KPI values (R0.7).
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir, cpus, totalmem } from 'node:os';
import { dirname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createDocument } from './src/fakedom.mjs';
import { runSpike } from './src/harness.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const BUDGET = { decode: 1, merge: 2, query: 10, view: 20, total: 50 };

function args(argv) {
  const out = { browser: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--browser') out.browser = true;
    else if (a.startsWith('--')) out[a.slice(2)] = argv[++i];
  }
  return out;
}

function options(a) {
  const o = {};
  for (const k of ['probes', 'count', 'seed', 'warmup']) if (a[k] !== undefined) o[k] = Number(a[k]);
  if (a.pause !== undefined) o.pauseMs = Number(a.pause);
  return o;
}

async function runNode(o) {
  const doc = createDocument();
  const env = {
    doc,
    root: doc.body,
    now: () => performance.now(),
    nextFrame: async () => {},
    sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
  };
  return { mode: 'node (stub DOM, view and frame not representative)', ...(await runSpike(env, o)) };
}

const TYPES = { '.html': 'text/html', '.mjs': 'text/javascript' };

function serve() {
  const server = createServer((req, res) => {
    const path = normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname));
    const file = join(HERE, path === '/' ? 'page.html' : path);
    const type = TYPES[file.slice(file.lastIndexOf('.'))];
    if (!file.startsWith(HERE) || !type || !existsSync(file)) {
      res.writeHead(404).end();
      return;
    }
    // Cross origin isolation raises the resolution of performance.now() from 100 us to 5 us.
    res
      .writeHead(200, {
        'content-type': type,
        'cross-origin-opener-policy': 'same-origin',
        'cross-origin-embedder-policy': 'require-corp',
      })
      .end(readFileSync(file));
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

function findChrome(explicit) {
  const candidates = [
    explicit,
    process.env.CHROME,
    '/opt/pw-browsers/chromium-1194/chrome-linux/chrome',
    '/opt/google/chrome/chrome',
    '/usr/bin/chromium',
    '/usr/bin/google-chrome',
  ];
  const found = candidates.find((c) => c && existsSync(c));
  if (!found) throw new Error('no Chromium found, pass --chrome PATH');
  return found;
}

async function waitFor(fn, ms) {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error('timeout');
    await new Promise((r) => setTimeout(r, 50));
  }
}

async function runBrowser(o, chromePath) {
  const server = await serve();
  const url = `http://127.0.0.1:${server.address().port}/`;
  const profile = mkdtempSync(join(tmpdir(), 'kpi-a-spike-'));
  const chrome = spawn(
    chromePath,
    [
      '--headless=new',
      '--no-sandbox',
      '--no-first-run',
      '--disable-gpu',
      '--disable-extensions',
      '--remote-debugging-port=0',
      `--user-data-dir=${profile}`,
      '--window-size=1366,768',
      'about:blank',
    ],
    { stdio: 'ignore' },
  );
  try {
    const portFile = join(profile, 'DevToolsActivePort');
    const port = (await waitFor(() => existsSync(portFile) && readFileSync(portFile, 'utf8').split('\n')[0], 20000)).trim();
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    const page = targets.find((t) => t.type === 'page');
    const ws = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
      ws.onopen = resolve;
      ws.onerror = reject;
    });
    let id = 0;
    const pending = new Map();
    ws.onmessage = (e) => {
      const m = JSON.parse(e.data);
      if (m.id && pending.has(m.id)) {
        pending.get(m.id)(m);
        pending.delete(m.id);
      }
    };
    const send = (method, params = {}) =>
      new Promise((resolve) => {
        pending.set(++id, resolve);
        ws.send(JSON.stringify({ id, method, params }));
      });
    const evaluate = async (expression) => {
      const m = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (m.error || m.result.exceptionDetails) {
        throw new Error(JSON.stringify(m.error || m.result.exceptionDetails));
      }
      return m.result.result.value;
    };
    await send('Page.navigate', { url });
    for (let i = 0; i < 200 && !(await evaluate('window.spikeReady === true')); i++) {
      await new Promise((r) => setTimeout(r, 50));
    }
    const version = await evaluate('navigator.userAgent');
    const isolated = await evaluate('window.crossOriginIsolated');
    const result = await evaluate(`window.runSpike(${JSON.stringify(o)})`);
    ws.close();
    return { mode: 'browser (headless Chromium, real DOM, rAF)', userAgent: version, crossOriginIsolated: isolated, ...result };
  } finally {
    const exited = new Promise((r) => chrome.once('exit', r));
    chrome.kill('SIGKILL');
    await exited;
    server.close();
    rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
}

function table(res) {
  const lines = [`mode: ${res.mode}`, `setup: ${res.setupMs} ms, failures: ${res.failures.length}`];
  if (res.crossOriginIsolated === false) lines.push('warning: timer resolution 100 us (not isolated)');
  lines.push('step      n      p50      p95      p99      max   budget p99');
  const row = (name, s, budget) =>
    `${name.padEnd(8)} ${String(s.n).padStart(5)} ${[s.p50, s.p95, s.p99, s.max]
      .map((x) => x.toFixed(3).padStart(8))
      .join(' ')}   ${budget ?? ''}`;
  for (const [k, s] of Object.entries(res.steps)) lines.push(row(k, s, BUDGET[k]));
  lines.push('by probe kind (total):');
  for (const [k, s] of Object.entries(res.kinds)) lines.push(row(k, s));
  return lines.join('\n');
}

const a = args(process.argv.slice(2));
const o = options(a);
const res = a.browser ? await runBrowser(o, findChrome(a.chrome)) : await runNode(o);
res.environment = {
  label: 'dev box, not official',
  node: process.version,
  cpu: cpus()[0]?.model,
  cores: cpus().length,
  ramGiB: Math.round(totalmem() / 2 ** 30),
};
console.log(table(res));
if (a.out) writeFileSync(a.out, JSON.stringify(res, null, 1));
process.exit(res.failures.length ? 1 : 0);
