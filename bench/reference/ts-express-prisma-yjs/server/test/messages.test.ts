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

test('rejects timestamps in the future', () => {
  const { server, owners } = room();
  const bob = client(server, 2);
  messagesOf(bob.doc).push([msg('bob', 'late', NOW + 2001)]);
  assert.deepEqual(checkUpdate(server, bob.diff(), 'bob', owners, NOW), { ok: false, reason: 'timestamp in the future' });
  const ok = client(server, 6);
  messagesOf(ok.doc).push([msg('alice', 'soon', NOW + 2000)]);
  assert.ok(checkUpdate(server, ok.diff(), 'alice', owners, NOW).ok);
});

// MEASUREMENT 1.6 and AC-57 (e): a clock set back must not place a message earlier.
test('rejects a clock set back by one hour or ten years on a fresh client id', () => {
  for (const back of [3_600_000, 10 * 365 * 86_400_000]) {
    const { server, owners } = room();
    const skewed = client(server, 7);
    messagesOf(skewed.doc).push([msg('bob', 'skewed', NOW - back)]);
    assert.deepEqual(checkUpdate(server, skewed.diff(), 'bob', owners, NOW), { ok: false, reason: 'timestamp before a message it follows' });

    const empty = new Y.Doc({ gc: false });
    const first = client(empty, 8);
    messagesOf(first.doc).push([msg('bob', 'first in room', NOW - back)]);
    const update = Y.encodeStateAsUpdate(first.doc);
    assert.deepEqual(checkUpdate(empty, update, 'bob', new Map(), NOW, NOW - 60_000), { ok: false, reason: 'timestamp before the room was created' });
  }
});

test('rejects a message placed before a message it was inserted in front of', () => {
  const { server, owners } = room();
  const front = client(server, 9);
  messagesOf(front.doc).insert(0, [msg('bob', 'front', NOW - 3_600_000)]);
  assert.deepEqual(checkUpdate(server, front.diff(), 'bob', owners, NOW), { ok: false, reason: 'timestamp before a message it follows' });
});

test('rejects a message before the last accepted message of the same client id', () => {
  const { server, owners } = room();
  const bob = client(server, 2);
  messagesOf(bob.doc).push([msg('bob', 'one', NOW)]);
  Y.applyUpdate(server, bob.diff());
  owners.set(2, 'bob');
  const replica = client(server, 2);
  messagesOf(replica.doc).insert(0, [msg('bob', 'equal', NOW)]);
  assert.ok(checkUpdate(server, replica.diff(), 'bob', owners, NOW).ok, 'same time is allowed');
  const back = client(server, 2);
  messagesOf(back.doc).insert(0, [msg('bob', 'back', NOW - 1)]);
  assert.deepEqual(checkUpdate(server, back.diff(), 'bob', owners, NOW), { ok: false, reason: 'timestamp before your last message' });
  const inOne = client(server, 3);
  messagesOf(inOne.doc).push([msg('bob', 'b', NOW), msg('bob', 'a', NOW - 1)]);
  assert.deepEqual(checkUpdate(server, inOne.diff(), 'bob', owners, NOW), { ok: false, reason: 'timestamp before your last message' });
});

// Review of T60: an honest offline message must not be rejected because the same user wrote
// later from another client (that would close the socket on every reconnect).
test('accepts honest offline messages of a second client of the same user', () => {
  const { server, owners } = room();
  const laptop = client(server, 2);
  const phone = client(server, 3);
  messagesOf(laptop.doc).push([msg('bob', 'offline', NOW - 5000)]);
  messagesOf(phone.doc).push([msg('bob', 'online', NOW - 1000)]);
  assert.ok(checkUpdate(server, phone.diff(), 'bob', owners, NOW).ok);
  Y.applyUpdate(server, phone.diff());
  owners.set(3, 'bob');
  assert.ok(checkUpdate(server, laptop.diff(), 'bob', owners, NOW).ok);
});

test('accepts a message written offline hours ago after the messages it had seen', () => {
  const alice = new Y.Doc({ gc: false });
  alice.clientID = 1;
  messagesOf(alice).push([msg('alice', 'first', NOW - 4 * 3_600_000)]);
  const server = new Y.Doc({ gc: false });
  Y.applyUpdate(server, Y.encodeStateAsUpdate(alice));
  const traveller = client(server, 4);
  messagesOf(traveller.doc).push([msg('bob', 'from the train', NOW - 3 * 3_600_000)]);
  messagesOf(alice).push([msg('alice', 'meanwhile', NOW - 60_000)]);
  Y.applyUpdate(server, Y.encodeStateAsUpdate(alice));
  const owners = new Map([[1, 'alice']]);
  assert.ok(checkUpdate(server, traveller.diff(), 'bob', owners, NOW, NOW - 5 * 3_600_000).ok);
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
