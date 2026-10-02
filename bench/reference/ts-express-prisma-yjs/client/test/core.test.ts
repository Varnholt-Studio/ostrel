import { test } from "node:test";
import assert from "node:assert/strict";

import {
  MAX_TEXT_LENGTH,
  MESSAGE_KEYS,
  newMessage,
  nextTs,
  parseMessage,
  type Message,
} from "../src/message.ts";
import { compareCodePoints, orderMessages } from "../src/order.ts";
import {
  MemoryStorage,
  Outbox,
  outcomeOfClose,
  wipeLocalState,
  type Entry,
  type Outcome,
} from "../src/outbox.ts";

// Ids in the server's format: lower case UUIDs (Prisma uuid() for users and rooms,
// crypto.randomUUID() for messages).
const ANN = "0a0a0a0a-0000-4000-8000-000000000001";
const BOB = "0b0b0b0b-0000-4000-8000-000000000002";
const ROOM = "1f1f1f1f-0000-4000-8000-000000000003";
const OTHER_ROOM = "2f2f2f2f-0000-4000-8000-000000000004";

function uuid(n: number): string {
  return `00000000-0000-4000-8000-${n.toString(16).padStart(12, "0")}`;
}

function msg(n: number, ts = 1000, author = ANN, text = "hi"): Message {
  return { author, id: uuid(n), text, ts };
}

function entry(n: number, ts = 1000, room = ROOM, author = ANN): Entry {
  return { room, message: msg(n, ts, author) };
}

// Wire form: the server's isMessage requires exactly these keys (server/src/messages.ts).
const SERVER_KEYS = "author,id,text,ts";

test("wire form has exactly the server key set", () => {
  assert.equal(MESSAGE_KEYS.join(), SERVER_KEYS);
  const r = newMessage(uuid(1), ANN, "hello", 5000, undefined);
  assert.ok(r.ok);
  if (r.ok) {
    assert.equal(Object.keys(r.message).sort().join(), SERVER_KEYS);
    assert.equal(Object.keys(JSON.parse(JSON.stringify(r.message))).sort().join(), SERVER_KEYS);
    assert.equal(r.message.ts, 5000);
  }
});

test("parseMessage rejects extra or missing keys like the server", () => {
  assert.equal(parseMessage({ ...msg(1), room: ROOM }).ok, false);
  assert.equal(parseMessage({ ...msg(1), made: 1 }).ok, false);
  const { ts: _ts, ...noTs } = msg(1);
  assert.equal(parseMessage(noTs).ok, false);
  assert.equal(parseMessage(msg(1)).ok, true);
});

test("text limit is 2000 UTF-16 units, as on the server", () => {
  assert.equal(MAX_TEXT_LENGTH, 2000);
  assert.equal(parseMessage({ ...msg(1), text: "x".repeat(2000) }).ok, true);
  assert.equal(parseMessage({ ...msg(1), text: "x".repeat(2001) }).ok, false);
  // 1000 astral characters are 2000 units: accepted; one more is 2002 units: rejected.
  assert.equal(parseMessage({ ...msg(1), text: "\u{1F600}".repeat(1000) }).ok, true);
  assert.equal(parseMessage({ ...msg(1), text: "\u{1F600}".repeat(1000) + "x" }).ok, false);
});

test("empty and white space only text is rejected", () => {
  for (const text of ["", " ", "   ", "\t\n", " ", "　"]) {
    assert.equal(parseMessage({ ...msg(1), text }).ok, false, JSON.stringify(text));
  }
  assert.equal(parseMessage({ ...msg(1), text: " a " }).ok, true);
});

test("ids and authors must be lower case UUIDs", () => {
  const bad = [
    "",
    "a1",
    "abc_DEF-123",
    uuid(0xabc).toUpperCase(),
    `{${uuid(1)}}`,
    uuid(1).replace(/-/g, ""),
    uuid(1) + "0",
    ` ${uuid(1)}`,
  ];
  for (const id of bad) {
    assert.equal(parseMessage({ ...msg(1), id }).ok, false, id);
    assert.equal(parseMessage({ ...msg(1), author: id }).ok, false, id);
  }
});

test("parseMessage rejects hostile shapes", () => {
  const bad: unknown[] = [
    null,
    [],
    "x",
    { ...msg(1), author: 7 },
    { ...msg(1), ts: 1.5 },
    { ...msg(1), ts: "1" },
    { ...msg(1), ts: Number.MAX_SAFE_INTEGER + 1 },
    { ...msg(1), ts: Number.NaN },
  ];
  for (const value of bad) assert.equal(parseMessage(value).ok, false, String(value));
});

