// Offline outbox. Messages written while offline are kept per signed in user and sent
// after reconnect. The outbox is bound to the user who wrote the messages: it is never
// drained under another identity, and signing out deletes it together with the local
// store (MEASUREMENT 1.6, revocation and sign out probes).
//
// The server rejects an update as a whole and closes the socket (`../server/README.md`).
// Each entry is therefore sent on its own, and a message the server refuses for good is
// moved to the rejected list instead of blocking every later message (head of line).

import { isUuid, parseMessage, type Message } from "./message.ts";

// Minimal key value storage. The browser build backs it with IndexedDB; tests use memory.
export interface Storage {
  get(key: string): Promise<string | undefined>;
  set(key: string, value: string): Promise<void>;
  delete(key: string): Promise<void>;
  keys(): Promise<string[]>;
}

export class MemoryStorage implements Storage {
  private readonly data = new Map<string, string>();
  async get(key: string): Promise<string | undefined> {
    return this.data.get(key);
  }
  async set(key: string, value: string): Promise<void> {
    this.data.set(key, value);
  }
  async delete(key: string): Promise<void> {
    this.data.delete(key);
  }
  async keys(): Promise<string[]> {
    return [...this.data.keys()];
  }
}

// One queued message and the room (Yjs document) it belongs to.
export interface Entry {
  room: string;
  message: Message;
}

// Result of one send attempt.
//   accepted: the server stored the message.
//   rejected: the server refused this message for good; retrying cannot succeed.
//   retry:    a transient condition (offline, rate limit, expired token); stop and retry later.
export type Outcome = "accepted" | "rejected" | "retry";

export type Send = (entry: Entry) => Promise<Outcome>;

// Maps a WebSocket close code of the REF-A server to an outcome for the message that caused
// it. 4400: invalid update. 4403: not a member of the room. 4401 (token) and 4429 (rate or
// quota) depend on time or a new sign in, not on the message.
export function outcomeOfClose(code: number): Outcome {
  return code === 4400 || code === 4403 ? "rejected" : "retry";
}

const PENDING = "outbox:";
const REJECTED = "rejected:";

function parseEntries(raw: string | undefined, user: string): Entry[] {
  if (raw === undefined) return [];
  let list: unknown;
  try {
    list = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(list)) return [];
  const out: Entry[] = [];
  for (const item of list) {
    if (typeof item !== "object" || item === null || !isUuid(item.room)) continue;
    const parsed = parseMessage(item.message);
    // Entries of another author can only come from tampering; they are dropped.
    if (parsed.ok && parsed.message.author === user) {
      out.push({ room: item.room, message: parsed.message });
    }
  }
  return out;
}

export class Outbox {
  private readonly storage: Storage;
  private readonly user: string;

  constructor(storage: Storage, user: string) {
    if (!isUuid(user)) throw new Error("outbox needs a signed in user id");
    this.storage = storage;
    this.user = user;
  }

  async pending(): Promise<Entry[]> {
    return parseEntries(await this.storage.get(PENDING + this.user), this.user);
  }

  // Messages the server refused for good, so the UI can mark them.
  async rejected(): Promise<Entry[]> {
    return parseEntries(await this.storage.get(REJECTED + this.user), this.user);
  }

  private async write(prefix: string, list: Entry[]): Promise<void> {
    if (list.length === 0) await this.storage.delete(prefix + this.user);
    else await this.storage.set(prefix + this.user, JSON.stringify(list));
  }

  async add(entry: Entry): Promise<void> {
    if (!isUuid(entry.room)) throw new Error("invalid room id");
    const parsed = parseMessage(entry.message);
    if (!parsed.ok) throw new Error(parsed.reason);
    const message = parsed.message;
    if (message.author !== this.user) throw new Error("message author is not the outbox user");
    const list = await this.pending();
    if (list.some((e) => e.message.id === message.id)) return;
    // The server rejects a ts before the author's last message in the room.
    if (list.some((e) => e.room === entry.room && e.message.ts > message.ts)) {
      throw new Error("ts goes backwards in the room");
    }
    list.push({ room: entry.room, message });
    await this.write(PENDING, list);
  }

  // Sends pending messages in order while `currentUser` is still the outbox user. A
  // rejected message is moved to the rejected list and the next one is sent; a retry
  // outcome stops the drain with the order kept. Returns the number of accepted messages.
  async drain(send: Send, currentUser: () => string | undefined): Promise<number> {
    let sent = 0;
    for (;;) {
      if (currentUser() !== this.user) return sent;
      const next = (await this.pending())[0];
      if (next === undefined) return sent;
      const outcome = await send(next);
      if (outcome === "retry") return sent;
      if (currentUser() !== this.user) return sent;
      const rest = (await this.pending()).filter((e) => e.message.id !== next.message.id);
      await this.write(PENDING, rest);
      if (outcome === "accepted") {
        sent += 1;
      } else {
        const rejected = await this.rejected();
        if (!rejected.some((e) => e.message.id === next.message.id)) rejected.push(next);
        await this.write(REJECTED, rejected);
      }
    }
  }
}

// Sign out: deletes every outbox and every other locally stored entry.
export async function wipeLocalState(storage: Storage): Promise<void> {
  for (const key of await storage.keys()) await storage.delete(key);
}
