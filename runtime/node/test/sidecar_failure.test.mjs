// Failure tests for the Node sidecar host (ARCHITECTURE 7.2, SPEC AC-39, AC-38).
//
// These tests cover what the host process itself must guarantee so that the server side
// supervisor (crates/ostrel_server) can contain every failure: one well formed JSON-RPC line per
// response, a typed error instead of a crash for bad input or bad extern results, no changed
// values, and a visible exit instead of a silent hang where the process state is lost.
// Timeouts, restarts with backoff and the in-flight limit belong to the supervisor and are tested
// there.
//
// ASSUMPTION (T48, to be confirmed by T30 and the F3 sidecar RPC schema):
//   * request shape and command line: see sidecar_harness.mjs
//   * protocol errors use the JSON-RPC 2.0 codes -32700, -32600 and -32601
//   * every other error carries `error.data.kind`, one of
//       "NotFound"  module or export not available,
//       "Thrown"    the extern function threw or its promise rejected,
//       "Value"     the result has no canonical JSON form (NaN, Infinity, BigInt, cycles, functions),
//       "TooLarge"  request or response line over 1 MiB
//   * an uncaught exception outside a call ends the host with a non zero exit code
//
// Until runtime/node/host.mjs exists (T30) every test here is reported as TODO: the tests run and
// fail, but do not fail the gate. Once the host is present they are ordinary tests.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { MAX_MESSAGE_BYTES, Sidecar } from './sidecar_harness.mjs';

const hostPath = fileURLToPath(new URL('../host.mjs', import.meta.url));
const externDir = fileURLToPath(new URL('./fixtures/extern', import.meta.url));
const MOD = 'failing.mjs';
const hostMissing = existsSync(hostPath) ? false : 'runtime/node/host.mjs does not exist yet (T30)';

function hostTest(name, fn) {
  test(name, { todo: hostMissing }, async (t) => {
    const host = new Sidecar(hostPath, externDir);
    t.after(() => host.kill());
    await fn(host);
  });
}

function assertKind(response, kind) {
  assert.ok(response.error, `expected an error, got ${JSON.stringify(response)}`);
  assert.equal(response.result, undefined);
  assert.equal(Number.isInteger(response.error.code), true);
  assert.equal(typeof response.error.message, 'string');
  assert.equal(response.error.data?.kind, kind, JSON.stringify(response.error));
}

async function assertStillServes(host, id) {
  const r = await host.call(id, MOD, 'echo', ['still alive']);
  assert.equal(r.result, 'still alive');
}

function assertCleanStdout(host) {
  assert.deepEqual(host.badLines, [], 'every stdout line must be JSON');
  assert.deepEqual(host.oversized, [], 'no stdout line may exceed 1 MiB');
  for (const m of host.lines) assert.equal(m.jsonrpc, '2.0');
}

hostTest('call round trips a value', async (host) => {
  const r = await host.call(1, MOD, 'echo', [{ a: [1, 'two', null, true] }]);
  assert.deepEqual(r, { jsonrpc: '2.0', id: 1, result: { a: [1, 'two', null, true] } });
});

hostTest('string ids are echoed unchanged', async (host) => {
  const r = await host.call('req-7', MOD, 'echo', [7]);
  assert.equal(r.id, 'req-7');
  assert.equal(r.result, 7);
});

hostTest('malformed JSON gets a parse error and the host keeps serving', async (host) => {
  host.sendRaw('{"jsonrpc":"2.0","id":1,\n');
  const r = await host.waitFor(() => host.lines.find((m) => m.error?.code === -32700), 'parse error');
  assert.equal(r.id, null);
  await assertStillServes(host, 2);
  assertCleanStdout(host);
});

hostTest('JSON that is not a request gets an invalid request error', async (host) => {
  host.sendRaw('42\n');
  host.sendRaw('"text"\n');
  host.sendRaw('{"id":5,"method":"call"}\n');
  host.sendRaw('{"jsonrpc":"2.0","id":6}\n');
  host.sendRaw('{"jsonrpc":"2.0","id":7,"method":"call","params":"not an object"}\n');
  const invalid = () => host.lines.filter((m) => m.error?.code === -32600);
  await host.waitFor(() => (invalid().length >= 5 ? true : undefined), 'five invalid request errors');
  const ids = invalid().map((m) => m.id);
  assert.deepEqual(ids.filter((id) => id !== null).sort(), [5, 6, 7]);
  await assertStillServes(host, 8);
  assertCleanStdout(host);
});

hostTest('unknown method gets method not found', async (host) => {
  host.send({ jsonrpc: '2.0', id: 1, method: 'eval', params: { code: '1' } });
  const r = await host.response(1);
  assert.equal(r.error?.code, -32601);
  await assertStillServes(host, 2);
});

hostTest('unknown module and unknown export are typed errors', async (host) => {
  assertKind(await host.call(1, 'missing.mjs', 'f', []), 'NotFound');
  assertKind(await host.call(2, MOD, 'noSuchExport', []), 'NotFound');
  assertKind(await host.call(3, MOD, 'constructor', []), 'NotFound');
  await assertStillServes(host, 4);
});

