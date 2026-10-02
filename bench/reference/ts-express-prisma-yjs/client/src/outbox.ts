// Offline outbox. Messages written while offline are kept per signed in user and sent
// after reconnect. The outbox is bound to the user who wrote the messages: it is never
// drained under another identity, and signing out deletes it together with the local
// store (MEASUREMENT 1.6, revocation and sign out probes).

import { parseMessage, type Message } from "./message.ts";

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

const PREFIX = "outbox:";

function keyFor(user: string): string {
  return PREFIX + user;
}

// Sends one message; resolves true when the server accepted it.
export type Send = (message: Message) => Promise<boolean>;

export class Outbox {
  private readonly storage: Storage;
  private readonly user: string;

  constructor(storage: Storage, user: string) {
    if (user.length === 0) throw new Error("outbox needs a signed in user");
    this.storage = storage;
    this.user = user;
  }

  async pending(): Promise<Message[]> {
    const raw = await this.storage.get(keyFor(this.user));
    if (raw === undefined) return [];
    let list: unknown;
    try {
      list = JSON.parse(raw);
    } catch {
      return [];
    }
    if (!Array.isArray(list)) return [];
    const out: Message[] = [];
    for (const item of list) {
      const parsed = parseMessage(item);
      // Entries of another author can only come from tampering; they are dropped.
      if (parsed.ok && parsed.message.author === this.user) out.push(parsed.message);
    }
    return out;
  }

  async add(message: Message): Promise<void> {
    if (message.author !== this.user) throw new Error("message author is not the outbox user");
    const list = await this.pending();
    if (list.some((m) => m.id === message.id)) return;
    const { made: _ignored, ...unacked } = message;
    list.push(unacked);
    await this.storage.set(keyFor(this.user), JSON.stringify(list));
  }

  // Sends pending messages in order while `currentUser` is still the outbox user. Stops at
  // the first failure so the order is kept. Returns the number of messages sent.
  async drain(send: Send, currentUser: () => string | undefined): Promise<number> {
    let sent = 0;
    for (;;) {
      if (currentUser() !== this.user) return sent;
      const list = await this.pending();
      const next = list[0];
      if (next === undefined) return sent;
      if (!(await send(next))) return sent;
      const rest = (await this.pending()).filter((m) => m.id !== next.id);
      if (rest.length === 0) await this.storage.delete(keyFor(this.user));
      else await this.storage.set(keyFor(this.user), JSON.stringify(rest));
      sent += 1;
    }
  }
}

// Sign out: deletes every outbox and every other locally stored entry.
export async function wipeLocalState(storage: Storage): Promise<void> {
  for (const key of await storage.keys()) await storage.delete(key);
}
