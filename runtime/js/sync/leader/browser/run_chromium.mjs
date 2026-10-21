// Two tab check of the tab leader in headless Chromium (AC-59, D32): two real tabs of one
// origin with the real Web Locks, BroadcastChannel and IndexedDB. Tabs are opened, reloaded
// and closed through the DevTools HTTP endpoints, so closing a tab is a real tab close.
//
// Usage: OSTREL_CHROMIUM=/path/to/chrome node runtime/js/sync/leader/browser/run_chromium.mjs
// Without OSTREL_CHROMIUM the first of chromium, chromium-browser, google-chrome on PATH is
// used. Exit code 0 means every check passed. Not part of the offline gate: this is the local
// development run (D27); the evidence run belongs to the browser job.
//
// Covered: exactly one leader for two tabs, messages between follower and leader, takeover
// when the leader reloads, takeover and outbox drain when the leader tab closes, every op
// drained exactly once. Not covered here: the generated chat client, the WebSocket to a real
// server and the second user (the full AC-59 scenario needs the sync client and server).

import { spawn, spawnSync } from 'node:child_process';
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { extname, join, normalize, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as sleep } from 'node:timers/promises';

// Serve runtime/js/sync/ so the tab can import both the leader and the outbox.
const ROOT = fileURLToPath(new URL('../..', import.meta.url));
const TIMEOUT_MS = 90_000;
const WAIT_MS = 15_000;
const TYPES = { '.html': 'text/html', '.mjs': 'text/javascript' };
const DB = 'ostrel-leader-check';
const LOCK = 'ostrel-sync-check';

function findChromium() {
  if (process.env.OSTREL_CHROMIUM) return process.env.OSTREL_CHROMIUM;
  for (const name of ['chromium', 'chromium-browser', 'google-chrome']) {
    const found = spawnSync('which', [name], { encoding: 'utf8' });
    if (found.status === 0) return found.stdout.trim();
  }
  throw new Error('Chromium not found; set OSTREL_CHROMIUM');
}

// Events posted by the tabs and command queues per tab name (long poll on /next).
const events = [];
const waiters = new Set();
const commands = new Map();

function queueFor(tab) {
  let queue = commands.get(tab);
  if (!queue) {
    queue = { items: [], polls: [] };
    commands.set(tab, queue);
  }
  return queue;
}

function command(tab, body) {
  const queue = queueFor(tab);
  const poll = queue.polls.shift();
  if (poll) poll(body);
  else queue.items.push(body);
}

function addEvent(event) {
  events.push(event);
  for (const waiter of waiters) waiter();
}

// Resolves with the first event at index `from` or later that matches, or null on timeout.
function waitFor(match, from = 0) {
  return new Promise((resolve) => {
    let timer;
    const look = () => {
      const found = events.slice(from).find(match);
      if (!found) return;
      waiters.delete(look);
      clearTimeout(timer);
      resolve(found);
    };
    timer = setTimeout(() => {
      waiters.delete(look);
      resolve(null);
    }, WAIT_MS);
    waiters.add(look);
    look();
  });
}

function startServer() {
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://x');
    if (request.method === 'POST' && url.pathname === '/event') {
      let body = '';
      for await (const chunk of request) body += chunk;
      response.end();
      addEvent(JSON.parse(body));
      return;
    }
    if (url.pathname === '/next') {
      const queue = queueFor(url.searchParams.get('tab'));
      const send = (body) => {
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end(JSON.stringify(body));
      };
      if (queue.items.length > 0) send(queue.items.shift());
      else queue.polls.push(send);
      return;
    }
    const path = normalize(join(ROOT, decodeURIComponent(url.pathname)));
    if (!path.startsWith(ROOT.endsWith(sep) ? ROOT : ROOT + sep)) {
      response.writeHead(403).end();
      return;
    }
    try {
      const content = await readFile(path);
      response.writeHead(200, { 'content-type': TYPES[extname(path)] ?? 'text/plain' });
      response.end(content);
    } catch {
      response.writeHead(404).end();
    }
  });
  server.keepAliveTimeout = 0;
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

