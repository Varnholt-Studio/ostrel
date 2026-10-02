// Tests for the Node sidecar host (ARCHITECTURE 7.2). Each test starts the
// host the way the server does: Node permission model, read access to the
// fixture directory only, empty environment.

import { after, test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import {
  Codes,
  MAX_IN_FLIGHT,
  MAX_MESSAGE_BYTES,
  MAX_NODES,
  PROTOCOL_VERSION,
  checkWireValue,
  lineSplitter,
  parseArgs,
} from "./host.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const fixtures = join(here, "fixtures");
const hostPath = join(here, "host.mjs");

// A failed assertion must not leave a host running, or the test run hangs.
const running = new Set();
after(() => {
  for (const child of running) child.kill("SIGKILL");
});

function startHost(modules = { math: "math.mjs", bad: "bad.mjs" }) {
  const args = ["--permission", `--allow-fs-read=${fixtures}`, hostPath];
  for (const [name, file] of Object.entries(modules)) {
    args.push("--module", `${name}=${join(fixtures, file)}`);
  }
  const child = spawn(process.execPath, args, { env: {}, stdio: ["pipe", "pipe", "pipe"] });
  running.add(child);
  child.on("exit", () => running.delete(child));
  const lines = [];
  const waiters = [];
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    stdout += chunk;
    let nl;
    while ((nl = stdout.indexOf("\n")) !== -1) {
      const line = stdout.slice(0, nl);
      stdout = stdout.slice(nl + 1);
      lines.push(line);
      for (const w of waiters.splice(0)) w();
    }
  });
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => (stderr += chunk));
  const exited = new Promise((resolve) => child.on("exit", (code) => resolve(code)));

  async function nextMessage(timeoutMs = 5000) {
    const deadline = Date.now() + timeoutMs;
    while (lines.length === 0) {
      if (Date.now() > deadline) throw new Error(`no message from host; stderr: ${stderr}`);
      await new Promise((resolve) => {
        waiters.push(resolve);
        setTimeout(resolve, 50);
      });
    }
    const line = lines.shift();
    try {
      return JSON.parse(line);
    } catch {
      throw new Error(`host wrote a line that is not JSON: ${line.slice(0, 200)}`);
    }
  }

  return {
    child,
    exited,
    nextMessage,
    rawLines: lines,
    stderr: () => stderr,
    writeLine: (text) => child.stdin.write(text + "\n"),
    send: (msg) => child.stdin.write(JSON.stringify(msg) + "\n"),
    async call(id, module, fn, args = []) {
      child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method: "call", params: { module, fn, args } }) + "\n");
      return nextMessage();
    },
    async stop() {
      child.stdin.end();
      return exited;
    },
  };
}

async function readyHost(modules) {
  const host = startHost(modules);
  const ready = await host.nextMessage();
  assert.equal(ready.method, "ready");
  return host;
}

test("announces ready with the exported functions of each module", async () => {
  const host = startHost();
  const ready = await host.nextMessage();
  assert.equal(ready.params.protocol, PROTOCOL_VERSION);
  assert.equal(PROTOCOL_VERSION, 1);
  assert.deepEqual(ready.params.modules.math, ["add", "chatty", "record", "slowEcho"]);
  assert.ok(ready.params.modules.bad.includes("throws"));
  assert.equal(Object.hasOwn(ready, "id"), false);
  assert.equal(await host.stop(), 0);
});

test("answers ping and call with the request id", async () => {
  const host = await readyHost();
  host.send({ jsonrpc: "2.0", id: "p1", method: "ping" });
  assert.deepEqual(await host.nextMessage(), { jsonrpc: "2.0", id: "p1", result: "pong" });
  assert.deepEqual(await host.call(7, "math", "add", [2, 3]), { jsonrpc: "2.0", id: 7, result: 5 });
  const rec = await host.call(8, "math", "record", ["a", ["x", "y"]]);
  assert.deepEqual(rec.result, { name: "a", tags: ["x", "y"], nested: { ok: true, none: null } });
  await host.stop();
});

test("runs calls concurrently and answers in completion order", async () => {
  const host = await readyHost();
  host.send({ jsonrpc: "2.0", id: 1, method: "call", params: { module: "math", fn: "slowEcho", args: ["slow", 300] } });
  host.send({ jsonrpc: "2.0", id: 2, method: "call", params: { module: "math", fn: "slowEcho", args: ["fast", 0] } });
  assert.equal((await host.nextMessage()).id, 2);
  assert.equal((await host.nextMessage()).id, 1);
  await host.stop();
});

