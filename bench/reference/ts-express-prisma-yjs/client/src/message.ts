// Chat message shape and checks, following the REF-A server contract
// (`../server/README.md`, Protocol; `../server/src/messages.ts`, isMessage). A message is a
// plain object in the Yjs array `messages` of its room document with exactly the keys
// `author`, `id`, `text` and `ts`. The room is the document, so it is not a message field.
// Every value received from the network or from local storage is treated as untrusted.

// Upper bound of `text`, counted in UTF-16 code units like the server (`text.length`).
export const MAX_TEXT_LENGTH = 2000;

export interface Message {
  // Lower case UUID chosen by the creating client; must not exist in the room yet.
  id: string;
  // User id (lower case UUID) of the signed in author; the server rejects any other value.
  author: string;
  // Plain text; rendered as text only, never as markup.
  text: string;
  // Creation time in ms since the epoch, chosen by the client (REF-A has no server clamp).
  ts: number;
}

// The exact key set the server accepts, sorted.
export const MESSAGE_KEYS = ["author", "id", "text", "ts"] as const;

export type ParseResult = { ok: true; message: Message } | { ok: false; reason: string };

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export function isUuid(value: unknown): value is string {
  return typeof value === "string" && UUID.test(value);
}

// Same rule as the server: at least one non white space character and at most
// MAX_TEXT_LENGTH UTF-16 units.
export function isValidText(value: unknown): value is string {
  return (
    typeof value === "string" && value.trim().length > 0 && value.length <= MAX_TEXT_LENGTH
  );
}

// Validates an untrusted value with the server's rules. Unknown keys are an error, not
// silently dropped, because the server closes the socket (4400) for them.
export function parseMessage(value: unknown): ParseResult {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return { ok: false, reason: "not an object" };
  }
  const raw = value as Record<string, unknown>;
  if (Object.keys(raw).sort().join() !== MESSAGE_KEYS.join()) {
    return { ok: false, reason: "keys must be exactly author, id, text, ts" };
  }
  if (!isUuid(raw.id)) return { ok: false, reason: "invalid id" };
  if (!isUuid(raw.author)) return { ok: false, reason: "invalid author" };
  if (!isValidText(raw.text)) return { ok: false, reason: "invalid text" };
  if (typeof raw.ts !== "number" || !Number.isSafeInteger(raw.ts)) {
    return { ok: false, reason: "invalid ts" };
  }
  return { ok: true, message: { author: raw.author, id: raw.id, text: raw.text, ts: raw.ts } };
}

// Time stamp for a new message: the local clock, but never before the author's last
// message in the room, which the server rejects. A message written offline keeps the time
// it was written; the server accepts past values as long as they do not go backwards.
export function nextTs(now: number, lastOwnTs: number | undefined): number {
  return lastOwnTs === undefined ? now : Math.max(now, lastOwnTs);
}

// Builds a message for the signed in user. `id` comes from `crypto.randomUUID()` in the
// browser. Text with unpaired surrogates is refused here although the server accepts it,
// because other clients cannot encode it as valid UTF-8.
export function newMessage(
  id: string,
  author: string,
  text: string,
  now: number,
  lastOwnTs: number | undefined,
): ParseResult {
  if (!text.isWellFormed()) return { ok: false, reason: "invalid text" };
  return parseMessage({ author, id, text, ts: nextTs(now, lastOwnTs) });
}
