// Probe server for the offline app shell spike (ARCHITECTURE 5.10, SPEC AC-62).
//
// Serves a small chat like fixture app, a service worker `sw.js` written the way the JS backend
// will write it (see runtime/js/shell/README.md), the shell runtime modules under
// `runtime/shell/`, and a tiny in memory message API under `/api/`. The probe stops and starts
// this server to take the app offline and back online, and switches builds to check update
// and failed install behaviour. No dependencies: node:http, node:fs and node:crypto only.

import { createHash } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { createServer } from 'node:http';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));

/** Fixture app files, served from `fixture/`. */
export const FIXTURE_DIR = join(HERE, 'fixture');

/** Default location of the shell runtime (T28) in the repository. */
export const DEFAULT_SHELL_DIR = join(HERE, '..', '..', 'runtime', 'js', 'shell');

/** URL prefix under which the shell runtime modules are served, as in the sw.js contract. */
export const SHELL_PREFIX = 'runtime/shell/';

/** Shell runtime modules that the fixture app and sw.js import. */
export const SHELL_MODULES = ['manifest.mjs', 'worker.mjs', 'register.mjs'];

/** Builds the probe can switch between. `broken` lists a file the server does not have. */
export const BUILDS = Object.freeze({
  a: Object.freeze({ label: 'A', extraFiles: [] }),
  b: Object.freeze({ label: 'B', extraFiles: [] }),
  broken: Object.freeze({ label: 'BROKEN', extraFiles: ['missing.css'] }),
});

/** Largest accepted API request body in bytes. */
export const MAX_BODY = 4096;

/** Largest accepted message text in characters. */
export const MAX_TEXT = 500;

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
};

function typeOf(path) {
  const dot = path.lastIndexOf('.');
  return TYPES[dot >= 0 ? path.slice(dot) : ''] || 'application/octet-stream';
}

/**
 * Reads the static files of the fixture and the shell runtime into memory.
 * Returns a map from URL path (relative to the scope, no leading slash) to file content.
 */
export function loadStatic(shellDir = DEFAULT_SHELL_DIR, fixtureDir = FIXTURE_DIR) {
  const files = new Map();
  for (const name of readdirSync(fixtureDir).sort()) {
    files.set(name, readFileSync(join(fixtureDir, name)));
  }
  for (const name of SHELL_MODULES) {
    files.set(SHELL_PREFIX + name, readFileSync(join(shellDir, name)));
  }
  return files;
}

/** Source of the generated build module that names the running build. */
export function buildModule(label) {
  return Buffer.from(`export const BUILD_LABEL = ${JSON.stringify(label)};\n`);
}

/**
 * Computes the manifest of one build. The build hash covers every path and file content, so it
 * changes whenever any shell file changes (contract in runtime/js/shell/README.md).
 */
export function manifestFor(staticFiles, buildKey) {
  const build = BUILDS[buildKey];
  if (!build) throw new Error(`unknown build ${JSON.stringify(buildKey)}`);
  const contents = new Map(staticFiles);
  contents.set('build.mjs', buildModule(build.label));
  const files = [...contents.keys(), ...build.extraFiles].sort();
  const hash = createHash('sha256');
  for (const path of files) {
    hash.update(path);
    hash.update('\0');
    hash.update(contents.get(path) || '');
    hash.update('\0');
  }
  return {
    manifest: { build: hash.digest('hex').slice(0, 32), entry: 'index.html', files, routes: [] },
    contents,
    label: build.label,
  };
}

/** Source of the generated service worker for a manifest, as the JS backend will emit it. */
export function serviceWorkerSource(manifest) {
  return (
    `import { attachShell } from './${SHELL_PREFIX}worker.mjs';\n` +
    `attachShell(self, ${JSON.stringify(manifest)});\n`
  );
}

/** In memory message store of the probe API. */
export class MessageStore {
  constructor() {
    this.messages = [];
    this.ids = new Set();
  }

