import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseManifest } from '../manifest.mjs';
import { InstallError, SKIP_WAITING, activate, attachShell, handleFetch, install, shellUrl } from '../worker.mjs';
import { FakeCacheStorage, fakeNetwork, fakeWorkerScope, request } from './fakes.mjs';

const SCOPE = 'https://app.example/chat/';
const OLD = '1111111111111111';
const NEW = '2222222222222222';

function manifestFor(build) {
  return parseManifest({
    build,
    entry: 'index.html',
    files: ['index.html', 'app.js', 'theme/std.css', 'icon.svg'],
    routes: ['rooms'],
  });
}

function setup(build = OLD) {
  const manifest = manifestFor(build);
  const files = new Map(manifest.files.map((f) => [SCOPE + f, `${build}:${f}`]));
  files.set(SCOPE + 'api/rooms', '{"rows":[]}');
  const net = fakeNetwork(files);
  const env = { caches: new FakeCacheStorage(), fetch: net.fetch, scope: SCOPE };
  return { manifest, net, env, files };
}

test('install caches every manifest file under the cache of the build', async () => {
  const { manifest, env } = setup();
  assert.equal(await install(env, manifest), manifest.files.length);
  const cache = await env.caches.open(manifest.cacheName);
  assert.deepEqual((await cache.keys()).sort(), manifest.files.map((f) => SCOPE + f).sort());
  assert.equal(await (await cache.match(SCOPE + 'app.js')).text(), `${OLD}:app.js`);
});

test('install of an already complete build fetches nothing', async () => {
  const { manifest, env, net } = setup();
  await install(env, manifest);
  net.calls.length = 0;
  assert.equal(await install(env, manifest), 0);
  assert.deepEqual(net.calls, []);
});

test('a failed install leaves no new cache and keeps the previous shell', async () => {
  const { manifest: old, env, files } = setup(OLD);
  await install(env, old);
  const next = manifestFor(NEW);
  for (const f of next.files) files.set(SCOPE + f, `${NEW}:${f}`);
  files.delete(SCOPE + 'theme/std.css');
  await assert.rejects(install(env, next), InstallError);
  assert.deepEqual(await env.caches.keys(), [old.cacheName]);
  assert.deepEqual(await activate(env, next), [], 'activate must not delete the old shell');
  assert.deepEqual(await env.caches.keys(), [old.cacheName]);
});

test('install rejects redirects, opaque answers and network errors', async () => {
  for (const make of [
    () => Object.defineProperty(new Response('x'), 'redirected', { value: true }),
    () => Object.defineProperty(new Response('x'), 'type', { value: 'opaque' }),
    () => new Response('x', { status: 206 }),
    () => {
      throw new TypeError('Failed to fetch');
    },
  ]) {
    const { manifest, env, net } = setup();
    net.overrides.set(SCOPE + 'app.js', make);
    await assert.rejects(install(env, manifest), InstallError);
    assert.deepEqual(await env.caches.keys(), []);
  }
});

test('install deletes the new cache when storing fails', async () => {
  const { manifest, env } = setup();
  env.caches.failPut = true;
  await assert.rejects(install(env, manifest), /quota/);
  assert.deepEqual(await env.caches.keys(), []);
});

test('activate deletes old shell caches and keeps caches of others', async () => {
  const { env, files } = setup(OLD);
  await install(env, manifestFor(OLD));
  await env.caches.open('app-images');
  const next = manifestFor(NEW);
  for (const f of next.files) files.set(SCOPE + f, `${NEW}:${f}`);
  await install(env, next);
  assert.deepEqual(await activate(env, next), [manifestFor(OLD).cacheName]);
  assert.deepEqual((await env.caches.keys()).sort(), ['app-images', next.cacheName].sort());
});

test('fetch serves manifest files from the cache, also offline', async () => {
  const { manifest, env, net } = setup();
  await install(env, manifest);
  net.online = false;
  net.calls.length = 0;
  const res = await handleFetch(env, manifest, request(SCOPE + 'app.js'));
  assert.equal(await res.text(), `${OLD}:app.js`);
  assert.deepEqual(net.calls, []);
});

