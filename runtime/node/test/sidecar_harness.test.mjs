// Tests of the sidecar test harness against a fake host, so that a failure in
// sidecar_failure.test.mjs points at the host and not at the harness.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { LineSplitter, Sidecar } from './sidecar_harness.mjs';

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
  host.send({ jsonrpc: '2.0', id: 1, method: 'split' });
  assert.equal((await host.response(1)).result, 'joined');
  host.send({ jsonrpc: '2.0', id: 3, method: 'echo', params: 'three' });
  host.send({ jsonrpc: '2.0', id: 2, method: 'echo', params: [1, 2] });
  assert.deepEqual((await host.response(2)).result, [1, 2]);
  assert.equal((await host.response(3)).result, 'three');
});

test('harness separates non JSON lines and oversized lines from responses', async (t) => {
  const host = new Sidecar(fakeHost, externDir);
  t.after(() => host.kill());
  host.send({ jsonrpc: '2.0', id: 1, method: 'garbage' });
  host.send({ jsonrpc: '2.0', id: 2, method: 'oversized', params: { bytes: 1024 * 1024 + 1 } });
  host.send({ jsonrpc: '2.0', id: 3, method: 'echo', params: 'after' });
  assert.equal((await host.response(3)).result, 'after');
  assert.deepEqual(host.badLines, ['this is not json']);
  assert.deepEqual(host.oversized, [1024 * 1024 + 3]);
});

test('harness rejects a wait when the host exits', async (t) => {
  const host = new Sidecar(fakeHost, externDir);
  t.after(() => host.kill());
  host.send({ jsonrpc: '2.0', id: 1, method: 'exit', params: { code: 7 } });
  await assert.rejects(host.response(1), /host exited/);
  assert.equal(host.exit.code, 7);
});

test('harness rejects a wait on timeout', async (t) => {
  const host = new Sidecar(fakeHost, externDir, { responseTimeoutMs: 100 });
  t.after(() => host.kill());
  host.send({ jsonrpc: '2.0', id: 1, method: 'silent' });
  await assert.rejects(host.response(1), /timeout after 100 ms/);
});

test('harness starts the host under the permission model with read access to extern only', async (t) => {
  const host = new Sidecar(fakeHost, externDir);
  t.after(() => host.kill());
  const args = host.child.spawnargs;
  assert.ok(args.includes('--permission'));
  assert.ok(args.includes(`--allow-fs-read=${externDir}`));
  assert.ok(!args.some((a) => a.startsWith('--allow-fs-write') || a === '--allow-child-process'));
  host.send({ jsonrpc: '2.0', id: 1, method: 'echo', params: null });
  await host.response(1);
});