hostTest('a module outside the extern directory is never loaded', async (host) => {
  for (const [id, module] of [
    [1, '../fake_host.mjs'],
    [2, '../../host.mjs'],
    [3, '/etc/passwd'],
    [4, 'node:child_process'],
    [5, 'data:text/javascript,export const f = () => 1'],
  ]) {
    const r = await host.call(id, module, 'f', []);
    assert.ok(r.error, `module ${module} must be rejected, got ${JSON.stringify(r)}`);
  }
  await assertStillServes(host, 6);
});

hostTest('a thrown error becomes a typed error', async (host) => {
  const r = await host.call(1, MOD, 'throwsError', []);
  assertKind(r, 'Thrown');
  assert.match(JSON.stringify(r.error), /extern failure/);
  assertKind(await host.call(2, MOD, 'throwsNonError', []), 'Thrown');
  assertKind(await host.call(3, MOD, 'rejects', []), 'Thrown');
  await assertStillServes(host, 4);
});

hostTest('a call that never settles does not block other calls', async (host) => {
  host.send({ jsonrpc: '2.0', id: 1, method: 'call', params: { module: MOD, export: 'neverSettles', args: [] } });
  await assertStillServes(host, 2);
  assert.equal(host.lines.find((m) => m.id === 1), undefined);
});

hostTest('an oversized request line is rejected and the host keeps serving', async (host) => {
  const big = 'z'.repeat(MAX_MESSAGE_BYTES);
  host.send({ jsonrpc: '2.0', id: 1, method: 'call', params: { module: MOD, export: 'echo', args: [big] } });
  const r = await host.waitFor(
    () => host.lines.find((m) => m.error?.data?.kind === 'TooLarge'),
    'TooLarge error for the request',
  );
  assert.ok(r.id === 1 || r.id === null);
  await assertStillServes(host, 2);
  assertCleanStdout(host);
});

hostTest('an oversized result becomes a typed error, never an oversized line', async (host) => {
  assertKind(await host.call(1, MOD, 'hugeResult', []), 'TooLarge');
  await assertStillServes(host, 2);
  assertCleanStdout(host);
});

hostTest('results without a canonical JSON form are typed errors, never changed values', async (host) => {
  let id = 0;
  for (const fn of ['nanResult', 'infinityResult', 'bigintResult', 'cyclicResult', 'functionResult']) {
    id += 1;
    const r = await host.call(id, MOD, fn, []);
    assert.ok(!('result' in r), `${fn} must not produce a result, got ${JSON.stringify(r)}`);
    assertKind(r, 'Value');
  }
  await assertStillServes(host, id + 1);
  assertCleanStdout(host);
});

hostTest('console output of an extern module never reaches the protocol stream', async (host) => {
  const r = await host.call(1, MOD, 'logsToStdout', []);
  assert.equal(r.result, 'logged');
  await assertStillServes(host, 2);
  assertCleanStdout(host);
  await host.waitFor(() => (host.stderr.includes('stray output') ? true : undefined), 'log on stderr');
});

hostTest('reading outside the extern directory is denied by the permission model', async (host) => {
  const r = await host.call(1, MOD, 'readsOutsideExternDir', []);
  assertKind(r, 'Thrown');
  assert.match(JSON.stringify(r.error), /ERR_ACCESS_DENIED/);
  await assertStillServes(host, 2);
});

hostTest('the host adds nothing to the empty environment', async (host) => {
  const r = await host.call(1, MOD, 'envKeys', []);
  assert.deepEqual(r.result, []);
});

hostTest('process exit inside a call ends the host visibly', async (host) => {
  await assertStillServes(host, 0);
  host.send({ jsonrpc: '2.0', id: 1, method: 'call', params: { module: MOD, export: 'exitsProcess', args: [] } });
  const exit = await host.exited;
  assert.notEqual(exit.code, 0);
  assertCleanStdout(host);
});

hostTest('an uncaught exception after a call ends the host with a non zero code', async (host) => {
  const r = await host.call(1, MOD, 'uncaughtLater', []);
  assert.equal(r.result, 'returned');
  const exit = await Promise.race([
    host.exited,
    new Promise((resolve) => setTimeout(() => resolve('still running'), 3000)),
  ]);
  assert.notEqual(exit, 'still running', 'the host must not keep running in an unknown state');
  assert.notEqual(exit.code, 0);
  assertCleanStdout(host);
});

hostTest('a blocked event loop is a hang that only the supervisor can end', async (host) => {
  host.send({ jsonrpc: '2.0', id: 1, method: 'call', params: { module: MOD, export: 'busyLoop', args: [] } });
  host.send({ jsonrpc: '2.0', id: 2, method: 'call', params: { module: MOD, export: 'echo', args: [1] } });
  assert.equal(await host.aliveAfter(500), true);
  assert.equal(host.lines.length, 0);
  const exit = await host.kill();
  assert.equal(exit.signal, 'SIGKILL');
});

hostTest('end of input ends the host with code 0', async (host) => {
  await assertStillServes(host, 1);
  host.closeInput();
  const exit = await Promise.race([
    host.exited,
    new Promise((resolve) => setTimeout(() => resolve('still running'), 3000)),
  ]);
  assert.deepEqual(exit, { code: 0, signal: null });
});
