// Runs browser/check.html in headless Chromium against the real IndexedDB.
//
// Usage: OSTREL_CHROMIUM=/path/to/chrome node runtime/js/sync/outbox/browser/run_chromium.mjs
// Without OSTREL_CHROMIUM the first of chromium, chromium-browser, google-chrome on PATH is
// used. Exit code 0 means every check passed. Not part of the offline gate: this is the local
// development run (D27); the evidence run belongs to the browser job.

import { spawn, spawnSync } from 'node:child_process';
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { extname, join, normalize, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = fileURLToPath(new URL('..', import.meta.url));
const TIMEOUT_MS = 60_000;
const TYPES = { '.html': 'text/html', '.mjs': 'text/javascript' };

function findChromium() {
  if (process.env.OSTREL_CHROMIUM) return process.env.OSTREL_CHROMIUM;
  for (const name of ['chromium', 'chromium-browser', 'google-chrome']) {
    const found = spawnSync('which', [name], { encoding: 'utf8' });
    if (found.status === 0) return found.stdout.trim();
  }
  throw new Error('Chromium not found; set OSTREL_CHROMIUM');
}

// Serves files below ROOT and receives the result posted by check.html.
function startServer(onResult) {
  const server = createServer(async (request, response) => {
    if (request.method === 'POST' && request.url === '/result') {
      let body = '';
      for await (const chunk of request) body += chunk;
      response.end();
      onResult(JSON.parse(body));
      return;
    }
    const path = normalize(join(ROOT, decodeURIComponent(new URL(request.url, 'http://x').pathname)));
    if (!path.startsWith(ROOT.endsWith(sep) ? ROOT : ROOT + sep)) {
      response.writeHead(403).end();
      return;
    }
    try {
      const content = await readFile(path);
      response.writeHead(200, { 'content-type': TYPES[extname(path)] ?? 'text/plain' });
      response.end(content);
    } catch {
      response.writeHead(404).end();
    }
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

async function main() {
  const chromium = findChromium();
  let finish;
  const result = new Promise((resolve) => {
    finish = resolve;
  });
  const server = await startServer((checks) => finish(checks));
  const profile = await mkdtemp(join(tmpdir(), 'ostrel-outbox-'));
  const url = `http://127.0.0.1:${server.address().port}/browser/check.html`;
  const browser = spawn(
    chromium,
    ['--headless=new', '--no-sandbox', '--disable-gpu', `--user-data-dir=${profile}`, url],
    { stdio: 'ignore' },
  );
  const timer = setTimeout(() => finish([{ name: 'finished in time', ok: false }]), TIMEOUT_MS);

  const checks = await result;
  clearTimeout(timer);
  const exited = once(browser, 'exit');
  browser.kill();
  await exited;
  server.close();
  await rm(profile, { recursive: true, force: true });

  for (const { name, ok, detail } of checks) {
    console.log(`${ok ? 'ok    ' : 'FAILED'} ${name}${detail ? ` (${detail})` : ''}`);
  }
  const passed = checks.length > 0 && checks.every((check) => check.ok);
  console.log(passed ? `all ${checks.length} checks passed` : 'some checks failed');
  process.exitCode = passed ? 0 : 1;
}

await main();
