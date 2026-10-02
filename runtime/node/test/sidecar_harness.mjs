// Test harness for the Node sidecar host (ARCHITECTURE 7.2, D60).
//
// Spawns a host script the way the server starts it: an empty environment, Node's permission
// model with read access limited to the extern directory, one `--module <name>=<absolute path>`
// per extern module, and JSON-RPC 2.0 framed as one JSON object per line on stdin and stdout:
//
//   node --permission --allow-fs-read=<externDir> host.mjs --module <name>=<path> [...]
//
// Every message the tests send is built by the functions below, so a change of the wire schema
// is a change in one place. Notifications from the host (the `ready` line) are kept apart from
// answers, so counting answers never counts `ready`.

import { spawn } from 'node:child_process';

export const MAX_MESSAGE_BYTES = 1024 * 1024;
export const MAX_OPEN_CALLS = 64;
export const PROTOCOL_VERSION = 1;

export const Codes = Object.freeze({
  PARSE_ERROR: -32700,
  INVALID_REQUEST: -32600,
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  EXTERN_ERROR: -32000,
});

export function request(id, method, params) {
  const message = { jsonrpc: '2.0', id, method };
  if (params !== undefined) message.params = params;
  return message;
}

export function notification(method, params) {
  const message = { jsonrpc: '2.0', method };
  if (params !== undefined) message.params = params;
  return message;
}

export function pingRequest(id) {
  return request(id, 'ping');
}

export function callRequest(id, module, fn, args = []) {
  return request(id, 'call', { module, fn, args });
}

export function cancelNotification(id) {
  return notification('cancel', { id });
}

// Command line arguments after the host script for a map of module name to absolute path.
export function moduleArgs(modules) {
  const args = [];
  for (const [name, path] of Object.entries(modules)) args.push('--module', `${name}=${path}`);
  return args;
}

export class LineSplitter {
  constructor(onLine, onOversized, maxBytes = MAX_MESSAGE_BYTES) {
    this.onLine = onLine;
    this.onOversized = onOversized;
    this.maxBytes = maxBytes;
    this.parts = [];
    this.size = 0;
    this.overflow = false;
  }

  push(chunk) {
    let start = 0;
    for (;;) {
      const nl = chunk.indexOf(0x0a, start);
      const end = nl === -1 ? chunk.length : nl;
      this.append(chunk.subarray(start, end));
      if (nl === -1) return;
      this.finishLine();
      start = nl + 1;
    }
  }

  append(piece) {
    this.size += piece.length;
    if (this.size > this.maxBytes) {
      this.overflow = true;
      this.parts = [];
    } else if (piece.length > 0) {
      this.parts.push(piece);
    }
  }

  finishLine() {
    if (this.overflow) {
      this.onOversized(this.size);
    } else {
      this.onLine(Buffer.concat(this.parts).toString('utf8'));
    }
    this.parts = [];
    this.size = 0;
    this.overflow = false;
  }

  // Bytes after the last newline when the stream ends; the protocol requires a final newline.
  pendingBytes() {
    return this.size;
  }
}

export class Sidecar {
  // `hostArgs` are passed after the host script; use `moduleArgs` to build them.
  constructor(hostPath, externDir, hostArgs = [], { responseTimeoutMs = 3000 } = {}) {
    this.responseTimeoutMs = responseTimeoutMs;
    // Every parsed stdout line in arrival order, `answers` only those with an id member.
    this.lines = [];
    this.answers = [];
    this.notifications = [];
    this.badLines = [];
    this.oversized = [];
    this.stderr = '';
    this.waiters = [];
    const nodeArgs = [
      '--permission',
      `--allow-fs-read=${externDir}`,
      hostPath,
      ...hostArgs,
    ];
    this.child = spawn(process.execPath, nodeArgs, { env: {}, stdio: ['pipe', 'pipe', 'pipe'] });
    this.splitter = new LineSplitter(
      (line) => this.onLine(line),
      (size) => {
        this.oversized.push(size);
        this.notify();
      },
    );
    this.child.stdout.on('data', (chunk) => this.splitter.push(chunk));
    this.child.stderr.on('data', (chunk) => {
      this.stderr += chunk.toString('utf8');
    });
    // Writes after the host died must not crash the test runner.
    this.child.stdin.on('error', () => {});
    this.exited = new Promise((resolve) => {
      this.child.on('close', (code, signal) => {
        this.exit = { code, signal };
        this.notify();
        resolve(this.exit);
      });
    });
  }

  onLine(line) {
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      this.badLines.push(line);
      this.notify();
      return;
    }
    this.lines.push(message);
    if (message !== null && typeof message === 'object' && Object.hasOwn(message, 'id')) {
      this.answers.push(message);
    } else {
      this.notifications.push(message);
    }
    this.notify();
  }

  notify() {
    for (const w of this.waiters.splice(0)) w();
  }

  // Resolves with the first value `pick` returns that is not undefined. Rejects when the host
  // exits first or nothing arrives in time.
  waitFor(pick, what, timeoutMs = this.responseTimeoutMs) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        cleanup();
        reject(new Error(`timeout after ${timeoutMs} ms waiting for ${what}; stderr: ${this.stderr}`));
      }, timeoutMs);
      const check = () => {
        const found = pick();
        if (found !== undefined) {
          cleanup();
          resolve(found);
        } else if (this.exit) {
          cleanup();
          reject(new Error(`host exited (${JSON.stringify(this.exit)}) before ${what}; stderr: ${this.stderr}`));
        } else {
          this.waiters.push(check);
        }
      };
      const cleanup = () => clearTimeout(timer);
      check();
    });
  }

  sendRaw(text) {
    this.child.stdin.write(text);
  }

  send(message) {
    this.sendRaw(`${JSON.stringify(message)}\n`);
  }

  response(id, timeoutMs) {
    return this.waitFor(() => this.answers.find((m) => m.id === id), `response id ${id}`, timeoutMs);
  }

  // Resolves with the `ready` notification the host sends once all modules are loaded.
  ready(timeoutMs) {
    return this.waitFor(
      () => this.notifications.find((m) => m?.method === 'ready'),
      'ready notification',
      timeoutMs,
    );
  }

  async call(id, module, fn, args = [], timeoutMs = undefined) {
    this.send(callRequest(id, module, fn, args));
    return this.response(id, timeoutMs);
  }

  async ping(id, timeoutMs = undefined) {
    this.send(pingRequest(id));
    return this.response(id, timeoutMs);
  }

  cancel(id) {
    this.send(cancelNotification(id));
  }

  // Waits for `ms` and reports whether the host is still running.
  async aliveAfter(ms) {
    await new Promise((resolve) => setTimeout(resolve, ms));
    return !this.exit;
  }

  closeInput() {
    this.child.stdin.end();
  }

  async kill() {
    if (!this.exit) this.child.kill('SIGKILL');
    return this.exited;
  }
}
