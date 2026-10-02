import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  CACHE_PREFIX,
  MAX_FILES,
  ManifestError,
  cacheNameFor,
  isShellCache,
  parseManifest,
} from '../manifest.mjs';

const BUILD = '0123456789abcdef';
const valid = () => ({ build: BUILD, entry: 'index.html', files: ['index.html', 'app.js', 'theme/std.css'] });

test('parseManifest accepts a valid manifest and names the cache by the build hash', () => {
  const m = parseManifest({ ...valid(), routes: ['rooms', 'rooms'] });
  assert.equal(m.cacheName, CACHE_PREFIX + BUILD);
  assert.equal(m.cacheName, cacheNameFor(BUILD));
  assert.deepEqual(m.files, ['index.html', 'app.js', 'theme/std.css']);
  assert.deepEqual(m.routes, ['rooms']);
  assert.ok(Object.isFrozen(m) && Object.isFrozen(m.files) && Object.isFrozen(m.routes));
});

test('parseManifest copies its input, so later changes to the input have no effect', () => {
  const raw = valid();
  const m = parseManifest(raw);
  raw.files.push('evil.js');
  assert.equal(m.files.length, 3);
});

test('parseManifest rejects a build that is not 16 to 64 lowercase hex digits', () => {
  for (const build of ['', 'abc', 'ABCDEF0123456789', '0123456789abcdeg', 'a'.repeat(65), 7, null]) {
    assert.throws(() => parseManifest({ ...valid(), build }), ManifestError, String(build));
  }
  assert.doesNotThrow(() => parseManifest({ ...valid(), build: 'a'.repeat(64) }));
});

test('parseManifest rejects paths that leave the scope or carry a query', () => {
  const bad = ['', '/abs.js', '../up.js', 'a/../b.js', './a.js', 'a//b.js', 'a.js?v=1', 'a.js#x',
    'https://evil.example/x.js', 'a b.js', 'a\\b.js', 'x'.repeat(513)];
  for (const path of bad) {
    const m = valid();
    m.files.push(path);
    assert.throws(() => parseManifest(m), ManifestError, JSON.stringify(path));
    assert.throws(() => parseManifest({ ...valid(), routes: [path] }), ManifestError, JSON.stringify(path));
  }
});

test('parseManifest rejects duplicates, a missing entry and bad shapes', () => {
  assert.throws(() => parseManifest({ ...valid(), files: ['index.html', 'index.html'] }), /duplicate/);
  assert.throws(() => parseManifest({ ...valid(), entry: 'other.html' }), /entry/);
  assert.throws(() => parseManifest({ ...valid(), files: [] }), /files/);
  assert.throws(() => parseManifest({ ...valid(), files: 'index.html' }), /files/);
  assert.throws(() => parseManifest({ ...valid(), routes: 'rooms' }), /routes/);
  for (const raw of [null, undefined, 'x', [], 3]) {
    assert.throws(() => parseManifest(raw), ManifestError);
  }
});

test('parseManifest bounds the number of files', () => {
  const files = Array.from({ length: MAX_FILES + 1 }, (_, i) => `f${i}.js`);
  assert.throws(() => parseManifest({ build: BUILD, entry: 'f0.js', files }), /files/);
  assert.doesNotThrow(() => parseManifest({ build: BUILD, entry: 'f0.js', files: files.slice(0, MAX_FILES) }));
});

test('isShellCache only claims caches with the shell prefix', () => {
  assert.ok(isShellCache(cacheNameFor(BUILD)));
  assert.ok(!isShellCache('app-images'));
  assert.ok(!isShellCache(undefined));
});
