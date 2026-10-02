// Page side of the offline app shell (ARCHITECTURE 5.10, SPEC AC-62).
//
// The HTML entry calls `startShell` before it imports any app module. A build that finished
// installing in the background waits until the next load; `startShell` then activates it and
// reloads once, so the page never runs modules of one build against the cache of another.

import { SKIP_WAITING } from './worker.mjs';

/** Session storage key that stops a reload loop when a waiting build cannot activate. */
export const RELOAD_FLAG = 'ostrel-shell-reloaded';

/** Time the page waits for a waiting build to take control before it gives up. */
export const ACTIVATE_TIMEOUT_MS = 3000;

function readFlag(storage) {
  try {
    return storage ? storage.getItem(RELOAD_FLAG) : null;
  } catch {
    return null;
  }
}

function writeFlag(storage, value) {
  try {
    if (!storage) return;
    if (value === null) storage.removeItem(RELOAD_FLAG);
    else storage.setItem(RELOAD_FLAG, value);
  } catch {
    // Storage may be blocked; the worst case is one extra reload attempt per load.
  }
}

function waitForController(container, timers, timeoutMs) {
  return new Promise((resolve) => {
    const done = (ok) => {
      container.removeEventListener('controllerchange', onChange);
      timers.clearTimeout(timer);
      resolve(ok);
    };
    const onChange = () => done(true);
    const timer = timers.setTimeout(() => done(false), timeoutMs);
    container.addEventListener('controllerchange', onChange);
  });
}

/**
 * Registers the service worker of the build and activates a waiting build.
 *
 * @param {object} host `{ navigator, location, sessionStorage, setTimeout, clearTimeout }`,
 *   normally `window`.
 * @param {string} swUrl URL of the generated service worker, relative to the entry document.
 * @returns {Promise<{status: 'unsupported'|'ready'|'reloading'|'update-stuck'|'failed', error?: unknown}>}
 */
export async function startShell(host, swUrl = 'sw.js') {
  const container = host.navigator && host.navigator.serviceWorker;
  if (!container) return { status: 'unsupported' };
  let registration;
  try {
    registration = await container.register(swUrl, { type: 'module', updateViaCache: 'none' });
  } catch (error) {
    // Without a service worker the app still runs online; offline reload is lost.
    return { status: 'failed', error };
  }
  const waiting = registration.waiting;
  if (!waiting || !container.controller) {
    writeFlag(host.sessionStorage, null);
    return { status: 'ready' };
  }
  if (readFlag(host.sessionStorage) !== null) {
    // We already reloaded for this update and it is still waiting: keep the running build.
    writeFlag(host.sessionStorage, null);
    return { status: 'update-stuck' };
  }
  const timers = { setTimeout: host.setTimeout.bind(host), clearTimeout: host.clearTimeout.bind(host) };
  const changed = waitForController(container, timers, ACTIVATE_TIMEOUT_MS);
  waiting.postMessage({ type: SKIP_WAITING });
  if (!(await changed)) return { status: 'update-stuck' };
  writeFlag(host.sessionStorage, '1');
  host.location.reload();
  return { status: 'reloading' };
}
