# Offline reload probe for the app shell (spike)

Development probe for the offline app shell of ARCHITECTURE 5.10 and SPEC AC-62. It drives
headless Chromium against a small chat like fixture app that uses the shell runtime in
`runtime/js/shell/` exactly as the generated client will (`sw.js` per the contract in
`runtime/js/shell/README.md`, `startShell` before any app module).

This is not the AC-62 evidence. That is the browser job test `ac_62_*` on the real chat example.
The probe checks the shell runtime early, before the JS backend emits `sw.js`.

## Run

    node spikes/t3-shell/probe.mjs [--shell-dir DIR]

`DIR` defaults to `runtime/js/shell`. Chromium is looked up in the usual places; `CHROMIUM`
overrides the path. Exit code 0: all steps passed; 1: a step failed; 2: cannot run.
Nothing is installed: the probe uses Node built ins only (`node:http`, the global `WebSocket` of
Node 22 for the DevTools protocol).

## Steps

1. First load online: the shell of build A is installed and its cache is complete.
2. A message sent online reaches the room log.
3. The probe server stops (real offline, no emulation); a second message stays pending.
4. Reload while offline: the page is controlled by the service worker, renders build A, and
   shows the room log and the pending message (from IndexedDB).
5. Cache Storage holds exactly the manifest files of build A: no API answer, no other entry.
6. The server comes back on the same port; the pending message arrives on the server.
7. Build B is served: it installs in the background, is activated on the next load (via
   `startShell`), the room log survives, and only the cache of build B remains.
8. A broken build (manifest lists a file the server answers with 404) fails to install after
   the missing file was requested; build B stays active and its cache stays.

## Files

| File | Purpose |
|---|---|
| `probe.mjs` | The steps above. |
| `server.mjs` | Probe server: fixture, generated `sw.js`, shell modules under `runtime/shell/`, `/api/messages`. |
| `cdp.mjs` | Minimal DevTools protocol client and Chromium launcher. |
| `fixture/` | The fixture app (entry, app module with IndexedDB log and outbox, CSS). |
| `test/` | Unit tests of server and helpers, without a browser (`node --test`, run by the gate). |

## Results

Run against the shell runtime of T28 at `b6a35b4`, Chromium 141 (headless), local machine:
all 8 steps pass, three runs in a row, about 5 s per run. Two deliberate defects in a copy of
the runtime are caught: not deleting old caches fails step 7, not answering navigations from
the cache fails step 4.

Observations:

* Activation of a new build takes two loads, as designed: the first load after a deploy finds
  the new worker still installing (here about 1.7 s for the install), the second load lets
  `startShell` activate the waiting worker and reload once (about 1.2 s).
* The offline reload is served entirely from the shell cache; the update check of `sw.js`
  fails silently while offline and does not block the page.
* `sign in` of AC-62 is not covered: the fixture has no auth. The cache check after sign out
  therefore belongs to the browser job on the real chat example.

## Not covered

Several tabs of one origin (finding 2 in `runtime/js/shell/README.md`), storage eviction, and
the generated `sw.js` of `crates/ostrel_codegen_js`.