test("module output never reaches the protocol stream", async () => {
  const host = await readyHost();
  const res = await host.call(1, "math", "chatty", ["hi"]);
  assert.equal(res.result, "hi");
  assert.equal(host.rawLines.length, 0);
  assert.match(host.stderr(), /log line from module/);
  assert.match(host.stderr(), /raw stdout from module/);
  await host.stop();
});

test("thrown errors and rejections become Threw", async () => {
  const host = await readyHost();
  const cases = [
    ["throws", "TypeError: bad input"],
    ["rejects", "RangeError: async failure"],
    ["throwsString", "non Error value thrown"],
  ];
  for (const [i, [fn, message]] of cases.entries()) {
    const res = await host.call(i, "bad", fn);
    assert.equal(res.error.code, Codes.EXTERN_ERROR);
    assert.equal(res.error.data.kind, "Threw");
    assert.equal(res.error.message, message);
  }
  const long = await host.call(9, "bad", "longThrow");
  assert.ok(long.error.message.length < 300);
  await host.stop();
});

test("undefined result becomes Undefined", async () => {
  const host = await readyHost();
  const res = await host.call(1, "bad", "nothing");
  assert.equal(res.error.data.kind, "Undefined");
  await host.stop();
});

test("results that are not plain JSON become Type errors", async () => {
  const host = await readyHost();
  const fns = ["nested", "aFunction", "notFinite", "big", "cyclic", "date", "loneSurrogate", "sparse", "tooDeep"];
  for (const [i, fn] of fns.entries()) {
    const res = await host.call(i, "bad", fn);
    assert.equal(res.error?.data?.kind, "Type", `${fn}: ${JSON.stringify(res).slice(0, 200)}`);
  }
  const ok = await host.call(99, "bad", "shared");
  assert.deepEqual(ok.result, { a: { x: 1 }, b: { x: 1 } });
  await host.stop();
});

test("unknown methods, modules and functions are rejected without lookup on prototypes", async () => {
  const host = await readyHost();
  host.send({ jsonrpc: "2.0", id: 1, method: "eval" });
  assert.equal((await host.nextMessage()).error.code, Codes.METHOD_NOT_FOUND);
  for (const [i, [module, fn]] of [
    ["nope", "add"],
    ["__proto__", "add"],
    ["math", "nope"],
    ["math", "notAFunction"],
    ["math", "constructor"],
    ["math", "toString"],
    ["math", "__proto__"],
  ].entries()) {
    const res = await host.call(10 + i, module, fn);
    assert.equal(res.error.code, Codes.INVALID_PARAMS, `${module}.${fn}`);
  }
  await host.stop();
});

test("malformed lines get an error with id null and the host keeps serving", async () => {
  const host = await readyHost();
  const bad = [
    ["{not json", Codes.PARSE_ERROR],
    ["[1,2]", Codes.INVALID_REQUEST],
    ["42", Codes.INVALID_REQUEST],
    ['{"id":1,"method":"ping"}', Codes.INVALID_REQUEST],
    ['{"jsonrpc":"2.0","id":{"a":1},"method":"ping"}', Codes.INVALID_REQUEST],
    ['{"jsonrpc":"2.0","id":1.5,"method":"ping"}', Codes.INVALID_REQUEST],
  ];
  for (const [line, code] of bad) {
    host.writeLine(line);
    const res = await host.nextMessage();
    assert.equal(res.id, null, line);
    assert.equal(res.error.code, code, line);
  }
  host.writeLine('{"jsonrpc":"2.0","id":2,"method":7}');
  assert.equal((await host.nextMessage()).error.code, Codes.INVALID_REQUEST);
  for (const params of [undefined, null, { module: "math", fn: "add" }, { module: "math", fn: "add", args: {} }]) {
    host.send({ jsonrpc: "2.0", id: 3, method: "call", params });
    assert.equal((await host.nextMessage()).error.code, Codes.INVALID_PARAMS);
  }
  host.writeLine("");
  host.send({ jsonrpc: "2.0", id: 4, method: "ping" });
  assert.equal((await host.nextMessage()).id, 4);
  await host.stop();
});

test("notifications are never answered", async () => {
  const host = await readyHost();
  host.send({ jsonrpc: "2.0", method: "call", params: { module: "math", fn: "add", args: [1, 2] } });
  host.send({ jsonrpc: "2.0", method: "ping" });
  host.send({ jsonrpc: "2.0", id: 5, method: "ping" });
  assert.equal((await host.nextMessage()).id, 5);
  await host.stop();
});

