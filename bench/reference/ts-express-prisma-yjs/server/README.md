# REF-A server: TypeScript, Express, Prisma, Yjs

Server half of reference stack REF-A (SPEC AC-28, MEASUREMENT 4.1). It implements the
benchmark chat (rooms, messages, membership, offline send, sync, sign in with an Ed25519 user
key under open registration) the way a TypeScript team would build it with these libraries.
The client lives in `../client/`.

Chat scope, identical to the Ostrel chat (SPEC AC-65): all rooms are public, membership is
visible to every signed in user, users join and leave rooms themselves, messages cannot be
edited or deleted.

## Run

Requires Node 22.6 or later (TypeScript runs through `--experimental-strip-types`) and
PostgreSQL. Dependencies are bench only (ARCHITECTURE 11.1): install them from the committed
lockfile, never inside the gate.

```
npm ci --ignore-scripts
npx prisma generate
DATABASE_URL=postgresql://user@host/db npm run migrate
DATABASE_URL=postgresql://user@host/db REF_A_SERVER_SEED=<64 hex chars> npm start
DATABASE_URL=postgresql://user@host/db npm test   # wipes the tables of that database
```

The server also imports `ws`, `y-protocols` and `lib0`, which `y-websocket` installs as its
own dependencies; they get their own entries in `package.json` once the bench allowlist
(ARCHITECTURE 11) names them.

Without `DATABASE_URL` the end to end test is reported as skipped; the unit tests still run.
`REF_A_SERVER_SEED` is the Ed25519 seed of the server token key. Without it a random key is
used and all tokens become invalid on restart.

## Protocol

HTTP bodies are JSON. Keys, challenges and signatures are base64url without padding.

| Request | Body | Answer |
|---|---|---|
| `POST /auth/challenge` | none | `{ challenge }`, single use, valid 60 s |
| `POST /auth/register` | `{ publicKey, challenge, signature, handle, name? }` | 201 `{ token, user }`; 400 invalid handle; 401 bad proof; 409 handle or key taken |
| `POST /auth/signin` | `{ publicKey, challenge, signature }` | `{ token, user }`; 401 unknown key or bad proof |
| `GET /api/me`, `PATCH /api/me` | `{ name }` for PATCH | `{ id, handle, name }` |
| `GET /api/rooms` | none | `[{ id, name, members: [{ id, handle, name }] }]` |
| `POST /api/rooms` | `{ name }` | 201 room, the creator is a member |
| `PUT /api/rooms/:id/members/me` | none | joins, returns the room |
| `DELETE /api/rooms/:id/members/me` | none | 204, leaves and closes open room sockets |

`publicKey` is the raw 32 byte Ed25519 key, `signature` signs the UTF-8 bytes of the
challenge string. Handles are case folded and must match `[a-z0-9_]{3,20}`. The token is a
PASETO v4.public token with claims `sub` (user id), `kid` and `exp` (24 h); `/api` requests
send it as `Authorization: Bearer <token>`.

Realtime uses the y-websocket sync protocol (`y-websocket` `WebsocketProvider` works as is) at
`ws://host/sync/<roomId>?token=<token>`. Awareness messages are ignored. Each room is one Yjs
document with a single root, the array `messages`, holding plain objects
`{ id: uuid, author: userId, text, ts: epochMs }`. Clients show messages ordered by `ts`, then
`id`.

The server rejects an update, stores nothing and closes the socket with:

| Code | Reason |
|---|---|
| 4401 | missing, invalid or expired token |
| 4403 | not a member of the room, or evicted after leaving |
| 4400 | the update deletes content, writes anything other than new messages, uses a message id that exists, names another author, writes with a Yjs client id bound to another user, has a `ts` more than 2 s in the future or before the author's last message, or has text that is empty or longer than 2000 characters |
| 4429 | operation rate or row quota exceeded |

A client id is bound to the first user whose accepted update uses it, so a replica cannot be
taken over by another user (for example an outbox drained after sign out).

## Limits

Values follow ARCHITECTURE 5.9 so both stacks run under the same rules. All are in process
and reset on restart.

| Limit | Value | Enforced here |
|---|---|---|
| HTTP body, WebSocket frame | 1 MiB | yes |
| New messages per user | 50 per s, burst 200 | yes, close 4429 |
| Messages per user | 1 000 per hour | yes, close 4429 |
| Sign in challenges per IP | 10 per min | yes, 429 |
| New users per IP | 5 per hour | yes, 429 |
| Clock tolerance (future) | 2 000 ms | rejected, not re-stamped |
| Message text | 2 000 characters | yes, close 4400 |

## Known differences to the Ostrel chat

* Yjs has no server authority over individual operations, so a bad update is rejected as a
  whole and the connection is closed; the client keeps the rejected content in its local
  document. Ostrel rejects or re-stamps single operations.
* Timestamps outside the window are rejected instead of re-stamped (MEASUREMENT 1.6 allows
  either).
* Every incoming update is validated on a copy of the room document, which costs time linear
  in the room size.
* One key per user. Device links, key revocation and invite registration are not part of the
  reference chat.
