// End to end against PostgreSQL. Needs DATABASE_URL with the migrations applied
// (npm run migrate) and wipes the tables before it starts.
import { after, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSync, randomUUID, sign, type KeyObject } from 'node:crypto';
import type { AddressInfo } from 'node:net';
import type { Server } from 'node:http';
import * as Y from 'yjs';
import * as encoding from 'lib0/encoding';
import * as decoding from 'lib0/decoding';
import * as sync from 'y-protocols/sync';
import { createApp, CLOSE } from '../src/app.ts';
import { privateKeyFromSeed } from '../src/paseto.ts';
import { PrismaStore } from '../src/store.ts';
import { messagesOf, type Message } from '../src/messages.ts';
import { LIMITS } from '../src/limits.ts';
import { WebsocketProvider } from 'y-websocket';

const enabled = Boolean(process.env.DATABASE_URL);
const store = enabled ? new PrismaStore() : (null as unknown as PrismaStore);
const serverKey = privateKeyFromSeed(Buffer.alloc(32, 9));
let server: Server;
let base = '';

let limits = { ...LIMITS, challengesPerMinute: 1000, registrationsPerHour: 1000 };

async function start() {
  server = createApp(store, serverKey, limits);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  base = `127.0.0.1:${(server.address() as AddressInfo).port}`;
}

async function stop() {
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
}

before(async () => {
  if (!enabled) return;
  await store.db.$executeRawUnsafe('TRUNCATE "User", "Room", "Member", "Client", "RoomUpdate" CASCADE');
  await start();
});

after(async () => {
  if (!enabled) return;
  await stop();
  await store.db.$disconnect();
});