test("an oversized request is dropped and the next request is served", async () => {
  const host = await readyHost();
  const big = JSON.stringify({ jsonrpc: "2.0", id: 1, method: "call", params: { module: "math", fn: "add", args: ["z".repeat(MAX_MESSAGE_BYTES), 1] } });
  host.writeLine(big);
  const res = await host.nextMessage();
  assert.equal(res.id, null);
  assert.equal(res.error.code, Codes.INVALID_REQUEST);
  assert.deepEqual(await host.call(2, "math", "add", [1, 1]), { jsonrpc: "2.0", id: 2, result: 2 });
  await host.stop();
});

test("calls beyond the in-flight limit fail with Busy", async () => {
  const host = await readyHost();
  for (let i = 0; i < MAX_IN_FLIGHT; i++) {
    host.send({ jsonrpc: "2.0", id: i, method: "call", params: { module: "math", fn: "slowEcho", args: [i, 500] } });
  }
  const extra = await host.call("extra", "math", "add", [1, 2]);
  assert.equal(extra.error.data.kind, "Busy");
  const ids = new Set();
  for (let i = 0; i < MAX_IN_FLIGHT; i++) ids.add((await host.nextMessage()).id);
  assert.equal(ids.size, MAX_IN_FLIGHT);
  assert.deepEqual(await host.call("after", "math", "add", [1, 2]), { jsonrpc: "2.0", id: "after", result: 3 });
  await host.stop();
});

function callRequest(id, module, fn, args) {
  return { jsonrpc: "2.0", id, method: "call", params: { module, fn, args } };
}

test("a response over 1 MiB becomes TooLarge", async () => {
  const host = await readyHost();
  const res = await host.call(1, "bad", "tooLarge");
  assert.equal(res.error.code, Codes.EXTERN_ERROR);
  assert.equal(res.error.data.kind, "TooLarge");
  assert.deepEqual(await host.call(2, "math", "add", [1, 1]), { jsonrpc: "2.0", id: 2, result: 2 });
  await host.stop();
});

test("cancel frees the slots of calls that never settle", async () => {
  // Without cancel, 64 promises that never settle would block the host for
  // good while ping still answers (review of T30 at 92fa0f5).
  const host = await readyHost();
  for (let i = 0; i < MAX_IN_FLIGHT; i++) host.send(callRequest(i, "bad", "hang", []));
  assert.equal((await host.call("full", "math", "add", [1, 2])).error.data.kind, "Busy");
  for (let i = 0; i < MAX_IN_FLIGHT; i++) host.send({ jsonrpc: "2.0", method: "cancel", params: { id: i } });
  // Every slot is free again: a full batch of calls runs without Busy.
  for (let i = 0; i < MAX_IN_FLIGHT; i++) host.send(callRequest(`b${i}`, "math", "add", [i, 1]));
  for (let i = 0; i < MAX_IN_FLIGHT; i++) assert.equal(typeof (await host.nextMessage()).result, "number");
  // A cancelled id can be used again.
  assert.deepEqual(await host.call(0, "math", "add", [2, 2]), { jsonrpc: "2.0", id: 0, result: 4 });
  assert.equal(host.rawLines.length, 0);
  await host.stop();
});

test("a cancelled call is not answered when it settles later", async () => {
  const host = await readyHost();
  host.send(callRequest(1, "math", "slowEcho", ["late", 200]));
  host.send({ jsonrpc: "2.0", method: "cancel", params: { id: 1 } });
  host.send(callRequest(2, "bad", "throws", []));
  assert.equal((await host.nextMessage()).id, 2);
  await new Promise((resolve) => setTimeout(resolve, 300));
  assert.deepEqual(await host.call(3, "math", "add", [1, 1]), { jsonrpc: "2.0", id: 3, result: 2 });
  assert.equal(host.rawLines.length, 0);
  await host.stop();
});

test("cancel with an unknown or malformed id is ignored", async () => {
  const host = await readyHost();
  host.send(callRequest(1, "math", "slowEcho", ["kept", 100]));
  for (const params of [{ id: 2 }, { id: "1" }, { id: 1.5 }, { id: null }, null, [], "1"]) {
    host.send({ jsonrpc: "2.0", method: "cancel", params });
  }
  assert.deepEqual(await host.nextMessage(), { jsonrpc: "2.0", id: 1, result: "kept" });
  assert.equal(host.rawLines.length, 0);
  await host.stop();
});

