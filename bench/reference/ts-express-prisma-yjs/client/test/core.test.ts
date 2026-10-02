import { test } from "node:test";
import assert from "node:assert/strict";

import { MAX_TEXT_LENGTH, parseMessage, type Message } from "../src/message.ts";
import { compareCodePoints, orderMessages } from "../src/order.ts";
import { MemoryStorage, Outbox, wipeLocalState } from "../src/outbox.ts";

function msg(id: string, made?: number, author = "ann", text = "hi"): Message {
  const m: Message = { id, room: "general", author, text };
  if (made !== undefined) m.made = made;
  return m;
}

test("parseMessage accepts a valid message and drops unknown fields", () => {
  const r = parseMessage({ ...msg("a1", 5), extra: "<script>" });
  assert.equal(r.ok, true);
  if (r.ok) assert.deepEqual(r.message, msg("a1", 5));
});

test("parseMessage rejects hostile shapes", () => {
  const bad: unknown[] = [
    null,
    [],
    "x",
    { ...msg("a1"), id: "" },
    { ...msg("a1"), id: "a b" },
    { ...msg("a1"), room: "x".repeat(65) },
    { ...msg("a1"), author: 7 },
    { ...msg("a1"), text: "" },
    { ...msg("a1"), text: "x".repeat(MAX_TEXT_LENGTH + 1) },
    { ...msg("a1"), made: -1 },
    { ...msg("a1"), made: 1.5 },
    { ...msg("a1"), made: "1" },
  ];
  for (const value of bad) assert.equal(parseMessage(value).ok, false, JSON.stringify(value));
});

test("markup in text is kept as plain data, not interpreted", () => {
  const text = '<img src=x onerror="alert(1)"><a href="javascript:alert(1)">x</a>';
  const r = parseMessage(msg("a1", 1, "ann", text));
  assert.ok(r.ok);
  if (r.ok) assert.equal(r.message.text, text);
});

test("compareCodePoints orders by code point, not UTF-16 unit", () => {
  // U+FF21 is one UTF-16 unit above the surrogates of U+1F600 but a lower code point.
  assert.equal(compareCodePoints("Ａ", "\u{1F600}"), -1);
  assert.equal("Ａ" < "\u{1F600}", false);
  assert.equal(compareCodePoints("a", "ab"), -1);
  assert.equal(compareCodePoints("ab", "ab"), 0);
  assert.equal(compareCodePoints("B", "a"), -1);
});

test("same millisecond messages converge to one order on every client", () => {
  const a = msg("zz", 1000);
  const b = msg("aa", 1000);
  const c = msg("mm", 999);
  const first = orderMessages([a, b, c], []).map((m) => m.id);
  const second = orderMessages([c, a, b], []).map((m) => m.id);
  assert.deepEqual(first, ["mm", "aa", "zz"]);
  assert.deepEqual(second, first);
});

test("pending messages follow acknowledged ones and duplicates collapse", () => {
  const acked = [msg("b", 20), msg("a", 10)];
  const pending = [msg("p2"), msg("b"), msg("p1"), msg("p2")];
  assert.deepEqual(
    orderMessages(acked, pending).map((m) => m.id),
    ["a", "b", "p2", "p1"],
  );
});

test("unacknowledged entries in the acked set are ignored", () => {
  assert.deepEqual(orderMessages([msg("x")], []), []);
});

test("outbox keeps order across reload and drains after reconnect", async () => {
  const storage = new MemoryStorage();
  const box = new Outbox(storage, "ann");
  await box.add(msg("m1", undefined, "ann"));
  await box.add(msg("m2", 77, "ann"));
  await box.add(msg("m1", undefined, "ann"));
  const reloaded = new Outbox(storage, "ann");
  assert.deepEqual(
    (await reloaded.pending()).map((m) => [m.id, m.made]),
    [
      ["m1", undefined],
      ["m2", undefined],
    ],
  );
  const sent: string[] = [];
  const n = await reloaded.drain(
    async (m) => {
      sent.push(m.id);
      return true;
    },
    () => "ann",
  );
  assert.equal(n, 2);
  assert.deepEqual(sent, ["m1", "m2"]);
  assert.deepEqual(await storage.keys(), []);
});

test("outbox stops at the first failed send and keeps the rest", async () => {
  const box = new Outbox(new MemoryStorage(), "ann");
  await box.add(msg("m1"));
  await box.add(msg("m2"));
  const n = await box.drain(async (m) => m.id !== "m2", () => "ann");
  assert.equal(n, 1);
  assert.deepEqual((await box.pending()).map((m) => m.id), ["m2"]);
});

test("outbox is never drained under another principal", async () => {
  const box = new Outbox(new MemoryStorage(), "ann");
  await box.add(msg("m1"));
  let calls = 0;
  const send = async () => {
    calls += 1;
    return true;
  };
  assert.equal(await box.drain(send, () => "bob"), 0);
  assert.equal(await box.drain(send, () => undefined), 0);
  assert.equal(calls, 0);
  assert.equal((await box.pending()).length, 1);
});

test("outbox rejects foreign authors and drops tampered entries", async () => {
  const storage = new MemoryStorage();
  const box = new Outbox(storage, "ann");
  await assert.rejects(box.add(msg("m1", undefined, "bob")));
  await storage.set("outbox:ann", JSON.stringify([msg("t1", undefined, "bob"), msg("t2")]));
  assert.deepEqual((await box.pending()).map((m) => m.id), ["t2"]);
  await storage.set("outbox:ann", "{not json");
  assert.deepEqual(await box.pending(), []);
  assert.throws(() => new Outbox(storage, ""));
});

test("sign out wipes every outbox and local entry", async () => {
  const storage = new MemoryStorage();
  await new Outbox(storage, "ann").add(msg("m1"));
  await new Outbox(storage, "bob").add(msg("m2", undefined, "bob"));
  await storage.set("doc:general", "state");
  await wipeLocalState(storage);
  assert.deepEqual(await storage.keys(), []);
});