async function http(method: string, path: string, body?: unknown, token?: string) {
  const res = await fetch(`http://${base}${path}`, {
    method,
    headers: { 'content-type': 'application/json', ...(token ? { authorization: `Bearer ${token}` } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const json = res.headers.get('content-type')?.startsWith('application/json');
  return { status: res.status, body: json ? await res.json() : await res.text() };
}

type Account = { key: KeyObject; publicKey: string; token: string; id: string };

async function proof(key: KeyObject, publicKey: string) {
  const { body } = await http('POST', '/auth/challenge');
  return { publicKey, challenge: body.challenge, signature: sign(null, Buffer.from(body.challenge), key).toString('base64url') };
}

function newKey() {
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  return { key: privateKey, publicKey: publicKey.export({ format: 'der', type: 'spki' }).subarray(-32).toString('base64url') };
}

async function register(handle: string): Promise<Account> {
  const { key, publicKey } = newKey();
  const res = await http('POST', '/auth/register', { ...(await proof(key, publicKey)), handle });
  assert.equal(res.status, 201, JSON.stringify(res.body));
  return { key, publicKey, token: res.body.token, id: res.body.user.id };
}

// Minimal y-websocket client: answers sync messages and sends local edits.
function connect(roomId: string, token: string, doc = new Y.Doc()) {
  const ws = new WebSocket(`ws://${base}/sync/${roomId}?token=${encodeURIComponent(token)}`);
  ws.binaryType = 'arraybuffer';
  const send = (write: (e: encoding.Encoder) => void) => {
    const e = encoding.createEncoder();
    encoding.writeVarUint(e, 0);
    write(e);
    if (ws.readyState === WebSocket.OPEN) ws.send(encoding.toUint8Array(e));
  };
  let synced: () => void;
  const ready = new Promise<void>((resolve) => (synced = resolve));
  const closed = new Promise<number>((resolve) => ws.addEventListener('close', (ev) => resolve(ev.code)));
  ws.addEventListener('open', () => send((e) => sync.writeSyncStep1(e, doc)));
  ws.addEventListener('message', (ev) => {
    const d = decoding.createDecoder(new Uint8Array(ev.data as ArrayBuffer));
    decoding.readVarUint(d);
    const reply = encoding.createEncoder();
    encoding.writeVarUint(reply, 0);
    const type = sync.readSyncMessage(d, reply, doc, 'remote');
    if (encoding.length(reply) > 1) ws.send(encoding.toUint8Array(reply));
    if (type === sync.messageYjsSyncStep2) synced();
  });
  doc.on('update', (update: Uint8Array, origin: unknown) => {
    if (origin !== 'remote') send((e) => sync.writeUpdate(e, update));
  });
  return { doc, ws, ready, closed, messages: () => messagesOf(doc).toArray() };
}

async function until(cond: () => boolean, what: string) {
  for (let i = 0; i < 200 && !cond(); i++) await new Promise((r) => setTimeout(r, 10));
  assert.ok(cond(), `timed out waiting for ${what}`);
}

const message = (author: string, text: string): Message => ({ id: randomUUID(), author, text, ts: Date.now() });
// Close code of a connection, or -1 if it is still open after two seconds.
const closeCode = (conn: { closed: Promise<number> }) => Promise.race([conn.closed, new Promise<number>((r) => setTimeout(() => r(-1), 2000))]);
const updateCount = () => store.db.roomUpdate.count();

test('chat server end to end on PostgreSQL', { skip: enabled ? false : 'DATABASE_URL not set' }, async (t) => {
  const alice = await register('alice');
  const bob = await register('Bob_2');
  let roomId = '';

  await t.test('registration validates handles, keys and challenges', async () => {
    const { key, publicKey } = newKey();
    assert.equal((await http('POST', '/auth/register', { ...(await proof(key, publicKey)), handle: 'ALICE' })).status, 409);
    for (const handle of ['ab', 'a'.repeat(21), 'no-dash', 'émile', '']) {
      assert.equal((await http('POST', '/auth/register', { ...(await proof(key, publicKey)), handle })).status, 400, handle);
    }
    const p = await proof(alice.key, alice.publicKey);
    assert.equal((await http('POST', '/auth/signin', p)).status, 200);
    assert.equal((await http('POST', '/auth/signin', p)).status, 401, 'challenge is single use');
    const forged = await proof(key, alice.publicKey);
    assert.equal((await http('POST', '/auth/signin', forged)).status, 401, 'signature by another key');
    const me = await http('GET', '/api/me', undefined, bob.token);
    assert.equal(me.body.handle, 'bob_2');
    assert.equal((await http('GET', '/api/me', undefined, bob.token + 'x')).status, 401);
    assert.equal((await http('GET', '/api/rooms')).status, 401);
  });

  await t.test('signing in again gives the same user and rooms', async () => {
    const room = await http('POST', '/api/rooms', { name: 'general' }, alice.token);
    assert.equal(room.status, 201);
    roomId = room.body.id;
    const again = await http('POST', '/auth/signin', await proof(alice.key, alice.publicKey));
    assert.equal(again.body.user.id, alice.id);
    const rooms = await http('GET', '/api/rooms', undefined, again.body.token);
    assert.deepEqual(rooms.body[0].members.map((m: { handle: string }) => m.handle), ['alice']);
  });

  await t.test('non members cannot open the room', async () => {
    assert.equal(await connect(roomId, bob.token).closed, CLOSE.forbidden);
    assert.equal(await connect(roomId, 'v4.public.garbage').closed, CLOSE.unauthorized);
  });

  await t.test('messages reach other members in real time and persist', async () => {
    assert.equal((await http('PUT', `/api/rooms/${roomId}/members/me`, undefined, bob.token)).status, 200);
    const a = connect(roomId, alice.token);
    const b = connect(roomId, bob.token);
    await Promise.all([a.ready, b.ready]);
    messagesOf(a.doc).push([message(alice.id, 'hi bob')]);
    await until(() => b.messages().length === 1, 'bob to receive');
    messagesOf(b.doc).push([message(bob.id, 'hi alice')]);
    await until(() => a.messages().length === 2, 'alice to receive');
    a.ws.close();
    b.ws.close();
    await stop();
    await start();
    const c = connect(roomId, alice.token);
    await c.ready;
    assert.deepEqual(c.messages().map((m) => m.text).sort(), ['hi alice', 'hi bob']);
    c.ws.close();
  });

  await t.test('the stock y-websocket provider can read and write', async () => {
    const doc = new Y.Doc();
    const provider = new WebsocketProvider(`ws://${base}/sync`, roomId, doc, { params: { token: bob.token }, WebSocketPolyfill: WebSocket as never, disableBc: true });
    await new Promise<void>((resolve) => provider.once('sync', () => resolve()));
    assert.equal(messagesOf(doc).length, 2);
    const watcher = connect(roomId, alice.token);
    await watcher.ready;
    messagesOf(doc).push([message(bob.id, 'from provider')]);
    await until(() => watcher.messages().some((m) => m.text === 'from provider'), 'provider message');
    provider.destroy();
    provider.awareness.destroy(); // the provider leaves its awareness timer running
    watcher.ws.close();
  });

  await t.test('offline messages sync after reconnect', async () => {
    const offline = connect(roomId, bob.token);
    await offline.ready;
    offline.ws.close();
    await offline.closed;
    messagesOf(offline.doc).push([message(bob.id, 'sent offline')]);
    const back = connect(roomId, bob.token, offline.doc);
    const watcher = connect(roomId, alice.token);
    await Promise.all([back.ready, watcher.ready]);
    await until(() => watcher.messages().some((m) => m.text === 'sent offline'), 'offline message');
    back.ws.close();
    watcher.ws.close();
  });

  await t.test('crafted updates are rejected and store nothing', async () => {
    const crafted: [string, (doc: Y.Doc) => void][] = [
      ['foreign author', (doc) => messagesOf(doc).push([message(alice.id, 'spoofed')])],
      ['delete', (doc) => messagesOf(doc).delete(0, 1)],
      ['existing id', (doc) => messagesOf(doc).push([{ ...message(bob.id, 'dup'), id: messagesOf(doc).get(0).id }])],
      ['future', (doc) => messagesOf(doc).push([{ ...message(bob.id, 'later'), ts: Date.now() + 60_000 }])],
      ['backdated', (doc) => messagesOf(doc).push([{ ...message(bob.id, 'earlier'), ts: Date.now() - 3_600_000 }])],
    ];
    for (const [name, edit] of crafted) {
      const before = await updateCount();
      const conn = connect(roomId, bob.token);
      await conn.ready;
      edit(conn.doc);
      assert.equal(await conn.closed, CLOSE.invalid, name);
      assert.equal(await updateCount(), before, `${name} stored nothing`);
    }
  });

  // MEASUREMENT 1.6: a client clock set back is rejected even for the first message of a room.
  await t.test('a clock set back is rejected in an empty room', async () => {
    const quiet = await http('POST', '/api/rooms', { name: 'quiet' }, bob.token);
    for (const back of [3_600_000, 10 * 365 * 86_400_000]) {
      const before = await updateCount();
      const conn = connect(quiet.body.id, bob.token);
      await conn.ready;
      messagesOf(conn.doc).push([{ ...message(bob.id, 'skewed'), ts: Date.now() - back }]);
      assert.equal(await closeCode(conn), CLOSE.invalid, `${back} ms back`);
      assert.equal(await updateCount(), before);
      conn.ws.close();
    }
  });

  await t.test('an offline message of a second client of the same user is kept', async () => {
    const laptop = connect(roomId, bob.token);
    await laptop.ready;
    laptop.ws.close();
    await laptop.closed;
    messagesOf(laptop.doc).push([message(bob.id, 'written on the laptop')]);
    await new Promise((r) => setTimeout(r, 20)); // the phone writes later, with a larger ts
    const phone = connect(roomId, bob.token);
    await phone.ready;
    messagesOf(phone.doc).push([message(bob.id, 'written on the phone')]);
    const watcher = connect(roomId, alice.token);
    await watcher.ready;
    await until(() => watcher.messages().some((m) => m.text === 'written on the phone'), 'phone message');
    const back = connect(roomId, bob.token, laptop.doc);
    await back.ready;
    await until(() => watcher.messages().some((m) => m.text === 'written on the laptop'), 'laptop message');
    assert.equal(back.ws.readyState, WebSocket.OPEN);
    for (const c of [phone, watcher, back]) c.ws.close();
  });

  await t.test('a client id used by alice cannot be reused by bob', async () => {
    const before = await updateCount();
    const conn = connect(roomId, bob.token);
    await conn.ready;
    const aliceClient = (await store.db.client.findFirst({ where: { userId: alice.id } }))!.id;
    conn.doc.clientID = Number(aliceClient);
    messagesOf(conn.doc).push([message(bob.id, 'as alice replica')]);
    assert.equal(await conn.closed, CLOSE.invalid);
    assert.equal(await updateCount(), before);
  });

  await t.test('membership changes concern only oneself, leaving evicts', async () => {
    assert.equal((await http('DELETE', `/api/rooms/${roomId}/members/${alice.id}`, undefined, bob.token)).status, 404);
    const conn = connect(roomId, bob.token);
    await conn.ready;
    assert.equal((await http('DELETE', `/api/rooms/${roomId}/members/me`, undefined, bob.token)).status, 204);
    assert.equal(await conn.closed, CLOSE.forbidden);
    const rooms = await http('GET', '/api/rooms', undefined, bob.token);
    assert.deepEqual(rooms.body[0].members.map((m: { handle: string }) => m.handle), ['alice']);
  });

  await t.test('flooding is limited per principal and per IP', async () => {
    await stop();
    limits = { ...LIMITS, opsBurst: 3, opsPerSecond: 0.001, challengesPerMinute: 2, registrationsPerHour: 1 };
    await start();
    assert.equal((await http('POST', '/auth/challenge')).status, 200);
    assert.equal((await http('POST', '/auth/challenge')).status, 200);
    assert.equal((await http('POST', '/auth/challenge')).status, 429);
    await stop();
    limits = { ...LIMITS, opsBurst: 3, opsPerSecond: 0.001, registrationsPerHour: 1 };
    await start();
    await register('carol');
    const { key, publicKey } = newKey();
    assert.equal((await http('POST', '/auth/register', { ...(await proof(key, publicKey)), handle: 'dave' })).status, 429);

    const before = await updateCount();
    const flood = connect(roomId, alice.token);
    const reader = connect(roomId, alice.token);
    await Promise.all([flood.ready, reader.ready]);
    messagesOf(flood.doc).push([message(alice.id, 'one'), message(alice.id, 'two'), message(alice.id, 'three')]);
    await until(() => reader.messages().some((m) => m.text === 'three'), 'burst to arrive');
    assert.equal(await updateCount(), before + 1);
    messagesOf(flood.doc).push([message(alice.id, 'too many')]);
    assert.equal(await flood.closed, CLOSE.limit);
    assert.equal(await updateCount(), before + 1, 'nothing of the excess is stored');
    assert.ok(!reader.messages().some((m) => m.text === 'too many'), 'nor broadcast');
    reader.ws.close();
  });
});