test("a call id that is still running is rejected", async () => {
  const host = await readyHost();
  host.send(callRequest("dup", "math", "slowEcho", [1, 200]));
  host.send(callRequest("dup", "math", "add", [1, 1]));
  const first = await host.nextMessage();
  assert.equal(first.error.code, Codes.INVALID_REQUEST);
  assert.deepEqual(await host.nextMessage(), { jsonrpc: "2.0", id: "dup", result: 1 });
  await host.stop();
});

test("results with many shared references fail fast as TooLarge", async () => {
  // dag(30) has 2^31 paths: without the value budget the check would not end.
  let dag = [];
  for (let i = 0; i < 30; i++) dag = [dag, dag];
  const started = Date.now();
  assert.throws(() => checkWireValue(dag), new RegExp(`more than ${MAX_NODES} values`));
  assert.ok(Date.now() - started < 2000);
  const host = await readyHost();
  assert.equal((await host.call(1, "bad", "dag", [30])).error.data.kind, "TooLarge");
  assert.deepEqual(await host.call(2, "math", "add", [1, 1]), { jsonrpc: "2.0", id: 2, result: 2 });
  await host.stop();
});

test("checkWireValue accepts negative zero, which the wire carries as 0", () => {
  assert.doesNotThrow(() => checkWireValue(-0));
  assert.equal(JSON.stringify(-0), "0");
});

test("the host exits when the server closes stdin", async () => {
  const host = await readyHost();
  host.send({ jsonrpc: "2.0", id: 1, method: "call", params: { module: "math", fn: "slowEcho", args: [1, 60000] } });
  assert.equal(await host.stop(), 0);
});

test("modules outside the allowed directory cannot be loaded", async () => {
  const host = startHost({ escape: "../host.test.mjs" });
  assert.equal(await host.exited, 2);
  assert.match(host.stderr(), /cannot load module escape/);
  assert.equal(host.rawLines.length, 0);
});

test("a module that fails to load stops the host before ready", async () => {
  const host = startHost({ broken: "broken.mjs" });
  assert.equal(await host.exited, 2);
  assert.match(host.stderr(), /load failure/);
  assert.equal(host.rawLines.length, 0);
});

test("parseArgs accepts only named absolute module paths", () => {
  const m = parseArgs(["--module", "a=/x/a.mjs", "--module", "b_2=/x/b.mjs"]);
  assert.deepEqual([...m], [["a", "/x/a.mjs"], ["b_2", "/x/b.mjs"]]);
  for (const argv of [
    ["--module"],
    ["--module", "a=rel.mjs"],
    ["--module", "=/x.mjs"],
    ["--module", "a-b=/x.mjs"],
    ["--module", "a=/x.mjs", "--module", "a=/y.mjs"],
    ["--other"],
  ]) {
    assert.throws(() => parseArgs(argv), undefined, JSON.stringify(argv));
  }
});

test("checkWireValue accepts plain JSON values", () => {
  for (const v of [null, true, 0, -1.5, "", "\u{1F600}", [], {}, { a: [1, { b: null }] }, Object.create(null)]) {
    assert.doesNotThrow(() => checkWireValue(v), JSON.stringify(v));
  }
  const getter = {};
  Object.defineProperty(getter, "g", { get: () => 1, enumerable: true });
  for (const v of [undefined, Infinity, Symbol("s"), new Map(), getter, { [Symbol("k")]: 1 }, { "\uDC00": 1 }]) {
    assert.throws(() => checkWireValue(v));
  }
});

test("lineSplitter joins chunks, drops oversized lines and resyncs", () => {
  const lines = [];
  let oversized = 0;
  const feed = lineSplitter((l) => lines.push(l), () => oversized++);
  feed(Buffer.from('{"a"'));
  feed(Buffer.from(":1}\n\n  \n{\"b\":2}\n"));
  feed(Buffer.alloc(MAX_MESSAGE_BYTES, 0x61));
  feed(Buffer.from("aa"));
  feed(Buffer.from('\n{"c":3}\n'));
  // A multi byte character split across chunks stays intact.
  const smile = Buffer.from('"\u{1F600}"\n');
  feed(smile.subarray(0, 3));
  feed(smile.subarray(3));
  feed(Buffer.alloc(MAX_MESSAGE_BYTES, 0x62));
  feed(Buffer.from("\n"));
  assert.deepEqual(lines, ['{"a":1}', '{"b":2}', '{"c":3}', '"\u{1F600}"', "b".repeat(MAX_MESSAGE_BYTES)]);
  assert.equal(oversized, 1);
});
