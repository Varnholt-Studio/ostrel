// Display order of chat messages, as the REF-A server contract prescribes
// (`../server/README.md`, Protocol): by `ts`, then by `id`. REF-A has no server clamp, so
// `ts` is the author's client time as accepted by the server. Messages still waiting in the
// outbox are ordered by the same rule, so a message keeps its place once it is delivered.

import type { Message } from "./message.ts";

// Compares strings by Unicode code point, never by UTF-16 code unit (ARCHITECTURE 6.2).
export function compareCodePoints(a: string, b: string): number {
  const ai = a[Symbol.iterator]();
  const bi = b[Symbol.iterator]();
  for (;;) {
    const x = ai.next();
    const y = bi.next();
    if (x.done || y.done) return x.done === y.done ? 0 : x.done ? -1 : 1;
    const cx = x.value.codePointAt(0) as number;
    const cy = y.value.codePointAt(0) as number;
    if (cx !== cy) return cx < cy ? -1 : 1;
  }
}

export function compareMessages(a: Message, b: Message): number {
  if (a.ts !== b.ts) return a.ts < b.ts ? -1 : 1;
  return compareCodePoints(a.id, b.id);
}

export interface Shown {
  message: Message;
  // True while the message is only in the local outbox.
  pending: boolean;
}

// Merges the messages of the room document with the pending outbox entries of that room.
// A duplicate id keeps the copy from the room document.
export function orderMessages(room: Iterable<Message>, pending: Iterable<Message>): Shown[] {
  const byId = new Map<string, Shown>();
  for (const m of room) if (!byId.has(m.id)) byId.set(m.id, { message: m, pending: false });
  for (const m of pending) if (!byId.has(m.id)) byId.set(m.id, { message: m, pending: true });
  return [...byId.values()].sort((a, b) => compareMessages(a.message, b.message));
}