  /** Adds a message once per client id. Returns the stored message or an error string. */
  add(body) {
    if (body === null || typeof body !== 'object' || Array.isArray(body)) return 'expected an object';
    const { id, text } = body;
    if (typeof id !== 'string' || !/^[A-Za-z0-9_-]{1,64}$/.test(id)) return 'invalid id';
    if (typeof text !== 'string' || text.length === 0 || text.length > MAX_TEXT) return 'invalid text';
    if (!this.ids.has(id)) {
      this.ids.add(id);
      this.messages.push({ id, text, seq: this.messages.length + 1 });
    }
    return this.messages.find((m) => m.id === id);
  }

  list() {
    return this.messages.map((m) => ({ ...m }));
  }
}

function send(res, status, type, body, extraHeaders = {}) {
  res.writeHead(status, {
    'content-type': type,
    'content-length': Buffer.byteLength(body),
    'x-content-type-options': 'nosniff',
    ...extraHeaders,
  });
  res.end(body);
}

function sendJson(res, status, value) {
  // API answers must never be stored by the browser or the shell (AC-62 cache check).
  send(res, status, TYPES['.json'], JSON.stringify(value), { 'cache-control': 'no-store' });
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    req.on('data', (chunk) => {
      size += chunk.length;
      if (size > MAX_BODY) {
        reject(new Error('body too large'));
        req.destroy();
        return;
      }
      chunks.push(chunk);
    });
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });
}

/**
 * Creates the probe server. The server keeps its state (messages, current build) across
 * `stop()` and `start()`, so the probe can go offline and come back to the same data.
 *
 * @param {{shellDir?: string, fixtureDir?: string, build?: string}} options
 */
export function createProbeServer(options = {}) {
  const staticFiles = loadStatic(options.shellDir, options.fixtureDir);
  const store = new MessageStore();
  const requests = [];
  let current = manifestFor(staticFiles, options.build || 'a');
  let server = null;
  let port = 0;

  async function handle(req, res) {
    const url = new URL(req.url, 'http://probe.invalid');
    const path = url.pathname.replace(/^\/+/, '');
    requests.push(`${req.method} /${path}`);
    if (path === 'api/messages') {
      if (req.method === 'GET') return sendJson(res, 200, store.list());
      if (req.method === 'POST') {
        let body;
        try {
          body = JSON.parse(await readBody(req));
        } catch {
          return sendJson(res, 400, { error: 'invalid body' });
        }
        const result = store.add(body);
        if (typeof result === 'string') return sendJson(res, 400, { error: result });
        return sendJson(res, 200, result);
      }
      return sendJson(res, 405, { error: 'method not allowed' });
    }
    if (req.method !== 'GET') return send(res, 405, 'text/plain', 'method not allowed');
    if (path === 'sw.js') {
      return send(res, 200, TYPES['.js'], serviceWorkerSource(current.manifest), {
        'cache-control': 'no-store',
      });
    }
    const name = path === '' ? 'index.html' : path;
    const body = current.contents.get(name);
    if (!body) return send(res, 404, 'text/plain', 'not found');
    return send(res, 200, typeOf(name), body, { 'cache-control': 'no-cache' });
  }

  return {
    store,
    requests,
    get manifest() {
      return current.manifest;
    },
    get label() {
      return current.label;
    },
    get port() {
      return port;
    },
    get url() {
      return `http://127.0.0.1:${port}/`;
    },
    get online() {
      return server !== null;
    },
    /** Switches the build served from now on. */
    setBuild(key) {
      current = manifestFor(staticFiles, key);
      return current.manifest;
    },
    /** Starts listening, on the previous port when there was one. */
    start() {
      return new Promise((resolve, reject) => {
        const s = createServer((req, res) => {
          handle(req, res).catch(() => {
            if (!res.headersSent) send(res, 500, 'text/plain', 'internal error');
            else res.destroy();
          });
        });
        s.once('error', reject);
        s.listen(port, '127.0.0.1', () => {
          server = s;
          port = s.address().port;
          resolve(port);
        });
      });
    },
    /** Stops listening and drops open connections, which takes the app offline. */
    stop() {
      if (!server) return Promise.resolve();
      const s = server;
      server = null;
      return new Promise((resolve) => {
        s.close(() => resolve());
        s.closeAllConnections();
      });
    },
  };
}