test("markup in text is kept as plain data, not interpreted", () => {
  const text = '<img src=x onerror="alert(1)"><a href="javascript:alert(1)">x</a>';
  const r = parseMessage({ ...msg(1), text });
  assert.ok(r.ok);
  if (r.ok) assert.equal(r.message.text, text);
});

test("new messages refuse unpaired surrogates", () => {
  assert.equal(newMessage(uuid(1), ANN, "a\uD800b", 1, undefined).ok, false);
  assert.equal(newMessage(uuid(1), ANN, "a\uDC00", 1, undefined).ok, false);
  assert.equal(newMessage(uuid(1), ANN, "\u{1F600}", 1, undefined).ok, true);
});

test("ts is the client clock and never goes before the author's last message", () => {
  assert.equal(nextTs(5000, undefined), 5000);
  assert.equal(nextTs(5000, 4000), 5000);
  // Local clock behind the last accepted message: keep the last value, never go back.
  assert.equal(nextTs(5000, 6000), 6000);
  const r = newMessage(uuid(2), ANN, "x", 5000, 6000);
  assert.ok(r.ok);
  if (r.ok) assert.equal(r.message.ts, 6000);
});

test("compareCodePoints orders by code point, not UTF-16 unit", () => {
  // U+FF21 is one UTF-16 unit above the surrogates of U+1F600 but a lower code point.
  assert.equal(compareCodePoints("Ａ", "\u{1F600}"), -1);
  assert.equal("Ａ" < "\u{1F600}", false);
  assert.equal(compareCodePoints("a", "ab"), -1);
  assert.equal(compareCodePoints("ab", "ab"), 0);
  assert.equal(compareCodePoints("B", "a"), -1);
});

test("messages are ordered by (ts, id) on every client", () => {
  const a = msg(0xff, 1000);
  const b = msg(0xaa, 1000);
  const c = msg(0xcc, 999);
  const first = orderMessages([a, b, c], []).map((s) => s.message.id);
  const second = orderMessages([c, a, b], []).map((s) => s.message.id);
  assert.deepEqual(first, [uuid(0xcc), uuid(0xaa), uuid(0xff)]);
  assert.deepEqual(second, first);
});

test("pending messages sort by the same rule and duplicates keep the room copy", () => {
  const room = [msg(2, 20), msg(1, 10)];
  const pending = [msg(3, 15), msg(2, 99), msg(4, 5)];
  const shown = orderMessages(room, pending);
  assert.deepEqual(
    shown.map((s) => [s.message.id, s.message.ts, s.pending]),
    [
      [uuid(4), 5, true],
      [uuid(1), 10, false],
      [uuid(3), 15, true],
      [uuid(2), 20, false],
    ],
  );
});

test("outbox keeps order across reload and drains after reconnect", async () => {
  const storage = new MemoryStorage();
  const box = new Outbox(storage, ANN);
  await box.add(entry(1, 100));
  await box.add(entry(2, 200, OTHER_ROOM));
  await box.add(entry(1, 100));
  const reloaded = new Outbox(storage, ANN);
  assert.deepEqual(
    (await reloaded.pending()).map((e) => [e.room, e.message.id]),
    [
      [ROOM, uuid(1)],
      [OTHER_ROOM, uuid(2)],
    ],
  );
  const sent: Entry[] = [];
  const n = await reloaded.drain(
    async (e) => {
      sent.push(e);
      return "accepted";
    },
    () => ANN,
  );
  assert.equal(n, 2);
  assert.deepEqual(
    sent.map((e) => Object.keys(e.message).sort().join()),
    [SERVER_KEYS, SERVER_KEYS],
  );
  assert.deepEqual(await storage.keys(), []);
});

test("outbox refuses entries the server would reject", async () => {
  const box = new Outbox(new MemoryStorage(), ANN);
  await assert.rejects(box.add({ room: ROOM, message: { ...msg(1), text: "x".repeat(2001) } }));
  await assert.rejects(box.add({ room: ROOM, message: { ...msg(1), text: "  " } }));
  await assert.rejects(box.add({ room: "general", message: msg(1) }));
  await box.add(entry(1, 500));
  // A later message in the same room must not have an earlier ts.
  await assert.rejects(box.add(entry(2, 499)));
  await box.add(entry(3, 400, OTHER_ROOM));
  assert.deepEqual(
    (await box.pending()).map((e) => e.message.id),
    [uuid(1), uuid(3)],
  );
});

