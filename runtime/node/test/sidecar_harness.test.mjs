// Tests of the sidecar test harness against a fake host, so that a failure in
// sidecar_failure.test.mjs points at the host and not at the harness.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import {
  LineSplitter,
  Sidecar,
  callRequest,
  cancelNotification,
  moduleArgs,
  pingRequest,
  request,
} from './sidecar_harness.mjs';

const fakeHost = fileURLToPath(new URL('./fixtures/fake_host.mjs', import.meta.url));
const externDir = fileURLToPath(new URL('./fixtures/extern', import.meta.url));

function collect(maxBytes) {
  const lines = [];
  const oversized = [];
  const splitter = new LineSplitter((l) => lines.push(l), (n) => oversized.push(n), maxBytes);
  return { splitter, lines, oversized };
}

test('splitter joins a line delivered in several chunks', () => {
  const { splitter, lines } = collect(100);
  splitter.push(Buffer.from('ab'));
  splitter.push(Buffer.from('c\nde'));
  splitter.push(Buffer.from('f\n'));
  assert.deepEqual(lines, ['abc', 'def']);
  assert.equal(splitter.pendingBytes(), 0);
});

test('splitter keeps a multi byte character split across chunks intact', () => {
  const { splitter, lines } = collect(100);
  const bytes = Buffer.from('ä\n', 'utf8');
  splitter.push(bytes.subarray(0, 1));
  splitter.push(bytes.subarray(1));
  assert.deepEqual(lines, ['ä']);
});

test('splitter reports a line over the limit by size without keeping it', () => {
  const { splitter, lines, oversized } = collect(4);
  splitter.push(Buffer.from('1234\n12345\nok\n'));
  assert.deepEqual(lines, ['1234', 'ok']);
  assert.deepEqual(oversized, [5]);
});

test('splitter reports bytes without a final newline', () => {
  const { splitter } = collect(100);
  splitter.push(Buffer.from('partial'));
  assert.equal(splitter.pendingBytes(), 7);
});

test('harness matches responses by id and reassembles split output', async (t) => {
  const host = new Sidecar(fakeHost, externDir);
  t.after(() => host.kill());
  host.send(request(1, 'split'));
  assert.equal((await host.response(1)).result, 'joined');
  host.send(request(3, 'echo', 'three'));
  host.send(request(2, 'echo', [1, 2]));
  assert.deepEqual((await host.response(2)).result, [1, 2]);
  assert.equal((await host.response(3)).result, 'three');
});

test('harness separates non JSON lines and oversized lines from responses', async (t) => {
  const host = new Sidecar(fakeHost, externDir);
  t.after(() => host.kill());
  host.send(request(1, 'garbage'));
  host.send(request(2, 'oversized', { bytes: 1024 * 1024 + 1 }));
  host.send(request(3, 'echo', 'after'));
  assert.equal((await host.response(3)).result, 'after');
  assert.deepEqual(host.badLines, ['this is not json']);
  assert.deepEqual(host.oversized, [1024 * 1024 + 3]);
});

test('harness rejects a wait when the host exits', async (t) => {
  const host = new Sidecar(fakeHost, externDir);
  t.after(() => host.kill());
  host.send(request(1, 'exit', { code: 7 }));
  await assert.rejects(host.response(1), /host exited/);
  assert.equal(host.exit.code, 7);
});

test('harness rejects a wait on timeout', async (t) => {
  const host = new Sidecar(fakeHost, externDir, [], { responseTimeoutMs: 100 });
  t.after(() => host.kill());
  host.send(request(1, 'silent'));
  await assert.rejects(host.response(1), /timeout after 100 ms/);
});

test('harness starts the host under the permission model with read access to extern only', async (t) => {
  const host = new Sidecar(fakeHost, externDir);
  t.after(() => host.kill());
  const args = host.child.spawnargs;
  assert.ok(args.includes('--permission'));
  assert.ok(args.includes(`--allow-fs-read=${externDir}`));
  assert.ok(!args.some((a) => a.startsWith('--allow-fs-write') || a === '--allow-child-process'));
  host.send(request(1, 'echo', null));
  await host.response(1);
});

test('message builders produce the request shapes of ARCHITECTURE 7.2', () => {
  assert.deepEqual(callRequest(3, 'failing', 'echo', [1]), {
    jsonrpc: '2.0',
    id: 3,
    method: 'call',
    params: { module: 'failing', fn: 'echo', args: [1] },
  });
  assert.deepEqual(callRequest('a', 'm', 'f'), {
    jsonrpc: '2.0',
    id: 'a',
    method: 'call',
    params: { module: 'm', fn: 'f', args: [] },
  });
  assert.deepEqual(pingRequest(1), { jsonrpc: '2.0', id: 1, method: 'ping' });
  assert.deepEqual(cancelNotification(4), { jsonrpc: '2.0', method: 'cancel', params: { id: 4 } });
  assert.deepEqual(moduleArgs({ a: '/x/a.mjs', b: '/x/b.mjs' }), [
    '--module',
    'a=/x/a.mjs',
    '--module',
    'b=/x/b.mjs',
  ]);
});

test('harness passes module arguments after the host script', async (t) => {
  const args = moduleArgs({ failing: '/abs/failing.mjs' });
  const host = new Sidecar(fakeHost, externDir, args);
  t.after(() => host.kill());
  const ready = await host.ready();
  assert.deepEqual(ready.params.argv, args);
  const spawned = host.child.spawnargs;
  assert.deepEqual(spawned.slice(spawned.indexOf(fakeHost) + 1), args);
});

test('harness keeps notifications such as ready apart from answers', async (t) => {
  const host = new Sidecar(fakeHost, externDir);
  t.after(() => host.kill());
  await host.ready();
  host.send(request(1, 'notify', 'x'));
  assert.equal((await host.response(1)).result, 'notified');
  assert.deepEqual(
    host.answers.map((m) => m.id),
    [1],
  );
  assert.deepEqual(
    host.notifications.map((m) => m.method),
    ['ready', 'note'],
  );
  assert.equal(host.lines.length, 3);
});
