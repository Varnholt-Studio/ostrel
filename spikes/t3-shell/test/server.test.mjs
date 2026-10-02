// Unit tests of the probe server. They need no browser and no shell runtime: a temporary
// directory stands in for runtime/js/shell.

import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, describe, test } from 'node:test';
import {
  BUILDS,
  MAX_TEXT,
  MessageStore,
  SHELL_MODULES,
  SHELL_PREFIX,
  createProbeServer,
  loadStatic,
  manifestFor,
  serviceWorkerSource,
} from '../server.mjs';

let shellDir;

before(() => {
  shellDir = mkdtempSync(join(tmpdir(), 'shell-probe-test-'));
  for (const name of SHELL_MODULES) writeFileSync(join(shellDir, name), `// ${name}\n`);
});

after(() => {
  rmSync(shellDir, { recursive: true, force: true });
});

// Same rules as runtime/js/shell/manifest.mjs, kept here so these tests run before T28 lands.
const BUILD_RE = /^[0-9a-f]{16,64}$/;
const SEGMENT_RE = /^[A-Za-z0-9._~@+-]+$/;

describe('manifest', () => {
  test('lists the fixture, the build module and the shell runtime', () => {
    const { manifest } = manifestFor(loadStatic(shellDir), 'a');
    for (const file of ['index.html', 'app.mjs', 'style.css', 'build.mjs']) {
      assert.ok(manifest.files.includes(file), file);
    }
    for (const name of SHELL_MODULES) assert.ok(manifest.files.includes(SHELL_PREFIX + name));
    assert.equal(manifest.entry, 'index.html');
    assert.ok(!manifest.files.includes('sw.js'), 'the worker script is not part of the shell cache');
  });

  test('satisfies the manifest rules of the shell runtime', () => {
    for (const key of Object.keys(BUILDS)) {
      const { manifest } = manifestFor(loadStatic(shellDir), key);
      assert.match(manifest.build, BUILD_RE);
      assert.equal(new Set(manifest.files).size, manifest.files.length);
      for (const file of manifest.files) {
        for (const segment of file.split('/')) assert.match(segment, SEGMENT_RE);
      }
    }
  });

  test('build hash changes with any shell file and is stable otherwise', () => {
    const files = loadStatic(shellDir);
    const a1 = manifestFor(files, 'a').manifest.build;
    const a2 = manifestFor(files, 'a').manifest.build;
    const b = manifestFor(files, 'b').manifest.build;
    assert.equal(a1, a2);
    assert.notEqual(a1, b);
    const changed = new Map(files);
    changed.set('style.css', Buffer.from('body{}'));
    assert.notEqual(manifestFor(changed, 'a').manifest.build, a1);
  });

  test('broken build lists a file the server does not have', () => {
    const { manifest, contents } = manifestFor(loadStatic(shellDir), 'broken');
    assert.ok(manifest.files.includes('missing.css'));
    assert.ok(!contents.has('missing.css'));
  });

  test('unknown build is rejected', () => {
    assert.throws(() => manifestFor(loadStatic(shellDir), 'zzz'), /unknown build/);
  });

  test('service worker follows the backend contract', () => {
    const { manifest } = manifestFor(loadStatic(shellDir), 'a');
    const source = serviceWorkerSource(manifest);
    assert.match(source, /^import \{ attachShell \} from '\.\/runtime\/shell\/worker\.mjs';\n/);
    assert.ok(source.includes(`attachShell(self, ${JSON.stringify(manifest)});`));
  });
});

