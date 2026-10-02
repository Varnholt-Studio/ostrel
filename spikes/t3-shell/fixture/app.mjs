// Fixture app of the shell probe: a single room chat with an offline outbox.
//
// It stands in for generated client code. Data lives in IndexedDB and comes from the API;
// the service worker only ever serves the files of the build (ARCHITECTURE 5.10). The probe
// reads the rendered lists and `window.shellProbe`.

import { startShell } from './runtime/shell/register.mjs';

const RETRY_MS = 300;

function openDb() {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open('shell-probe', 1);
    req.onupgradeneeded = () => {
      req.result.createObjectStore('log', { keyPath: 'id' });
      req.result.createObjectStore('outbox', { keyPath: 'id' });
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function tx(db, store, mode, work) {
  return new Promise((resolve, reject) => {
    const t = db.transaction(store, mode);
    const result = work(t.objectStore(store));
    t.oncomplete = () => resolve(result && 'result' in result ? result.result : undefined);
    t.onerror = () => reject(t.error);
  });
}

const getAll = (db, store) => tx(db, store, 'readonly', (s) => s.getAll());
const put = (db, store, value) => tx(db, store, 'readwrite', (s) => s.put(value));
const del = (db, store, key) => tx(db, store, 'readwrite', (s) => s.delete(key));

function fill(list, items) {
  list.replaceChildren(
    ...items.map((item) => {
      const li = document.createElement('li');
      li.textContent = item.text;
      return li;
    }),
  );
}

async function render(db) {
  const log = (await getAll(db, 'log')).sort((a, b) => a.seq - b.seq);
  const pending = (await getAll(db, 'outbox')).sort((a, b) => a.at - b.at);
  fill(document.getElementById('log'), log);
  fill(document.getElementById('pending'), pending);
}

async function pull(db) {
  const res = await fetch('api/messages', { cache: 'no-store' });
  if (!res.ok) throw new Error(`pull failed: ${res.status}`);
  for (const message of await res.json()) await put(db, 'log', message);
}

async function flush(db) {
  const pending = (await getAll(db, 'outbox')).sort((a, b) => a.at - b.at);
  for (const item of pending) {
    const res = await fetch('api/messages', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ id: item.id, text: item.text }),
    });
    if (!res.ok) throw new Error(`send failed: ${res.status}`);
    await put(db, 'log', await res.json());
    await del(db, 'outbox', item.id);
  }
}

async function sync(db) {
  try {
    await flush(db);
    await pull(db);
  } catch {
    // Offline or server down: keep the outbox and try again later.
  }
  await render(db);
}

async function main() {
  const shell = await startShell(window, 'sw.js');
  window.shellProbe = { shell: shell.status, ready: false };
  if (shell.status === 'reloading') return;
  const { BUILD_LABEL } = await import('./build.mjs');
  const db = await openDb();
  document.getElementById('build').textContent = BUILD_LABEL;
  document.getElementById('shell').textContent = shell.status;
  document.getElementById('send').addEventListener('submit', async (event) => {
    event.preventDefault();
    const input = document.getElementById('text');
    const text = input.value.trim();
    if (!text) return;
    input.value = '';
    await put(db, 'outbox', { id: crypto.randomUUID(), text, at: Date.now() });
    await render(db);
    await sync(db);
  });
  await render(db);
  await sync(db);
  let busy = false;
  setInterval(async () => {
    if (busy) return;
    busy = true;
    await sync(db);
    busy = false;
  }, RETRY_MS);
  window.shellProbe = {
    shell: shell.status,
    build: BUILD_LABEL,
    controlled: navigator.serviceWorker ? navigator.serviceWorker.controller !== null : false,
    ready: true,
  };
}

main().catch((error) => {
  window.shellProbe = { ready: false, error: String(error) };
});
