// Display order of chat messages. Same rule as the Ostrel runtime (ARCHITECTURE 5.2):
// acknowledged messages are ordered by (made, id), where `made` is the server clamped
// creation time, so every client shows the same sequence regardless of local clocks.
// Messages not yet acknowledged follow at the end in the order they were sent locally.

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
  const aAck = a.made !== undefined;
  const bAck = b.made !== undefined;
  if (aAck !== bAck) return aAck ? -1 : 1;
  if (aAck && bAck && a.made !== b.made) return (a.made as number) < (b.made as number) ? -1 : 1;
  return compareCodePoints(a.id, b.id);
}

// Returns the acknowledged messages sorted, then the pending ones in local send order.
// Duplicate ids keep the acknowledged copy.
export function orderMessages(acked: Iterable<Message>, pending: readonly Message[]): Message[] {
  const byId = new Map<string, Message>();
  for (const m of acked) {
    if (m.made !== undefined) byId.set(m.id, m);
  }
  const sorted = [...byId.values()].sort(compareMessages);
  const seen = new Set(byId.keys());
  for (const m of pending) {
    if (!seen.has(m.id)) {
      seen.add(m.id);
      sorted.push(m);
    }
  }
  return sorted;
}