// Reads the DevTools port that Chromium writes into the profile once it listens.
async function devtoolsPort(profile) {
  const deadline = Date.now() + WAIT_MS;
  while (Date.now() < deadline) {
    try {
      const text = await readFile(join(profile, 'DevToolsActivePort'), 'utf8');
      const port = Number.parseInt(text.split('\n')[0], 10);
      if (port > 0) return port;
    } catch {
      // not written yet
    }
    await sleep(50);
  }
  throw new Error('Chromium did not report a DevTools port');
}

async function openTab(devtools, url) {
  const response = await fetch(`${devtools}/json/new?${encodeURIComponent(url)}`, { method: 'PUT' });
  if (!response.ok) throw new Error(`opening a tab failed: ${response.status}`);
  return (await response.json()).id;
}

async function closeTab(devtools, id) {
  const response = await fetch(`${devtools}/json/close/${id}`);
  if (!response.ok) throw new Error(`closing a tab failed: ${response.status}`);
}

const results = [];
function check(name, ok, detail = '') {
  results.push({ name, ok: Boolean(ok), detail });
}

// Asks a tab for its election state and returns the reported event.
async function stateOf(tab) {
  const from = events.length;
  command(tab, { do: 'state' });
  return waitFor((e) => e.tab === tab && e.e === 'state', from);
}

// Asks again until `ready` holds or the wait ends. A lock request reaches the browser's lock
// manager asynchronously, so `navigator.locks.query()` may not list it yet right after start.
async function settledStateOf(tab, ready) {
  const deadline = Date.now() + WAIT_MS;
  let state = await stateOf(tab);
  while (state && !ready(state) && Date.now() < deadline) {
    await sleep(20);
    state = await stateOf(tab);
  }
  return state;
}

async function scenario(devtools, base) {
  const tabUrl = (tab) => `${base}/leader/browser/tab.html?tab=${tab}&db=${DB}&lock=${LOCK}`;

  // One leader for two tabs.
  const a = await openTab(devtools, tabUrl('a'));
  const aLead = await waitFor((e) => e.tab === 'a' && e.e === 'drained');
  check('ac_59_first_tab_leads', aLead, aLead ? '' : 'tab a never drained as leader');
  const b = await openTab(devtools, tabUrl('b'));
  check('ac_59_second_tab_starts', await waitFor((e) => e.tab === 'b' && e.e === 'started'));
  const sa = await settledStateOf('a', (state) => state.waiting === 1);
  const sb = await stateOf('b');
  check('ac_59_exactly_one_leader', sa?.isLeader === true && sb?.isLeader === false,
    `a ${sa?.isLeader} b ${sb?.isLeader}`);
  check('ac_59_one_lock_held_one_waiting', sa?.held === 1 && sa?.waiting === 1,
    `held ${sa?.held} waiting ${sa?.waiting}`);
  check('ac_59_follower_knows_leader', sb?.leaderId === aLead?.tabId,
    `${sb?.leaderId} vs ${aLead?.tabId}`);

  // Messages: follower to leader and leader to follower.
  let from = events.length;
  command('b', { do: 'post', body: { n: 1 } });
  const up = await waitFor((e) => e.tab === 'a' && e.e === 'message' && e.kind === 'up', from);
  check('ac_59_post_reaches_leader', up?.body?.n === 1);
  from = events.length;
  command('a', { do: 'broadcast', body: { n: 2 } });
  const down = await waitFor((e) => e.tab === 'b' && e.e === 'message' && e.kind === 'down', from);
  check('ac_59_broadcast_reaches_follower', down?.body?.n === 2);

  // Both tabs write ops while a leads; a drained only on taking over, so they stay pending.
  from = events.length;
  command('a', { do: 'append', count: 3 });
  command('b', { do: 'append', count: 4 });
  const appendedA = await waitFor((e) => e.tab === 'a' && e.e === 'appended', from);
  const appendedB = await waitFor((e) => e.tab === 'b' && e.e === 'appended', from);
  const written = [...(appendedA?.opIds ?? []), ...(appendedB?.opIds ?? [])];
  check('ac_59_both_tabs_append', written.length === 7 && new Set(written).size === 7,
    `${written.length} ops`);

  // The leader reloads: b takes over and drains, a comes back as a follower.
  from = events.length;
  command('a', { do: 'reload' });
  const bLead = await waitFor((e) => e.tab === 'b' && e.e === 'drained', from);
  check('ac_59_takeover_on_reload', bLead, bLead ? '' : 'tab b never took over');
  const drainedByB = bLead?.opIds ?? [];
  check('ac_59_takeover_drains_outbox',
    JSON.stringify([...drainedByB].sort()) === JSON.stringify([...written].sort()),
    `${drainedByB.length} of ${written.length}`);
  check('ac_59_reloaded_tab_starts', await waitFor((e) => e.tab === 'a' && e.e === 'started', from));
  const [ra, rb] = [await stateOf('a'), await stateOf('b')];
  check('ac_59_one_leader_after_reload', ra?.isLeader === false && rb?.isLeader === true,
    `a ${ra?.isLeader} b ${rb?.isLeader}`);

  // A follower writes, then the leader tab closes: a takes over and drains the rest.
  from = events.length;
  command('a', { do: 'append', count: 5 });
  const appendedLate = await waitFor((e) => e.tab === 'a' && e.e === 'appended', from);
  const late = appendedLate?.opIds ?? [];
  from = events.length;
  await closeTab(devtools, b);
  const aAgain = await waitFor((e) => e.tab === 'a' && e.e === 'drained', from);
  check('ac_59_takeover_on_close', aAgain, aAgain ? '' : 'tab a never took over');
  const drainedByA = aAgain?.opIds ?? [];
  check('ac_59_close_drains_outbox',
    late.length === 5 && JSON.stringify([...drainedByA].sort()) === JSON.stringify([...late].sort()),
    `${drainedByA.length} of ${late.length}`);
  const last = await stateOf('a');
  check('ac_59_survivor_holds_lock', last?.isLeader === true && last?.held === 1 && last?.waiting === 0,
    `leader ${last?.isLeader} held ${last?.held} waiting ${last?.waiting}`);

  // Every op was drained exactly once over all leaders.
  const drained = events.filter((e) => e.e === 'drained').flatMap((e) => e.opIds);
  check('ac_59_each_op_drained_once',
    drained.length === written.length + late.length && new Set(drained).size === drained.length,
    `${drained.length} drained`);
  const errors = events.filter((e) => e.e === 'error');
  check('ac_59_no_tab_errors', errors.length === 0, errors.map((e) => e.detail).join('; '));
  await closeTab(devtools, a);
}

