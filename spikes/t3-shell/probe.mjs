#!/usr/bin/env node
// Offline reload probe for the app shell spike (ARCHITECTURE 5.10, SPEC AC-62, T46).
//
// Drives headless Chromium against the probe server through these steps:
//   1. first load online; the shell of build A gets installed
//   2. a message sent online reaches the room log
//   3. the server stops (offline); a second message stays pending
//   4. reload while offline: the app renders from the shell, log and pending message visible
//   5. Cache Storage holds exactly the files of build A, no API answer
//   6. the server comes back; the pending message arrives
//   7. build B is installed in the background and activated on the next load; cache A deleted
//   8. a broken build fails to install; build B stays active and its cache stays
//
// Usage: node spikes/t3-shell/probe.mjs [--shell-dir DIR]
// Exit code 0 when every step passed, 1 on a failed step, 2 when it cannot run (no Chromium,
// no shell runtime). This is a development probe; evidence for AC-62 is the browser job.

import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { findChromium, launch, waitFor } from './cdp.mjs';
import { DEFAULT_SHELL_DIR, SHELL_MODULES, createProbeServer } from './server.mjs';

const ONLINE_TEXT = 'hello while online';
const OFFLINE_TEXT = 'hello while offline';

/** Reads `--shell-dir DIR` from the arguments. */
export function parseArgs(argv) {
  const out = { shellDir: DEFAULT_SHELL_DIR };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--shell-dir' && i + 1 < argv.length) out.shellDir = argv[++i];
    else throw new Error(`unknown argument ${JSON.stringify(argv[i])}`);
  }
  return out;
}

/**
 * Compares the content of Cache Storage with the manifest of one build. Returns a list of
 * problems; empty means the caches hold exactly the shell files of that build.
 *
 * @param {Record<string, string[]>} caches cache name to request URLs
 * @param {{build: string, files: string[]}} manifest
 * @param {string} origin scope URL ending in '/'
 */
export function cacheProblems(caches, manifest, origin) {
  const problems = [];
  const expectedName = `ostrel-shell-${manifest.build}`;
  const names = Object.keys(caches);
  if (names.length !== 1 || names[0] !== expectedName) {
    problems.push(`expected only cache ${expectedName}, found [${names.join(', ')}]`);
  }
  const expected = new Set(manifest.files.map((f) => new URL(f, origin).href));
  for (const [name, urls] of Object.entries(caches)) {
    for (const url of urls) {
      if (new URL(url).pathname.startsWith('/api/')) problems.push(`${name} holds API answer ${url}`);
      else if (!expected.has(url)) problems.push(`${name} holds non manifest entry ${url}`);
    }
  }
  const held = new Set(caches[expectedName] || []);
  for (const url of expected) if (!held.has(url)) problems.push(`${expectedName} misses ${url}`);
  return problems;
}

const PAGE_STATE = `(() => ({
  stale: window.shellProbeStale === true,
  probe: window.shellProbe || null,
  log: [...document.querySelectorAll('#log li')].map((li) => li.textContent),
  pending: [...document.querySelectorAll('#pending li')].map((li) => li.textContent),
}))()`;

const CACHE_STATE = `(async () => {
  const out = {};
  for (const name of await caches.keys()) {
    const cache = await caches.open(name);
    out[name] = (await cache.keys()).map((request) => request.url);
  }
  return out;
})()`;

const REGISTRATION_STATE = `navigator.serviceWorker.getRegistration().then((r) => r
  ? { active: !!r.active, waiting: !!r.waiting, installing: !!r.installing }
  : null)`;

function sendExpression(text) {
  return `(() => {
    document.getElementById('text').value = ${JSON.stringify(text)};
    document.getElementById('send').requestSubmit();
    return true;
  })()`;
}