test('navigations to the scope root, the entry and declared routes get the entry document', async () => {
  const { manifest, env, net } = setup();
  await install(env, manifest);
  net.online = false;
  for (const url of [SCOPE, SCOPE + '?room=7', SCOPE + 'index.html', SCOPE + 'rooms']) {
    const res = await handleFetch(env, manifest, request(url, { mode: 'navigate' }));
    assert.equal(await res.text(), `${OLD}:index.html`, url);
  }
});

test('fetch leaves data, other methods, other origins and unknown paths to the network', async () => {
  const { manifest, env } = setup();
  await install(env, manifest);
  const untouched = [
    request(SCOPE + 'api/rooms'),
    request(SCOPE + 'api/rooms', { mode: 'navigate' }),
    request(SCOPE + 'app.js?token=abc'),
    request(SCOPE + 'app.js', { method: 'POST' }),
    request(SCOPE + 'app.js', { method: 'HEAD' }),
    request('https://other.example/chat/app.js'),
    request('https://app.example/other/app.js'),
    request('https://app.example/chat'),
    request(SCOPE),
    request('not a url'),
  ];
  for (const req of untouched) assert.equal(handleFetch(env, manifest, req), null, JSON.stringify(req));
});

test('a cache miss falls back to the network without storing the answer', async () => {
  const { manifest, env, net } = setup();
  await install(env, manifest);
  const cache = await env.caches.open(manifest.cacheName);
  cache.entries.delete(SCOPE + 'icon.svg');
  const res = await handleFetch(env, manifest, request(SCOPE + 'icon.svg'));
  assert.equal(await res.text(), `${OLD}:icon.svg`);
  assert.equal(await cache.match(SCOPE + 'icon.svg'), undefined);
});

test('after use, Cache Storage holds exactly the manifest files (AC-62 cache check)', async () => {
  const { manifest, env } = setup();
  await install(env, manifest);
  for (const req of [request(SCOPE + 'api/rooms'), request(SCOPE, { mode: 'navigate' }), request(SCOPE + 'app.js')]) {
    const res = handleFetch(env, manifest, req);
    if (res) await res;
  }
  const all = [];
  for (const name of await env.caches.keys()) all.push(...(await (await env.caches.open(name)).keys()));
  assert.deepEqual(all.sort(), manifest.files.map((f) => shellUrl(env, f)).sort());
});

test('attachShell wires install, activate, fetch and the skip waiting message', async () => {
  const { manifest, env, net } = setup();
  const self = fakeWorkerScope({ caches: env.caches, fetch: net.fetch, scope: SCOPE });
  const parsed = attachShell(self, { build: OLD, entry: 'index.html', files: [...manifest.files], routes: ['rooms'] });
  assert.equal(parsed.cacheName, manifest.cacheName);
  await self.dispatch('install');
  await self.dispatch('activate');
  assert.deepEqual(await env.caches.keys(), [manifest.cacheName]);
  net.online = false;
  const res = await self.dispatch('fetch', { request: request(SCOPE + 'rooms', { mode: 'navigate' }) });
  assert.equal(await res.text(), `${OLD}:index.html`);
  assert.equal(await self.dispatch('fetch', { request: request(SCOPE + 'api/rooms') }), null);
  await self.dispatch('message', { data: { type: SKIP_WAITING }, source: { type: 'worker' } });
  await self.dispatch('message', { data: { type: 'other' }, source: { type: 'window' } });
  await self.dispatch('message', { data: null, source: { type: 'window' } });
  assert.equal(self.skipped, 0);
  await self.dispatch('message', { data: { type: SKIP_WAITING }, source: { type: 'window' } });
  assert.equal(self.skipped, 1);
});

test('attachShell refuses an invalid manifest', () => {
  const { env, net } = setup();
  const self = fakeWorkerScope({ caches: env.caches, fetch: net.fetch, scope: SCOPE });
  assert.throws(() => attachShell(self, { build: 'nothex', entry: 'index.html', files: ['index.html'] }));
});
