// Failure tests for the Node sidecar host (ARCHITECTURE 7.2, D60, SPEC AC-39, AC-38).
//
// These tests cover what the host process itself must guarantee so that the server side
// supervisor (crates/ostrel_server) can contain every failure: one well formed JSON-RPC line per
// answer, the error codes and kinds of 7.2 instead of a crash for bad input or bad extern results,
// no changed values, a slot that `cancel` frees, and a visible exit instead of a silent hang where
// the process state is lost. Timeouts, restarts with backoff and the open call limit of the
// supervisor are tested in crates/ostrel_server.
//
// Every message is built by sidecar_harness.mjs; the `ready` notification is kept apart from
// answers there. Until runtime/node/host.mjs exists (T30) every test here is reported as TODO:
// the tests run and fail, but do not fail the gate. Once the host is present they are ordinary
// tests.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import {
  Codes,
  MAX_MESSAGE_BYTES,
  MAX_OPEN_CALLS,
  PROTOCOL_VERSION,
  Sidecar,
  callRequest,
  moduleArgs,
  notification,
  request,
} from './sidecar_harness.mjs';

const hostPath = fileURLToPath(new URL('../host.mjs', import.meta.url));
const externDir = fileURLToPath(new URL('./fixtures/extern', import.meta.url));
const failingPath = fileURLToPath(new URL('./fixtures/extern/failing.mjs', import.meta.url));
const brokenPath = fileURLToPath(new URL('./fixtures/extern/broken.mjs', import.meta.url));
const MOD = 'failing';
const hostMissing = existsSync(hostPath) ? false : 'runtime/node/host.mjs does not exist yet (T30)';

function startHost(hostArgs = moduleArgs({ [MOD]: failingPath })) {
  return new Sidecar(hostPath, externDir, hostArgs);
}

function hostTest(name, fn) {
  test(name, { todo: hostMissing }, async (t) => {
    const host = startHost();
    t.after(() => host.kill());
    await host.ready();
    await fn(host);
  });
}

// A host started with `hostArgs` must refuse to start: exit code 2, reason on stderr, nothing on
// stdout.
function startFailureTest(name, hostArgs) {
  test(name, { todo: hostMissing }, async (t) => {
    const host = startHost(hostArgs);
    t.after(() => host.kill());
    const exit = await withTimeout(host.exited, 5000);
    assert.deepEqual(exit, { code: 2, signal: null });
    assert.deepEqual(host.lines, [], 'nothing, not even ready, may reach stdout');
    assert.deepEqual(host.badLines, []);
    assert.notEqual(host.stderr.trim(), '', 'the reason must be on stderr');
  });
}

