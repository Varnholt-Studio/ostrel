// Runs the keys.mjs checks inside headless Chromium (local only, D27; not part of the gate).
// Usage: node chromium.mjs /path/to/chrome
// Serves this folder on 127.0.0.1 (a secure context), opens check.html and prints
// the report the page posts back. Exit code 0 only when every check passed.
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';

const chrome = process.argv[2];
if (!chrome) {
  console.error('usage: node chromium.mjs <chrome binary>');
  process.exit(2);
}
const types = { '.html': 'text/html', '.mjs': 'text/javascript' };
const files = new Set(['/check.html', '/keys.mjs', '/vector.mjs']);
let browser;
let profile;

// Stops the browser, removes its fresh profile and sets the exit code.
function finish(code) {
  browser.once('exit', () => rm(profile, { recursive: true, force: true }));
  browser.kill();
  server.close();
  process.exitCode = code;
}

const server = createServer(async (req, res) => {
  if (req.method === 'POST' && req.url === '/report') {
    let body = '';
    for await (const chunk of req) body += chunk;
    res.end();
    const report = JSON.parse(body);
    console.log(JSON.stringify(report, null, 2));
    finish(report.checks.every((c) => c.ok) ? 0 : 1);
    return;
  }
  if (!files.has(req.url)) {
    res.writeHead(404).end();
    return;
  }
  const ext = req.url.slice(req.url.lastIndexOf('.'));
  res.writeHead(200, { 'content-type': types[ext] });
  res.end(await readFile(new URL(`.${req.url}`, import.meta.url)));
});

// A fresh profile per run: Chromium refuses a profile written by a newer version.
profile = await mkdtemp(join(tmpdir(), 'ostrel-t4-identity-'));
server.listen(0, '127.0.0.1', () => {
  const url = `http://127.0.0.1:${server.address().port}/check.html`;
  browser = spawn(chrome, ['--headless', '--no-sandbox', '--disable-gpu', `--user-data-dir=${profile}`, url], {
    stdio: process.env.CHROME_LOG ? 'inherit' : 'ignore',
  });
  setTimeout(() => {
    console.error('timeout: no report from the page');
    finish(1);
  }, 20000).unref();
});
