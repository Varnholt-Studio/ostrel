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

// `owners` maps every writer client id to its bound user id (unbound ids are absent).
export function checkUpdate(doc: Y.Doc, update: Uint8Array, user: string, owners: Map<number, string>, now = Date.now()): Verdict {
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
  const added: unknown[] = [];
  for (let item = after._start; item; item = item.right) {
    const known = sv.get(item.id.client) ?? 0;
    item.content.getContent().forEach((v, i) => {
      if (item.id.clock + i >= known) added.push(v);
    });
  }
  if (added.length !== newItems) return { ok: false, reason: 'update carries content that is not a new message' };
  let lastTs = Math.max(-Infinity, ...before.filter((m) => m.author === user).map((m) => m.ts));
  if (!added.every(isMessage)) return { ok: false, reason: 'invalid message' };
  for (const m of [...added].sort((a, b) => a.ts - b.ts)) {
    if (ids.has(m.id)) return { ok: false, reason: 'duplicate message id' };
    if (m.author !== user) return { ok: false, reason: 'author must be the signed in user' };
    if (m.ts > now + LIMITS.clockToleranceMs) return { ok: false, reason: 'timestamp in the future' };
    if (m.ts < lastTs) return { ok: false, reason: 'timestamp before your last message' };
    ids.add(m.id);
    lastTs = m.ts;
  }
  return { ok: true, added, clients: [...clients] };
}
