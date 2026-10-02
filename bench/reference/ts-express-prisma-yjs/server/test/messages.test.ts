import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import * as Y from 'yjs';
import { checkUpdate, messagesOf, writersOf, type Message } from '../src/messages.ts';

const NOW = 1_800_000_000_000;
const msg = (author: string, text = 'hello', ts = NOW): Message => ({ id: randomUUID(), author, text, ts });

// A room whose document holds one message by alice, written with client id 1.
function room() {
  const server = new Y.Doc({ gc: false });
  const alice = new Y.Doc({ gc: false });
  alice.clientID = 1;
  messagesOf(alice).push([msg('alice', 'first', NOW - 1000)]);
  Y.applyUpdate(server, Y.encodeStateAsUpdate(alice));
  const owners = new Map([[1, 'alice']]);
  return { server, owners };
}

// A client replica of the room with a fixed client id, and the diff it would send.
function client(server: Y.Doc, clientID: number) {
  const doc = new Y.Doc({ gc: false });
  Y.applyUpdate(doc, Y.encodeStateAsUpdate(server));
  doc.clientID = clientID; // set after the sync, Yjs would replace an id it sees in use
  return { doc, diff: () => Y.encodeStateAsUpdate(doc, Y.encodeStateVector(server)) };
}

function check(update: Uint8Array, user: string) {
  const { server, owners } = room();
  return checkUpdate(server, update, user, owners, NOW);
}

test('accepts a new message by the signed in user and reports its client id', () => {
  const { server, owners } = room();
  const bob = client(server, 2);
  const m = msg('bob');
  messagesOf(bob.doc).push([m]);
  const v = checkUpdate(server, bob.diff(), 'bob', owners, NOW);
  assert.deepEqual(v, { ok: true, added: [m], clients: [2] });
  assert.deepEqual(writersOf(server, bob.diff()), [2]);
});

test('accepts several messages in one update and an update with nothing new', () => {
  const { server, owners } = room();
  const bob = client(server, 2);
  messagesOf(bob.doc).push([msg('bob', 'a', NOW - 10), msg('bob', 'b', NOW)]);
  const v = checkUpdate(server, bob.diff(), 'bob', owners, NOW);
  assert.ok(v.ok && v.added.length === 2);
  assert.deepEqual(checkUpdate(server, Y.encodeStateAsUpdate(server), 'bob', owners, NOW), { ok: true, added: [], clients: [] });
});

test('rejects a message whose author is another user', () => {
  const { server } = room();
  const bob = client(server, 2);
  messagesOf(bob.doc).push([msg('alice')]);
  assert.equal(check(bob.diff(), 'bob').ok, false);
});

test('rejects deleting a message of another user and of oneself', () => {
  const { server } = room();
  const bob = client(server, 2);
  messagesOf(bob.doc).delete(0, 1);
  assert.deepEqual(check(bob.diff(), 'bob'), { ok: false, reason: 'messages cannot be deleted' });
  const alice = client(server, 1);
  messagesOf(alice.doc).delete(0, 1);
  assert.equal(check(alice.diff(), 'alice').ok, false);
});

test('rejects writing with a client id bound to another user', () => {
  const { server } = room();
  const forged = client(server, 1);
  messagesOf(forged.doc).push([msg('bob')]);
  assert.deepEqual(check(forged.diff(), 'bob'), { ok: false, reason: 'client id belongs to another user' });
});

test('rejects a message id that already exists', () => {
  const { server, owners } = room();
  const bob = client(server, 2);
  const existing = messagesOf(server).get(0);
  messagesOf(bob.doc).push([{ ...msg('bob'), id: existing.id }]);
  assert.deepEqual(checkUpdate(server, bob.diff(), 'bob', owners, NOW), { ok: false, reason: 'duplicate message id' });
  const twice = client(server, 3);
  const m = msg('bob');
  messagesOf(twice.doc).push([m, { ...m, text: 'again' }]);
  assert.equal(check(twice.diff(), 'bob').ok, false);
});

test('rejects timestamps in the future and before the own last message', () => {
  const { server, owners } = room();
  const bob = client(server, 2);
  messagesOf(bob.doc).push([msg('bob', 'late', NOW + 2001)]);
  assert.deepEqual(checkUpdate(server, bob.diff(), 'bob', owners, NOW), { ok: false, reason: 'timestamp in the future' });
  const alice = client(server, 5);
  messagesOf(alice.doc).push([msg('alice', 'back', NOW - 3_600_000)]);
  assert.deepEqual(checkUpdate(server, alice.diff(), 'alice', owners, NOW), { ok: false, reason: 'timestamp before your last message' });
  const ok = client(server, 6);
  messagesOf(ok.doc).push([msg('alice', 'soon', NOW + 2000)]);
  assert.ok(checkUpdate(server, ok.diff(), 'alice', owners, NOW).ok);
});

test('rejects malformed messages', () => {
  const cases: unknown[] = [
    { ...msg('bob'), text: '' },
    { ...msg('bob'), text: '   ' },
    { ...msg('bob'), text: 'x'.repeat(2001) },
    { ...msg('bob'), id: 'not-a-uuid' },
    { ...msg('bob'), ts: 1.5 },
    { ...msg('bob'), extra: true },
    'plain text',
    [msg('bob')],
    null,
  ];
  for (const c of cases) {
    const { server } = room();
    const bob = client(server, 2);
    messagesOf(bob.doc).push([c as Message]);
    assert.equal(check(bob.diff(), 'bob').ok, false, JSON.stringify(c));
  }
  const { server } = room();
  const bob = client(server, 2);
  messagesOf(bob.doc).push([{ ...msg('bob'), text: 'x'.repeat(2000) }]);
  assert.ok(check(bob.diff(), 'bob').ok);
});

test('rejects nested shared types, map entries and other shared roots', () => {
  const { server } = room();
  const nested = client(server, 2);
  const inner = new Y.Map();
  messagesOf(nested.doc).push([inner as unknown as Message]);
  inner.set('author', 'bob');
  assert.equal(check(nested.diff(), 'bob').ok, false);

  const text = client(server, 3);
  text.doc.getText('notes').insert(0, 'hi');
  assert.deepEqual(check(text.diff(), 'bob'), { ok: false, reason: 'unknown shared type' });

  const mapped = client(server, 4);
  mapped.doc.getMap('messages').set('x', msg('bob'));
  assert.equal(check(mapped.diff(), 'bob').ok, false);
});

test('rejects updates that depend on content the server does not have, and garbage', () => {
  const { server } = room();
  const bob = client(server, 2);
  messagesOf(bob.doc).push([msg('bob', 'one')]);
  const first = bob.diff();
  messagesOf(bob.doc).push([msg('bob', 'two')]);
  const second = Y.encodeStateAsUpdate(bob.doc, Y.encodeStateVector(server));
  const onlySecond = Y.diffUpdate(second, Y.encodeStateVectorFromUpdate(Y.mergeUpdates([Y.encodeStateAsUpdate(server), first])));
  assert.equal(check(onlySecond, 'bob').ok, false);
  assert.deepEqual(check(new Uint8Array([1, 2, 3, 250]), 'bob'), { ok: false, reason: 'malformed update' });
});

test('a rejected update leaves the room document unchanged', () => {
  const { server, owners } = room();
  const before = Y.encodeStateAsUpdate(server);
  const bob = client(server, 2);
  messagesOf(bob.doc).push([msg('alice')]);
  checkUpdate(server, bob.diff(), 'bob', owners, NOW);
  assert.deepEqual(Y.encodeStateAsUpdate(server), before);
});