async function main() {
  const chromium = findChromium();
  const server = await startServer();
  const profile = await mkdtemp(join(tmpdir(), 'ostrel-leader-'));
  const browser = spawn(
    chromium,
    [
      '--headless=new',
      '--no-sandbox',
      '--disable-gpu',
      '--remote-debugging-port=0',
      `--user-data-dir=${profile}`,
      'about:blank',
    ],
    { stdio: 'ignore' },
  );
  const base = `http://127.0.0.1:${server.address().port}`;
  let timer;
  const timeout = new Promise((resolve) => {
    timer = setTimeout(() => resolve('timeout'), TIMEOUT_MS);
  });
  try {
    const devtools = `http://127.0.0.1:${await devtoolsPort(profile)}`;
    const outcome = await Promise.race([scenario(devtools, base), timeout]);
    if (outcome === 'timeout') check('finished in time', false);
  } catch (error) {
    check('no unexpected error', false, String(error?.stack ?? error));
  }
  clearTimeout(timer);
  const exited = once(browser, 'exit');
  browser.kill();
  await exited;
  for (const queue of commands.values()) for (const poll of queue.polls) poll({ do: 'none' });
  server.closeAllConnections();
  server.close();
  // Helper processes may still write into the profile for a moment after the browser exits.
  await rm(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });

  for (const { name, ok, detail } of results) {
    console.log(`${ok ? 'ok    ' : 'FAILED'} ${name}${detail ? ` (${detail})` : ''}`);
  }
  const passed = results.length > 0 && results.every((result) => result.ok);
  console.log(passed ? `all ${results.length} checks passed` : 'some checks failed');
  process.exitCode = passed ? 0 : 1;
}

await main();
