// Chat message shape shared by the client modules and checks for values received from
// the network or from local storage. Every such value is treated as untrusted.

export const MAX_TEXT_LENGTH = 4000;
export const MAX_ID_LENGTH = 64;

export interface Message {
  // Unique id chosen by the creating client (opaque string).
  id: string;
  // Room the message belongs to.
  room: string;
  // Handle of the author, set by the server from the verified token.
  author: string;
  // Plain text; rendered as text only, never as markup.
  text: string;
  // Creation time in ms since the epoch, after the server clamp. Undefined while the
  // message has not been acknowledged by the server.
  made?: number;
}

export type ParseResult = { ok: true; message: Message } | { ok: false; reason: string };

const ID_PATTERN = /^[A-Za-z0-9_-]+$/;

function isId(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= MAX_ID_LENGTH &&
    ID_PATTERN.test(value)
  );
}

// Validates an untrusted value and returns a fresh object with only the known fields.
export function parseMessage(value: unknown): ParseResult {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return { ok: false, reason: "not an object" };
  }
  const raw = value as Record<string, unknown>;
  if (!isId(raw.id)) return { ok: false, reason: "invalid id" };
  if (!isId(raw.room)) return { ok: false, reason: "invalid room" };
  if (!isId(raw.author)) return { ok: false, reason: "invalid author" };
  if (typeof raw.text !== "string" || raw.text.length === 0) {
    return { ok: false, reason: "invalid text" };
  }
  if (raw.text.length > MAX_TEXT_LENGTH) return { ok: false, reason: "text too long" };
  let made: number | undefined;
  if (raw.made !== undefined) {
    if (typeof raw.made !== "number" || !Number.isSafeInteger(raw.made) || raw.made < 0) {
      return { ok: false, reason: "invalid made" };
    }
    made = raw.made;
  }
  const message: Message = {
    id: raw.id,
    room: raw.room,
    author: raw.author,
    text: raw.text,
  };
  if (made !== undefined) message.made = made;
  return { ok: true, message };
}