function withTimeout(promise, ms) {
  let timer;
  const timeout = new Promise((resolve) => {
    timer = setTimeout(() => resolve('still running'), ms);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

function assertCode(response, code) {
  assert.ok(response.error, `expected an error, got ${JSON.stringify(response)}`);
  assert.equal(Object.hasOwn(response, 'result'), false);
  assert.equal(response.error.code, code, JSON.stringify(response.error));
  assert.equal(typeof response.error.message, 'string');
  assert.equal(Object.hasOwn(response.error, 'data'), false, 'data only for code -32000');
}

function assertKind(response, kind) {
  assert.ok(response.error, `expected an error, got ${JSON.stringify(response)}`);
  assert.equal(Object.hasOwn(response, 'result'), false);
  assert.equal(response.error.code, Codes.EXTERN_ERROR, JSON.stringify(response.error));
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
  const ids = host.answers.map((m) => m.id).filter((id) => id !== null);
  assert.equal(new Set(ids).size, ids.length, `an id was answered twice: ${JSON.stringify(ids)}`);
}

// Start and ready.

hostTest('ready announces protocol 1 and the sorted function exports', async (host) => {
  const ready = await host.ready();
  assert.equal(Object.hasOwn(ready, 'id'), false, 'ready is a notification');
  assert.equal(ready.params.protocol, PROTOCOL_VERSION);
  const fns = ready.params.modules[MOD];
  assert.deepEqual(Object.keys(ready.params.modules), [MOD]);
  assert.deepEqual(fns, [...fns].sort());
  for (const fn of ['echo', 'throwsError', 'dagResult']) assert.ok(fns.includes(fn), fn);
  assert.equal(fns.includes('notAFunction'), false, 'only function exports are listed');
  assert.equal(host.notifications.length, 1);
  assert.deepEqual(host.answers, []);
});

hostTest('ping answers pong', async (host) => {
  assert.deepEqual(await host.ping(1), { jsonrpc: '2.0', id: 1, result: 'pong' });
});

startFailureTest(
  'a module that fails to load ends the host with code 2 before ready',
  moduleArgs({ [MOD]: failingPath, broken: brokenPath }),
);
startFailureTest(
  'a missing module file ends the host with code 2',
  moduleArgs({ gone: `${externDir}/gone.mjs` }),
);
startFailureTest('a relative module path ends the host with code 2', [
  '--module',
  `${MOD}=fixtures/extern/failing.mjs`,
]);
startFailureTest('an invalid module name ends the host with code 2', ['--module', `1bad=${failingPath}`]);
startFailureTest('a module named twice ends the host with code 2', [
  '--module',
  `${MOD}=${failingPath}`,
  '--module',
  `${MOD}=${failingPath}`,
]);
startFailureTest('an unknown argument ends the host with code 2', [externDir]);

// Requests.

hostTest('call round trips a value', async (host) => {
  const r = await host.call(1, MOD, 'echo', [{ a: [1, 'two', null, true] }]);
  assert.deepEqual(r, { jsonrpc: '2.0', id: 1, result: { a: [1, 'two', null, true] } });
});

hostTest('string ids are echoed unchanged', async (host) => {
  const r = await host.call('req-7', MOD, 'echo', [7]);
  assert.equal(r.id, 'req-7');
  assert.equal(r.result, 7);
});

hostTest('malformed JSON gets a parse error with id null and the host keeps serving', async (host) => {
  host.sendRaw('{"jsonrpc":"2.0","id":1,\n');
  const r = await host.waitFor(
    () => host.answers.find((m) => m.error?.code === Codes.PARSE_ERROR),
    'parse error',
  );
  assert.equal(r.id, null);
  assertCode(r, Codes.PARSE_ERROR);
  await assertStillServes(host, 2);
  assertCleanStdout(host);
});

hostTest('an invalid request without a valid id gets -32600 with id null', async (host) => {
  host.sendRaw('42\n');
  host.sendRaw('"text"\n');
  host.sendRaw('[1]\n');
  host.send(request(1.5, 'ping'));
  host.send(request(true, 'ping'));
  host.send(request('\ud800', 'ping'));
  const invalid = () => host.answers.filter((m) => m.error?.code === Codes.INVALID_REQUEST);
  await host.waitFor(() => (invalid().length >= 6 ? true : undefined), 'six invalid request errors');
  for (const r of invalid()) {
    assert.equal(r.id, null);
    assertCode(r, Codes.INVALID_REQUEST);
  }
  await assertStillServes(host, 2);
  assertCleanStdout(host);
});

hostTest('an invalid request with a valid id gets -32600 echoing that id', async (host) => {
  // ARCHITECTURE 7.2: "the id if the line is an object with a valid id, else null" (D60).
  const { jsonrpc: _omitted, ...noVersion } = callRequest(5, MOD, 'echo', [1]);
  host.send(noVersion);
  host.send({ ...request(6, 'ping'), jsonrpc: '1.0' });
  host.send({ jsonrpc: '2.0', id: 7 });
  host.send({ ...request('eight', 'ping'), method: 8 });
  for (const id of [5, 6, 7, 'eight']) assertCode(await host.response(id), Codes.INVALID_REQUEST);
  await assertStillServes(host, 9);
  assertCleanStdout(host);
});

hostTest('notifications are never answered', async (host) => {
  host.send(notification('ping'));
  host.send(notification('call', { module: MOD, fn: 'echo', args: [1] }));
  host.send(notification('cancel', { id: 999 }));
  host.send(notification('cancel', 'not an object'));
  host.send(notification('unknown'));
  await assertStillServes(host, 1);
  assert.deepEqual(
    host.answers.map((m) => m.id),
    [1],
  );
  assertCleanStdout(host);
});

hostTest('unknown method gets -32601', async (host) => {
  host.send(request(1, 'eval', { code: '1' }));
  assertCode(await host.response(1), Codes.METHOD_NOT_FOUND);
  await assertStillServes(host, 2);
});

hostTest('bad params get -32602', async (host) => {
  host.send(request(1, 'call', 'not an object'));
  host.send(request(2, 'call'));
  host.send(request(3, 'call', { module: MOD, fn: 'echo' }));
  host.send(request(4, 'call', { module: MOD, fn: 'echo', args: 'x' }));
  host.send(request(5, 'call', { module: MOD, export: 'echo', args: [] }));
  for (const id of [1, 2, 3, 4, 5]) assertCode(await host.response(id), Codes.INVALID_PARAMS);
  await assertStillServes(host, 6);
});

hostTest('unknown module and unknown function get -32602', async (host) => {
  assertCode(await host.call(1, 'missing', 'f', []), Codes.INVALID_PARAMS);
  assertCode(await host.call(2, MOD, 'noSuchExport', []), Codes.INVALID_PARAMS);
  assertCode(await host.call(3, MOD, 'constructor', []), Codes.INVALID_PARAMS);
  assertCode(await host.call(4, MOD, 'toString', []), Codes.INVALID_PARAMS);
  assertCode(await host.call(5, MOD, 'notAFunction', []), Codes.INVALID_PARAMS);
  await assertStillServes(host, 6);
});

hostTest('a call names a module only by its name, never by a path', async (host) => {
  const names = [
    'failing.mjs',
    failingPath,
    '../fake_host.mjs',
    '../../host.mjs',
    '/etc/passwd',
    'node:child_process',
    'data:text/javascript,export const f = () => 1',
  ];
  for (const [i, module] of names.entries()) {
    assertCode(await host.call(i + 1, module, 'echo', [1]), Codes.INVALID_PARAMS);
  }
  await assertStillServes(host, names.length + 1);
});

hostTest('an oversized request line gets -32600 with id null and the host keeps serving', async (host) => {
  const big = 'z'.repeat(MAX_MESSAGE_BYTES);
  host.send(callRequest(1, MOD, 'echo', [big]));
  const r = await host.waitFor(
    () => host.answers.find((m) => m.error?.code === Codes.INVALID_REQUEST),
    'invalid request error for the oversized line',
  );
  assert.equal(r.id, null, 'the line is dropped unread, so its id is unknown');
  assertCode(r, Codes.INVALID_REQUEST);
  await assertStillServes(host, 2);
  assert.equal(host.answers.find((m) => m.id === 1), undefined);
  assertCleanStdout(host);
});

// Extern call failures.

hostTest('a thrown error or a rejected promise is kind Threw', async (host) => {
  const r = await host.call(1, MOD, 'throwsError', []);
  assertKind(r, 'Threw');
  assert.equal(r.error.message, 'Error: extern failure');
  assertKind(await host.call(2, MOD, 'throwsNonError', []), 'Threw');
  assertKind(await host.call(3, MOD, 'rejects', []), 'Threw');
  for (const m of host.answers) assert.doesNotMatch(m.error.message, /\n\s+at /, 'no stack on the pipe');
  await assertStillServes(host, 4);
});

hostTest('a function that returns undefined is kind Undefined', async (host) => {
  assertKind(await host.call(1, MOD, 'returnsUndefined', []), 'Undefined');
  await assertStillServes(host, 2);
});

hostTest('results without a plain JSON form are kind Type, never changed values', async (host) => {
  const fns = [
    'nanResult',
    'infinityResult',
    'bigintResult',
    'cyclicResult',
    'functionResult',
    'sparseResult',
    'dateResult',
    'loneSurrogateResult',
  ];
  for (const [i, fn] of fns.entries()) {
    const r = await host.call(i + 1, MOD, fn, []);
    assert.equal(Object.hasOwn(r, 'result'), false, `${fn} must not produce a result: ${JSON.stringify(r)}`);
    assertKind(r, 'Type');
  }
  await assertStillServes(host, fns.length + 1);
  assertCleanStdout(host);
});

hostTest('negative zero arrives as 0', async (host) => {
  const r = await host.call(1, MOD, 'negativeZero', []);
  assert.equal(Object.is(r.result, 0), true);
});

hostTest('an oversized result is kind TooLarge, never an oversized line', async (host) => {
  assertKind(await host.call(1, MOD, 'hugeResult', []), 'TooLarge');
  await assertStillServes(host, 2);
  assertCleanStdout(host);
});

hostTest('a result with shared references is TooLarge quickly and ping is still answered', async (host) => {
  const started = Date.now();
  assertKind(await host.call(1, MOD, 'dagResult', [26]), 'TooLarge');
  const elapsed = Date.now() - started;
  assert.ok(elapsed < 2000, `the check took ${elapsed} ms`);
  assert.equal((await host.ping(2, 1000)).result, 'pong');
  await assertStillServes(host, 3);
  assertCleanStdout(host);
});

hostTest('a small result with shared references passes unchanged', async (host) => {
  const r = await host.call(1, MOD, 'dagResult', [2]);
  const leaf = { a: 0, b: 0 };
  assert.deepEqual(r.result, { a: leaf, b: leaf });
});

// Concurrency and cancel.

hostTest('a call that never settles does not block other calls', async (host) => {
  host.send(callRequest(1, MOD, 'neverSettles', []));
  await assertStillServes(host, 2);
  assert.equal(host.answers.find((m) => m.id === 1), undefined);
});

hostTest('calls run concurrently and answers come in completion order', async (host) => {
  host.send(callRequest(1, MOD, 'settlesAfter', [300, 'slow']));
  host.send(callRequest(2, MOD, 'settlesAfter', [10, 'fast']));
  assert.equal((await host.response(1)).result, 'slow');
  assert.deepEqual(
    host.answers.map((m) => m.id),
    [2, 1],
  );
});

hostTest('the 65th open call is kind Busy', async (host) => {
  for (let id = 1; id <= MAX_OPEN_CALLS; id++) host.send(callRequest(id, MOD, 'neverSettles', []));
  assertKind(await host.call(MAX_OPEN_CALLS + 1, MOD, 'echo', [1]), 'Busy');
  assert.equal((await host.ping(MAX_OPEN_CALLS + 2)).result, 'pong');
  assert.deepEqual(
    host.answers.map((m) => m.id),
    [MAX_OPEN_CALLS + 1, MAX_OPEN_CALLS + 2],
  );
});

hostTest('cancel frees the slots of calls that never settle', async (host) => {
  for (let id = 1; id <= MAX_OPEN_CALLS; id++) host.send(callRequest(id, MOD, 'neverSettles', []));
  assertKind(await host.call(100, MOD, 'echo', [1]), 'Busy');
  for (let id = 1; id <= MAX_OPEN_CALLS; id++) host.cancel(id);
  // 64 calls fit again, so none of them may be Busy.
  for (let id = 101; id <= 100 + MAX_OPEN_CALLS; id++) host.send(callRequest(id, MOD, 'echo', [id]));
  for (let id = 101; id <= 100 + MAX_OPEN_CALLS; id++) {
    assert.equal((await host.response(id)).result, id);
  }
  for (let id = 1; id <= MAX_OPEN_CALLS; id++) {
    assert.equal(host.answers.find((m) => m.id === id), undefined, `cancelled call ${id} was answered`);
  }
  assertCleanStdout(host);
});

hostTest('a cancelled call is never answered, also when it settles later', async (host) => {
  host.send(callRequest(1, MOD, 'settlesAfter', [100, 'late']));
  host.cancel(1);
  assert.equal(await host.aliveAfter(300), true);
  assert.equal(host.answers.find((m) => m.id === 1), undefined);
  // The id is free again after cancel.
  assert.equal((await host.call(1, MOD, 'echo', ['again'])).result, 'again');
  assertCleanStdout(host);
});

// Isolation.

hostTest('console output of an extern module never reaches the protocol stream', async (host) => {
  const r = await host.call(1, MOD, 'logsToStdout', []);
  assert.equal(r.result, 'logged');
  await assertStillServes(host, 2);
  assertCleanStdout(host);
  await host.waitFor(() => (host.stderr.includes('stray output') ? true : undefined), 'log on stderr');
});

hostTest('reading outside the extern directory is denied by the permission model', async (host) => {
  const r = await host.call(1, MOD, 'readsOutsideExternDir', []);
  assertKind(r, 'Threw');
  assert.match(r.error.message, /allow-fs-read|ERR_ACCESS_DENIED/);
  await assertStillServes(host, 2);
});

hostTest('the host adds nothing to the empty environment', async (host) => {
  const r = await host.call(1, MOD, 'envKeys', []);
  assert.deepEqual(r.result, []);
});

// Exits and hangs.

hostTest('process exit inside a call ends the host visibly', async (host) => {
  await assertStillServes(host, 0);
  host.send(callRequest(1, MOD, 'exitsProcess', []));
  const exit = await host.exited;
  assert.notEqual(exit.code, 0);
  assertCleanStdout(host);
});

hostTest('an uncaught exception after a call ends the host with a non zero code', async (host) => {
  const r = await host.call(1, MOD, 'uncaughtLater', []);
  assert.equal(r.result, 'returned');
  const exit = await withTimeout(host.exited, 3000);
  assert.notEqual(exit, 'still running', 'the host must not keep running in an unknown state');
  assert.notEqual(exit.code, 0);
  assertCleanStdout(host);
});

hostTest('a blocked event loop answers nothing, not even ping, until it is killed', async (host) => {
  host.send(callRequest(1, MOD, 'busyLoop', []));
  host.send(callRequest(2, MOD, 'echo', [1]));
  host.send(request(3, 'ping'));
  assert.equal(await host.aliveAfter(500), true);
  assert.deepEqual(host.answers, []);
  const exit = await host.kill();
  assert.equal(exit.signal, 'SIGKILL');
});

hostTest('end of input ends the host with code 0, also with calls running', async (host) => {
  await assertStillServes(host, 1);
  host.send(callRequest(2, MOD, 'neverSettles', []));
  host.closeInput();
  const exit = await withTimeout(host.exited, 3000);
  assert.deepEqual(exit, { code: 0, signal: null });
});
