// Test harness for the Node sidecar host (ARCHITECTURE 7.2).
//
// Spawns a host script the way the server is expected to start it: an empty environment,
// Node's permission model with read access limited to the extern directory, and JSON-RPC 2.0
// framed as one JSON object per line on stdin and stdout.
//
// ASSUMPTION (T48, to be confirmed by T30 and the F3 sidecar RPC schema): the host is started as
//   node --permission --allow-fs-read=<externDir> host.mjs <externDir>
// (the command line of ARCHITECTURE 7.2 plus the extern directory as argument)
// and an extern call is the request
//   {"jsonrpc":"2.0","id":<n>,"method":"call","params":{"module":<file>,"export":<name>,"args":[...]}}
// where <file> is resolved inside <externDir>. Every request shape used by the tests is built by
// `callRequest` below, so a change of the schema is a change in one place.

import { spawn } from 'node:child_process';

export const MAX_MESSAGE_BYTES = 1024 * 1024;

export function callRequest(id, module, exportName, args = []) {
  return { jsonrpc: '2.0', id, method: 'call', params: { module, export: exportName, args } };
}

// Splits a byte stream into lines. Lines longer than `maxBytes` are not kept in memory: they are
// reported once with their length so a test can assert on oversized output without buffering it.
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
  constructor(hostPath, externDir, { extraArgs = [], responseTimeoutMs = 3000 } = {}) {
    this.responseTimeoutMs = responseTimeoutMs;
    this.lines = [];
    this.badLines = [];
    this.oversized = [];
    this.stderr = '';
    this.waiters = [];
    const nodeArgs = [
      '--permission',
      `--allow-fs-read=${externDir}`,
      ...extraArgs,
      hostPath,
      externDir,
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
    try {
      this.lines.push(JSON.parse(line));
    } catch {
      this.badLines.push(line);
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
    return this.waitFor(() => this.lines.find((m) => m && m.id === id), `response id ${id}`, timeoutMs);
  }

  async call(id, module, exportName, args = [], timeoutMs = undefined) {
    this.send(callRequest(id, module, exportName, args));
    return this.response(id, timeoutMs);
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
