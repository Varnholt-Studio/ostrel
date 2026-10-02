// Server side validation of Yjs updates for a chat room.
// Yjs itself accepts any update, so every update is first applied to a copy of the room
// document and the result is compared with the rules of the chat app.
import * as Y from 'yjs';
import { LIMITS } from './limits.ts';

export type Message = { id: string; author: string; text: string; ts: number };
export type Verdict = { ok: true; added: Message[]; clients: number[] } | { ok: false; reason: string };

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export function messagesOf(doc: Y.Doc): Y.Array<Message> {
  return doc.getArray<Message>('messages');
}

// Yjs client ids that write new content in this update.
export function writersOf(doc: Y.Doc, update: Uint8Array): number[] {
  const known = Y.encodeStateVector(doc);
  const sv = Y.decodeStateVector(known);
  const { structs } = Y.decodeUpdate(update);
  const writers = new Set<number>();
  for (const s of structs) {
    if (!(s instanceof Y.Skip) && s.id.clock + s.length > (sv.get(s.id.client) ?? 0)) writers.add(s.id.client);
  }
  return [...writers];
}

function isMessage(v: unknown): v is Message {
  if (typeof v !== 'object' || v === null || Array.isArray(v) || v instanceof Y.AbstractType) return false;
  const m = v as Record<string, unknown>;
  return (
    Object.keys(m).sort().join() === 'author,id,text,ts' &&
    typeof m.id === 'string' && UUID.test(m.id) &&
    typeof m.author === 'string' &&
    typeof m.text === 'string' && m.text.trim().length > 0 && m.text.length <= LIMITS.messageChars &&
    Number.isSafeInteger(m.ts)
  );
}

// A new message value of the update with the Yjs ids of what its writer had seen next to it.
type Written = { client: number; clock: number; value: unknown; seen: string[] };
const key = (client: number, clock: number) => `${client}:${clock}`;

// `owners` maps every writer client id to its bound user id (unbound ids are absent).
// `roomCreated` is the server time the room was created; nobody can write into it earlier.
//
// Yjs content cannot be re-stamped by the server without diverging from the writer's copy, so
// a timestamp outside its window rejects the update. The window is built only from facts an
// honest offline writer cannot contradict, so an honest outbox is never rejected for its clock:
// * at most the server time plus the tolerance;
// * not before the last accepted message of the same Yjs client id (one client id is one
//   document instance, and its messages are written in clock order);
// * not before a message the writer inserted it next to, minus twice the tolerance (that
//   message was stamped at most one tolerance late and seen by a clock at most one early);
// * not before the creation of the room, minus the tolerance.
export function checkUpdate(
  doc: Y.Doc,
  update: Uint8Array,
  user: string,
  owners: Map<number, string>,
  now = Date.now(),
  roomCreated = -Infinity,
): Verdict {
  let decoded;
  try {
    decoded = Y.decodeUpdate(update);
  } catch {
    return { ok: false, reason: 'malformed update' };
  }
  if (decoded.ds.clients.size > 0) return { ok: false, reason: 'messages cannot be deleted' };
  const sv = Y.decodeStateVector(Y.encodeStateVector(doc));
  let newItems = 0;
  const clients = new Set<number>();
  for (const s of decoded.structs) {
    const known = sv.get(s.id.client) ?? 0;
    if (s instanceof Y.Skip || s.id.clock + s.length <= known) continue;
    if (s instanceof Y.GC) return { ok: false, reason: 'garbage collected content' };
    const owner = owners.get(s.id.client);
    if (owner !== undefined && owner !== user) return { ok: false, reason: 'client id belongs to another user' };
    clients.add(s.id.client);
    newItems += s.id.clock + s.length - Math.max(s.id.clock, known);
  }
  if (newItems === 0) return { ok: true, added: [], clients: [] };

  const before = messagesOf(doc).toArray();
  const ids = new Set(before.map((m) => m.id));
  const copy = new Y.Doc({ gc: false });
  Y.applyUpdate(copy, Y.encodeStateAsUpdate(doc));
  try {
    Y.applyUpdate(copy, update);
  } catch {
    return { ok: false, reason: 'malformed update' };
  }
  if (copy.store.pendingStructs || copy.store.pendingDs) return { ok: false, reason: 'update depends on missing content' };
  if ([...copy.share.keys()].some((k) => k !== 'messages')) return { ok: false, reason: 'unknown shared type' };
  const after = messagesOf(copy);
  if (after._map.size > 0) return { ok: false, reason: 'unexpected map entries' };

  // Every new item must be a value of the messages array.
  const values = new Map<string, unknown>();
  const lastOfClient = new Map<number, number>();
  const written: Written[] = [];
  for (let item = after._start; item; item = item.right) {
    const known = sv.get(item.id.client) ?? 0;
    const { client, clock } = item.id;
    const right = item.rightOrigin ? [key(item.rightOrigin.client, item.rightOrigin.clock)] : [];
    item.content.getContent().forEach((value, i) => {
      values.set(key(client, clock + i), value);
      if (clock + i < known) {
        const ts = (value as Message).ts;
        lastOfClient.set(client, Math.max(lastOfClient.get(client) ?? -Infinity, ts));
        return;
      }
      const left = i > 0 ? key(client, clock + i - 1) : item.origin ? key(item.origin.client, item.origin.clock) : null;
      written.push({ client, clock: clock + i, value, seen: left ? [left, ...right] : right });
    });
  }
  if (written.length !== newItems) return { ok: false, reason: 'update carries content that is not a new message' };
  const added = written.map((w) => w.value);
  if (!added.every(isMessage)) return { ok: false, reason: 'invalid message' };
  written.sort((a, b) => a.client - b.client || a.clock - b.clock);
  for (const w of written) {
    const m = w.value as Message;
    if (ids.has(m.id)) return { ok: false, reason: 'duplicate message id' };
    if (m.author !== user) return { ok: false, reason: 'author must be the signed in user' };
    if (m.ts > now + LIMITS.clockToleranceMs) return { ok: false, reason: 'timestamp in the future' };
    if (m.ts < roomCreated - LIMITS.clockToleranceMs) return { ok: false, reason: 'timestamp before the room was created' };
    if (m.ts < (lastOfClient.get(w.client) ?? -Infinity)) return { ok: false, reason: 'timestamp before your last message' };
    for (const k of w.seen) {
      const seen = values.get(k) as Message | undefined;
      if (seen && Number.isSafeInteger(seen.ts) && m.ts < seen.ts - 2 * LIMITS.clockToleranceMs) {
        return { ok: false, reason: 'timestamp before a message it follows' };
      }
    }
    ids.add(m.id);
    lastOfClient.set(w.client, m.ts);
  }
  return { ok: true, added, clients: [...clients] };
}
