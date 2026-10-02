// Service worker side of the offline app shell (ARCHITECTURE 5.10, SPEC AC-62).
//
// The generated `sw.js` of a build imports this module and calls `attachShell(self, manifest)`.
// Behaviour:
//   * install:  fetch every manifest file and store it in the cache of this build. The cache
//               is only filled once every file arrived intact; any failure deletes the new cache
//               and fails the install, so the previous shell stays in charge.
//   * activate: delete the shell caches of every other build. Never runs before the cache of
//               this build exists. Caches that do not belong to the shell are left alone.
//   * fetch:    answer GET requests for manifest files cache first, and navigations to the scope
//               root or a declared route with the entry document. Everything else (API calls,
//               the WebSocket, data) is not intercepted and never stored.
//   * message:  `{ type: SKIP_WAITING }` from a window lets the page activate a waiting build
//               before its own modules load (see register.mjs).
// The functions below take their environment as an argument so they run unchanged in tests.

import { isShellCache, parseManifest } from './manifest.mjs';

/** Message type a page sends to activate a waiting build. */
export const SKIP_WAITING = 'ostrel-shell-skip-waiting';

/** Thrown when a file of the manifest cannot be fetched for the cache. */
export class InstallError extends Error {
  constructor(message) {
    super(message);
    this.name = 'InstallError';
  }
}

/**
 * @typedef {object} ShellEnv
 * @property {CacheStorage} caches
 * @property {(input: RequestInfo, init?: RequestInit) => Promise<Response>} fetch
 * @property {string} scope absolute URL of the service worker scope, ending in '/'
 */

/** Absolute URL of a manifest path inside the scope. */
export function shellUrl(env, path) {
  return new URL(path, env.scope).href;
}

function checkResponse(path, response) {
  if (!response || response.status !== 200 || response.redirected) {
    const status = response ? response.status : 'none';
    throw new InstallError(`${path}: expected a direct 200 response, got ${status}`);
  }
  if (response.type === 'opaque' || response.type === 'opaqueredirect' || response.type === 'error') {
    throw new InstallError(`${path}: unusable response type ${response.type}`);
  }
}

async function isComplete(env, manifest) {
  if (!(await env.caches.has(manifest.cacheName))) return false;
  const cache = await env.caches.open(manifest.cacheName);
  for (const file of manifest.files) {
    if (!(await cache.match(shellUrl(env, file)))) return false;
  }
  return true;
}

/**
 * Fills the cache of this build. Resolves with the number of files fetched (0 when the cache
 * was already complete). Rejects without leaving a partial cache behind.
 */
export async function install(env, manifest) {
  if (await isComplete(env, manifest)) return 0;
  // Fetch everything first, so a single failure never leaves a half filled cache.
  const fetched = await Promise.all(
    manifest.files.map(async (file) => {
      const url = shellUrl(env, file);
      let response;
      try {
        response = await env.fetch(url, { cache: 'reload', credentials: 'same-origin', redirect: 'error' });
      } catch (err) {
        throw new InstallError(`${file}: ${err && err.message ? err.message : 'network error'}`);
      }
      checkResponse(file, response);
      return [url, response];
    }),
  );
  try {
    const cache = await env.caches.open(manifest.cacheName);
    for (const [url, response] of fetched) await cache.put(url, response);
  } catch (err) {
    await env.caches.delete(manifest.cacheName);
    throw err;
  }
  return fetched.length;
}

/**
 * Deletes the shell caches of all other builds and resolves with their names. Keeps every
 * cache if the cache of this build is missing or incomplete, so the origin is never left
 * without a working shell.
 */
export async function activate(env, manifest) {
  if (!(await isComplete(env, manifest))) return [];
  const deleted = [];
  for (const name of await env.caches.keys()) {
    if (isShellCache(name) && name !== manifest.cacheName) {
      await env.caches.delete(name);
      deleted.push(name);
    }
  }
  return deleted;
}

async function cacheFirst(env, manifest, path, request) {
  const cache = await env.caches.open(manifest.cacheName);
  const hit = await cache.match(shellUrl(env, path));
  if (hit) return hit;
  // Not cached (for example evicted by the browser): go to the network, but never store the
  // answer, so the cache keeps holding exactly the files of the manifest.
  return env.fetch(request);
}

/**
 * Decides how the shell answers a request. Returns a promise of the response, or null when the
 * request is not part of the app shell and must go to the network untouched.
 */
export function handleFetch(env, manifest, request) {
  if (request.method !== 'GET') return null;
  let url;
  try {
    url = new URL(request.url);
  } catch {
    return null;
  }
  const scope = new URL(env.scope);
  if (url.origin !== scope.origin || !url.pathname.startsWith(scope.pathname)) return null;
  const path = url.pathname.slice(scope.pathname.length);
  if (request.mode === 'navigate') {
    if (path === '' || path === manifest.entry || manifest.routes.includes(path)) {
      return cacheFirst(env, manifest, manifest.entry, request);
    }
    return null;
  }
  if (url.search === '' && manifest.files.includes(path)) {
    return cacheFirst(env, manifest, path, request);
  }
  return null;
}

/**
 * Wires the shell into a service worker global scope. Throws on an invalid manifest, which
 * makes the script evaluation, and with it the install of this build, fail.
 */
export function attachShell(self, rawManifest) {
  const manifest = parseManifest(rawManifest);
  const env = {
    caches: self.caches,
    fetch: (input, init) => self.fetch(input, init),
    scope: self.registration.scope,
  };
  self.addEventListener('install', (event) => {
    event.waitUntil(install(env, manifest));
  });
  self.addEventListener('activate', (event) => {
    event.waitUntil(activate(env, manifest));
  });
  self.addEventListener('fetch', (event) => {
    const response = handleFetch(env, manifest, event.request);
    if (response) event.respondWith(response);
  });
  self.addEventListener('message', (event) => {
    const fromWindow = event.source && event.source.type === 'window';
    if (fromWindow && event.data && event.data.type === SKIP_WAITING) {
      event.waitUntil(self.skipWaiting());
    }
  });
  return manifest;
}