// Stand in for the REF-A server: rejects what the server's isMessage rejects (from the
// contract) and a duplicate message id; `offline` makes every send a transient failure.
function serverLike(state: { offline: boolean; ids: Set<string> }) {
  return async (e: Entry): Promise<Outcome> => {
    if (state.offline) return "retry";
    if (!parseMessage(e.message).ok || state.ids.has(e.message.id)) return outcomeOfClose(4400);
    state.ids.add(e.message.id);
    return "accepted";
  };
}

test("a rejected message does not block later ones (head of line)", async () => {
  const storage = new MemoryStorage();
  const box = new Outbox(storage, ANN);
  await box.add(entry(1, 100));
  await box.add(entry(2, 200));
  await box.add(entry(3, 300));
  // uuid(1) already exists in the room, so the server closes with 4400 for it.
  const state = { offline: false, ids: new Set([uuid(1)]) };
  assert.equal(await box.drain(serverLike(state), () => ANN), 2);
  assert.deepEqual(await box.pending(), []);
  assert.deepEqual(
    (await box.rejected()).map((e) => e.message.id),
    [uuid(1)],
  );
  assert.ok(state.ids.has(uuid(2)) && state.ids.has(uuid(3)));
});

test("a tampered oversized entry is dropped and the rest drains", async () => {
  const storage = new MemoryStorage();
  const box = new Outbox(storage, ANN);
  const big = { room: ROOM, message: { ...msg(1, 100), text: "x".repeat(3000) } };
  await storage.set("outbox:" + ANN, JSON.stringify([big, entry(2, 200)]));
  const state = { offline: false, ids: new Set<string>() };
  for (let i = 0; i < 3; i++) await box.drain(serverLike(state), () => ANN);
  assert.deepEqual(await box.pending(), []);
  assert.deepEqual([...state.ids], [uuid(2)]);
});

test("a transient failure stops the drain and keeps the order", async () => {
  const box = new Outbox(new MemoryStorage(), ANN);
  await box.add(entry(1, 100));
  await box.add(entry(2, 200));
  const state = { offline: true, ids: new Set<string>() };
  assert.equal(await box.drain(serverLike(state), () => ANN), 0);
  assert.deepEqual((await box.pending()).map((e) => e.message.id), [uuid(1), uuid(2)]);
  state.offline = false;
  assert.equal(await box.drain(serverLike(state), () => ANN), 2);
  assert.deepEqual(await box.rejected(), []);
});

test("close codes map to outcomes", () => {
  assert.equal(outcomeOfClose(4400), "rejected");
  assert.equal(outcomeOfClose(4403), "rejected");
  assert.equal(outcomeOfClose(4401), "retry");
  assert.equal(outcomeOfClose(4429), "retry");
  assert.equal(outcomeOfClose(1006), "retry");
});

test("outbox is never drained under another principal", async () => {
  const box = new Outbox(new MemoryStorage(), ANN);
  await box.add(entry(1));
  let calls = 0;
  const send = async (): Promise<Outcome> => {
    calls += 1;
    return "accepted";
  };
  assert.equal(await box.drain(send, () => BOB), 0);
  assert.equal(await box.drain(send, () => undefined), 0);
  assert.equal(calls, 0);
  assert.equal((await box.pending()).length, 1);
});

test("outbox rejects foreign authors and drops tampered entries", async () => {
  const storage = new MemoryStorage();
  const box = new Outbox(storage, ANN);
  await assert.rejects(box.add(entry(1, 1000, ROOM, BOB)));
  await storage.set("outbox:" + ANN, JSON.stringify([entry(7, 1, ROOM, BOB), entry(8)]));
  assert.deepEqual((await box.pending()).map((e) => e.message.id), [uuid(8)]);
  await storage.set("outbox:" + ANN, "{not json");
  assert.deepEqual(await box.pending(), []);
  assert.throws(() => new Outbox(storage, ""));
  assert.throws(() => new Outbox(storage, "ann"));
});

test("sign out wipes every outbox and local entry", async () => {
  const storage = new MemoryStorage();
  await new Outbox(storage, ANN).add(entry(1));
  await new Outbox(storage, BOB).add(entry(2, 1000, ROOM, BOB));
  await storage.set("doc:" + ROOM, "state");
  await wipeLocalState(storage);
  assert.deepEqual(await storage.keys(), []);
});
