// Minimal Chrome DevTools Protocol client for the shell probe.
//
// Starts headless Chromium with a fresh profile, opens one page and evaluates expressions in
// it. Uses only Node built ins (child_process, fs, the global WebSocket of Node 22), so the
// probe needs no npm packages.

import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

/** Places where Chromium is commonly installed. `CHROMIUM` overrides them. */
export const CHROMIUM_CANDIDATES = [
  '/usr/bin/chromium',
  '/usr/bin/chromium-browser',
  '/usr/bin/google-chrome',
  '/opt/pw-browsers/chromium-1194/chrome-linux/chrome',
];

/** Returns the Chromium binary to use, or null when none is found. */
export function findChromium(env = process.env, exists = existsSync) {
  if (env.CHROMIUM) return exists(env.CHROMIUM) ? env.CHROMIUM : null;
  return CHROMIUM_CANDIDATES.find((path) => exists(path)) || null;
}

/** Command line flags for a headless, isolated browser that never uses a proxy. */
export function chromiumArgs(profileDir) {
  return [
    '--headless=new',
    '--no-sandbox',
    '--no-first-run',
    '--no-default-browser-check',
    '--no-proxy-server',
    '--disable-gpu',
    '--disable-extensions',
    '--remote-debugging-port=0',
    `--user-data-dir=${profileDir}`,
    'about:blank',
  ];
}

/** Parses the DevToolsActivePort file: first line port, second line browser path. */
export function parseActivePort(text) {
  const [port, path] = text.split('\n');
  if (!/^\d+$/.test(port || '') || !(path || '').startsWith('/devtools/browser/')) return null;
  return { port: Number(port), path };
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Rejects when `promise` does not settle within `ms`. */
export function withTimeout(promise, ms, what) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${what} took longer than ${ms} ms`)), ms);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

/**
 * Polls `check` until it returns a value other than undefined, or throws after `timeoutMs`.
 * A single check that hangs (for example during a navigation) counts as a failed attempt.
 */
export async function waitFor(what, check, timeoutMs = 10000, intervalMs = 100) {
  const end = Date.now() + timeoutMs;
  let last;
  for (;;) {
    try {
      const value = await withTimeout(Promise.resolve().then(check), 2000, 'check');
      if (value !== undefined) return value;
    } catch (error) {
      last = error;
    }
    if (Date.now() > end) {
      const reason = last ? `: ${last.message}` : '';
      throw new Error(`timed out waiting for ${what}${reason}`);
    }
    await sleep(intervalMs);
  }
}

/** One CDP session over a WebSocket, with request and response matching by id. */
export class CdpSession {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    socket.addEventListener('message', (event) => this.onMessage(event.data));
    socket.addEventListener('close', () => {
      for (const { reject } of this.pending.values()) reject(new Error('CDP connection closed'));
      this.pending.clear();
    });
  }

  static open(url) {
    return new Promise((resolve, reject) => {
      const socket = new WebSocket(url);
      socket.addEventListener('open', () => resolve(new CdpSession(socket)), { once: true });
      socket.addEventListener('error', () => reject(new Error(`cannot connect to ${url}`)), {
        once: true,
      });
    });
  }

  onMessage(data) {
    let msg;
    try {
      msg = JSON.parse(String(data));
    } catch {
      return;
    }
    const entry = this.pending.get(msg.id);
    if (!entry) return;
    this.pending.delete(msg.id);
    if (msg.error) entry.reject(new Error(`${entry.method}: ${msg.error.message}`));
    else entry.resolve(msg.result);
  }

  send(method, params = {}) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject, method });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }

  /** Evaluates an expression in the page, awaits promises, and returns its JSON value. */
  async evaluate(expression) {
    const result = await this.send('Runtime.evaluate', {
      expression,
      awaitPromise: true,
      returnByValue: true,
    });
    if (result.exceptionDetails) {
      const text = result.exceptionDetails.exception?.description || result.exceptionDetails.text;
      throw new Error(`evaluate failed: ${text}`);
    }
    return result.result.value;
  }

  close() {
    this.socket.close();
  }
}

/** Launches Chromium and returns `{ page, close }` where `page` is a CdpSession of one tab. */
export async function launch(binary) {
  const profileDir = mkdtempSync(join(tmpdir(), 'shell-probe-'));
  const child = spawn(binary, chromiumArgs(profileDir), { stdio: 'ignore' });
  let exited = false;
  child.on('exit', () => {
    exited = true;
  });
  const close = async () => {
    if (!exited) {
      child.kill('SIGKILL');
      await new Promise((resolve) => child.once('exit', resolve));
    }
    // Helper processes may still write to the profile for a moment after the kill.
    try {
      rmSync(profileDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    } catch {
      // A leftover temporary profile is harmless.
    }
  };
  try {
    const active = await waitFor('the DevTools port', () => {
      if (exited) throw new Error('Chromium exited');
      const file = join(profileDir, 'DevToolsActivePort');
      return existsSync(file) ? parseActivePort(readFileSync(file, 'utf8')) || undefined : undefined;
    });
    const targets = await (await fetch(`http://127.0.0.1:${active.port}/json/list`)).json();
    const target = targets.find((t) => t.type === 'page');
    if (!target) throw new Error('no page target');
    const page = await CdpSession.open(target.webSocketDebuggerUrl);
    await page.send('Page.enable');
    await page.send('Runtime.enable');
    return {
      page,
      close: async () => {
        page.close();
        await close();
      },
    };
  } catch (error) {
    await close();
    throw error;
  }
}
