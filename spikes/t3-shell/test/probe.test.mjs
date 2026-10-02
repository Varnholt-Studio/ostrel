// Unit tests of the probe helpers that run without a browser.

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import { chromiumArgs, findChromium, parseActivePort, waitFor, withTimeout } from '../cdp.mjs';
import { cacheProblems, parseArgs } from '../probe.mjs';

const ORIGIN = 'http://127.0.0.1:4000/';
const MANIFEST = { build: '0123456789abcdef', files: ['index.html', 'app.mjs'] };
const NAME = 'ostrel-shell-0123456789abcdef';
const URLS = [`${ORIGIN}index.html`, `${ORIGIN}app.mjs`];

describe('cacheProblems', () => {
  test('accepts exactly the shell files of the build', () => {
    assert.deepEqual(cacheProblems({ [NAME]: URLS }, MANIFEST, ORIGIN), []);
  });

  test('reports an API answer in the cache', () => {
    const problems = cacheProblems({ [NAME]: [...URLS, `${ORIGIN}api/messages`] }, MANIFEST, ORIGIN);
    assert.equal(problems.length, 1);
    assert.match(problems[0], /API answer/);
  });

  test('reports entries outside the manifest', () => {
    const problems = cacheProblems({ [NAME]: [...URLS, `${ORIGIN}index.html?room=7`] }, MANIFEST, ORIGIN);
    assert.match(problems.join(), /non manifest entry/);
  });

  test('reports missing files', () => {
    assert.match(cacheProblems({ [NAME]: [URLS[0]] }, MANIFEST, ORIGIN).join(), /misses .*app\.mjs/);
  });

  test('reports caches of other builds and a missing cache', () => {
    const old = 'ostrel-shell-ffffffffffffffff';
    assert.match(cacheProblems({ [NAME]: URLS, [old]: [] }, MANIFEST, ORIGIN).join(), /expected only/);
    assert.match(cacheProblems({}, MANIFEST, ORIGIN).join(), /expected only/);
  });
});

describe('parseArgs', () => {
  test('reads the shell directory', () => {
    assert.equal(parseArgs(['--shell-dir', '/x']).shellDir, '/x');
    assert.ok(parseArgs([]).shellDir.endsWith('runtime/js/shell'));
  });

  test('rejects unknown arguments', () => {
    assert.throws(() => parseArgs(['--offline']), /unknown argument/);
    assert.throws(() => parseArgs(['--shell-dir']), /unknown argument/);
  });
});

describe('cdp helpers', () => {
  test('parseActivePort accepts the DevTools file format only', () => {
    assert.deepEqual(parseActivePort('9222\n/devtools/browser/abc\n'), {
      port: 9222,
      path: '/devtools/browser/abc',
    });
    assert.equal(parseActivePort(''), null);
    assert.equal(parseActivePort('x\n/devtools/browser/abc'), null);
    assert.equal(parseActivePort('9222\n/elsewhere'), null);
  });

  test('findChromium prefers CHROMIUM and returns null when nothing exists', () => {
    assert.equal(findChromium({ CHROMIUM: '/b' }, (p) => p === '/b'), '/b');
    assert.equal(findChromium({ CHROMIUM: '/b' }, () => false), null);
    assert.equal(findChromium({}, () => false), null);
  });

  test('chromiumArgs isolate the profile and bypass any proxy', () => {
    const args = chromiumArgs('/tmp/p');
    assert.ok(args.includes('--user-data-dir=/tmp/p'));
    assert.ok(args.includes('--no-proxy-server'));
    assert.ok(args.includes('--headless=new'));
  });

  test('waitFor returns the first defined value', async () => {
    let n = 0;
    assert.equal(await waitFor('n', () => (++n >= 3 ? n : undefined), 1000, 1), 3);
  });

  test('waitFor times out with the last error', async () => {
    await assert.rejects(
      waitFor('never', () => {
        throw new Error('boom');
      }, 30, 5),
      /timed out waiting for never: boom/,
    );
  });

  test('withTimeout rejects a promise that never settles', async () => {
    await assert.rejects(withTimeout(new Promise(() => {}), 10, 'hang'), /hang took longer/);
    assert.equal(await withTimeout(Promise.resolve(1), 10, 'fast'), 1);
  });
});