describe('message store', () => {
  test('stores a message once per id, in arrival order', () => {
    const store = new MessageStore();
    assert.equal(store.add({ id: 'm1', text: 'one' }).seq, 1);
    assert.equal(store.add({ id: 'm2', text: 'two' }).seq, 2);
    assert.equal(store.add({ id: 'm1', text: 'replayed' }).text, 'one');
    assert.deepEqual(
      store.list().map((m) => m.text),
      ['one', 'two'],
    );
  });

  test('rejects malformed messages', () => {
    const store = new MessageStore();
    for (const bad of [null, [], 'x', { id: 'a b', text: 't' }, { id: 'a', text: '' }, { id: 'a' }]) {
      assert.equal(typeof store.add(bad), 'string', JSON.stringify(bad));
    }
    assert.equal(typeof store.add({ id: 'a', text: 'x'.repeat(MAX_TEXT + 1) }), 'string');
    assert.equal(typeof store.add({ id: 'x'.repeat(65), text: 't' }), 'string');
    assert.equal(store.list().length, 0);
  });
});

describe('http server', () => {
  test('serves shell files, sw.js and the API with the right caching headers', async () => {
    const server = createProbeServer({ shellDir });
    await server.start();
    try {
      const root = await fetch(server.url);
      assert.equal(root.status, 200);
      assert.match(root.headers.get('content-type'), /^text\/html/);
      const mod = await fetch(new URL(`${SHELL_PREFIX}worker.mjs`, server.url));
      assert.equal(mod.status, 200);
      assert.match(mod.headers.get('content-type'), /^text\/javascript/);
      const sw = await fetch(new URL('sw.js', server.url));
      assert.equal(sw.headers.get('cache-control'), 'no-store');
      assert.equal(await sw.text(), serviceWorkerSource(server.manifest));
      const post = await fetch(new URL('api/messages', server.url), {
        method: 'POST',
        body: JSON.stringify({ id: 'm1', text: 'hi' }),
      });
      assert.equal(post.status, 200);
      assert.equal(post.headers.get('cache-control'), 'no-store');
      const list = await fetch(new URL('api/messages', server.url));
      assert.equal(list.headers.get('cache-control'), 'no-store');
      assert.deepEqual(await list.json(), [{ id: 'm1', text: 'hi', seq: 1 }]);
      assert.equal((await fetch(new URL('nope.txt', server.url))).status, 404);
      assert.equal((await fetch(new URL('api/messages', server.url), { method: 'PUT' })).status, 405);
      const bad = await fetch(new URL('api/messages', server.url), { method: 'POST', body: '{' });
      assert.equal(bad.status, 400);
    } finally {
      await server.stop();
    }
  });

  test('stop takes the app offline, start brings it back on the same port with its data', async () => {
    const server = createProbeServer({ shellDir });
    const port = await server.start();
    try {
      server.store.add({ id: 'm1', text: 'kept' });
      await server.stop();
      assert.equal(server.online, false);
      await assert.rejects(fetch(server.url));
      assert.equal(await server.start(), port);
      const list = await (await fetch(new URL('api/messages', server.url))).json();
      assert.deepEqual(
        list.map((m) => m.text),
        ['kept'],
      );
    } finally {
      await server.stop();
    }
  });

  test('switching builds changes sw.js and the build module', async () => {
    const server = createProbeServer({ shellDir });
    await server.start();
    try {
      const swA = await (await fetch(new URL('sw.js', server.url))).text();
      server.setBuild('b');
      assert.equal(server.label, 'B');
      const swB = await (await fetch(new URL('sw.js', server.url))).text();
      assert.notEqual(swA, swB);
      const mod = await (await fetch(new URL('build.mjs', server.url))).text();
      assert.match(mod, /BUILD_LABEL = "B"/);
      server.setBuild('broken');
      assert.equal((await fetch(new URL('missing.css', server.url))).status, 404);
    } finally {
      await server.stop();
    }
  });

  test('rejects request bodies over the size limit', async () => {
    const server = createProbeServer({ shellDir });
    await server.start();
    try {
      const res = await fetch(new URL('api/messages', server.url), {
        method: 'POST',
        body: 'x'.repeat(10000),
      }).catch(() => null);
      // The server either answers 400/500 or drops the connection; it never stores the body.
      if (res) assert.ok(res.status >= 400);
      assert.equal(server.store.list().length, 0);
    } finally {
      await server.stop();
    }
  });
});