async function runSteps(page, server, report) {
  const state = () => page.evaluate(PAGE_STATE);
  const readyState = (what, accept, timeoutMs) =>
    waitFor(
      what,
      async () => {
        const s = await state();
        return !s.stale && s.probe && s.probe.ready && accept(s) ? s : undefined;
      },
      timeoutMs,
    );
  const registration = () => page.evaluate(REGISTRATION_STATE);
  // Marks the current document, so a check never mistakes it for the reloaded one.
  const reload = async () => {
    await page.evaluate('window.shellProbeStale = true');
    await page.send('Page.reload');
  };
  const checkCaches = async (manifest) => {
    const problems = cacheProblems(await page.evaluate(CACHE_STATE), manifest, server.url);
    if (problems.length > 0) throw new Error(problems.join('; '));
  };

  await report('first load online installs the shell of build A', async () => {
    await page.send('Page.navigate', { url: server.url });
    await readyState('build A', (s) => s.probe.build === 'A');
    await waitFor('active service worker', async () => ((await registration())?.active ? true : undefined), 15000);
    const manifest = server.manifest;
    await waitFor('complete shell cache', async () => {
      const problems = cacheProblems(await page.evaluate(CACHE_STATE), manifest, server.url);
      return problems.length === 0 ? true : undefined;
    });
  });

  await report('a message sent online reaches the room log', async () => {
    await page.evaluate(sendExpression(ONLINE_TEXT));
    await readyState('online message in log', (s) => s.log.includes(ONLINE_TEXT) && s.pending.length === 0);
  });

  await report('offline: a second message stays pending', async () => {
    await server.stop();
    const reachable = await fetch(server.url).then(() => true, () => false);
    if (reachable) throw new Error('server still reachable after stop');
    await page.evaluate(sendExpression(OFFLINE_TEXT));
    await readyState('pending message', (s) => s.pending.includes(OFFLINE_TEXT));
  });

  await report('offline reload renders the app with room log and pending message', async () => {
    await reload();
    const s = await readyState(
      'offline reload',
      (st) => st.log.includes(ONLINE_TEXT) && st.pending.includes(OFFLINE_TEXT),
    );
    if (!s.probe.controlled) throw new Error('page is not controlled by the service worker');
    if (s.probe.build !== 'A') throw new Error(`expected build A, got ${s.probe.build}`);
  });

  await report('Cache Storage holds exactly the files of build A', async () => {
    await checkCaches(server.manifest);
  });

  await report('back online: the pending message arrives', async () => {
    await server.start();
    await readyState(
      'synced log',
      (s) => s.log.includes(OFFLINE_TEXT) && s.pending.length === 0,
    );
    const texts = server.store.list().map((m) => m.text);
    if (texts.join('|') !== `${ONLINE_TEXT}|${OFFLINE_TEXT}`) {
      throw new Error(`server holds [${texts.join(', ')}]`);
    }
  });

  await report('build B installs in the background and activates on the next load', async () => {
    const manifestB = server.setBuild('b');
    await reload();
    await readyState('reload after deploy', () => true);
    await waitFor('waiting build B', async () => ((await registration())?.waiting ? true : undefined), 15000);
    await reload();
    const s = await readyState('build B', (st) => st.probe.build === 'B', 15000);
    if (!s.probe.controlled) throw new Error('build B page is not controlled');
    if (!s.log.includes(OFFLINE_TEXT)) throw new Error('room log lost across the update');
    await waitFor('old caches deleted', async () => {
      const problems = cacheProblems(await page.evaluate(CACHE_STATE), manifestB, server.url);
      return problems.length === 0 ? true : undefined;
    });
  });

  await report('a failed install keeps build B and its cache', async () => {
    const manifestB = server.manifest;
    server.setBuild('broken');
    const before = server.requests.length;
    await reload();
    await readyState('reload after broken deploy', (s) => s.probe.build === 'B');
    await waitFor('broken install to end', async () => {
      // The install must have been tried: the missing file was requested.
      if (!server.requests.slice(before).includes('GET /missing.css')) return undefined;
      const r = await registration();
      return r && r.active && !r.installing && !r.waiting ? true : undefined;
    }, 15000);
    await reload();
    await readyState('build B after failed install', (s) => s.probe.build === 'B');
    await checkCaches(manifestB);
  });
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const missing = SHELL_MODULES.filter((m) => !existsSync(join(args.shellDir, m)));
  if (missing.length > 0) {
    console.error(`shell runtime not found in ${args.shellDir} (missing ${missing.join(', ')})`);
    return 2;
  }
  const chromium = findChromium();
  if (!chromium) {
    console.error('Chromium not found; set CHROMIUM to the browser binary');
    return 2;
  }
  const server = createProbeServer({ shellDir: args.shellDir });
  await server.start();
  const browser = await launch(chromium);
  let n = 0;
  let failed = false;
  const report = async (name, step) => {
    n++;
    if (failed) {
      console.log(`skip ${n} ${name}`);
      return;
    }
    const started = Date.now();
    try {
      await step();
      console.log(`ok ${n} ${name} (${Date.now() - started} ms)`);
    } catch (error) {
      failed = true;
      console.log(`not ok ${n} ${name}: ${error.message}`);
    }
  };
  try {
    await runSteps(browser.page, server, report);
  } finally {
    await browser.close();
    await server.stop();
  }
  console.log(failed ? 'PROBE: FAIL' : `PROBE: PASS (${n} steps)`);
  return failed ? 1 : 0;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().then(
    (code) => process.exit(code),
    (error) => {
      console.error(error);
      process.exit(1);
    },
  );
}
