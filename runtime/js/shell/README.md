# Offline app shell (runtime part)

Runtime modules for the offline app shell of ARCHITECTURE 5.10 and SPEC AC-62. They have no
dependencies and import only each other.

| File | Runs in | Purpose |
|---|---|---|
| `manifest.mjs` | both | Validates the build manifest and names the cache `ostrel-shell-<build>`. |
| `worker.mjs` | service worker | Install, activate, fetch and message handling (`attachShell`). |
| `register.mjs` | page | Registers the worker and activates a waiting build on the next load (`startShell`). |
| `test/` | Node | Unit tests with in memory Cache Storage and network (`node --test`). |

## Contract with the JS backend

The generated `sw.js` of a build is an ES module (`type: 'module'`, Chromium only per D15):

    import { attachShell } from './runtime/shell/worker.mjs';
    attachShell(self, { build: '<hash>', entry: 'index.html', files: [...], routes: [...] });

The build hash must change whenever any shell file changes, so `sw.js` changes with it and the
browser installs the new build. The HTML entry calls `startShell(window, 'sw.js')` before it
imports any app module.

## Rules the code enforces

* Only manifest files are ever stored. Responses to API calls, the WebSocket, query string
  requests and non GET requests are not intercepted, so Cache Storage never holds row data or a
  token (AC-62 cache check, covered by `worker.test.mjs`).
* An install stores nothing until every file arrived as a direct `200`; redirects and opaque
  answers fail it. A failed install deletes its own cache and the previous shell stays active.
* Activation deletes old `ostrel-shell-*` caches only after the new cache is complete, and never
  touches caches without that prefix.
* Navigations to the scope root, the entry and declared `routes` get the cached entry document,
  also with a query string (for example `?room=7`).

## Findings of the spike

1. A plain reload does not activate a waiting worker (documented browser behaviour: the old page
   stays a client during the navigation; not yet observed in the probe). `startShell` therefore asks the waiting worker to skip waiting and reloads once;
   a session flag prevents a reload loop and a 3 s timeout keeps the running build if the new one
   never takes control.
2. Skip waiting deletes the old caches while other tabs of the old build may still be open. Those
   tabs keep working online (cache misses go to the network) but lose offline reload of lazily
   loaded modules until they reload. Proposal: the tab leader (D32) broadcasts "new build active"
   so other tabs reload; to be decided with the sync tab leader work.
3. The browser may evict Cache Storage. The worker then falls back to the network and does not
   refill the cache outside install. Persistent storage (`navigator.storage.persist()`) is a
   candidate for the client target, not part of this spike.

## Not in this part

The browser probe (first load online, go offline, reload, room log and pending message visible)
lives in `spikes/t3-shell/` (T46). The service worker emitter belongs to `crates/ostrel_codegen_js`.
