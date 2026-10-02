# REF-A client

Browser client of the reference stack REF-A (TypeScript, Express, Prisma, Yjs with
y-websocket, React), used as the comparison baseline for AC-28 (SPEC 5, MEASUREMENT 4).
It implements the same chat app as the Ostrel chat example and must pass the same black box
acceptance suite (`bench/apps/chat/acceptance`).

## Status

Part 1: the dependency free client core with unit tests.

| File | Purpose |
|---|---|
| `src/message.ts` | Message shape of the server contract, validation of untrusted values, `ts` choice |
| `src/order.ts` | Display order by `(ts, id)` with code point string order, pending entries included |
| `src/outbox.ts` | Offline outbox bound to the signed in user, rejected list, wipe on sign out |
| `test/core.test.ts` | Unit tests for the three modules, including the server contract cases |

Still to do: the React UI, the Yjs document and y-websocket provider, IndexedDB storage,
PASETO sign in, the production build, and the `package.json` entries in `bench/`.

## Running the tests

    bash run-tests.sh

Needs Node 22 (type stripping, no install step). These tests are not part of the offline
gate, which skips `bench/`; they run in the bench job.

## Server contract

The client follows the REF-A server protocol in `../server/README.md` and its validation in
`../server/src/messages.ts` (`isMessage`, `checkUpdate`):

1. A message is a plain object with exactly the keys `author`, `id`, `text`, `ts`. The room
   is the Yjs document (`/sync/<roomId>`), not a field. Any other key closes the socket
   with 4400, so `parseMessage` rejects unknown keys instead of dropping them.
2. `id` is a lower case UUID (`crypto.randomUUID()`); `author` is the signed in user id,
   also a lower case UUID.
3. `text` has at least one non white space character (`trim()`) and at most 2000 UTF-16
   units, counted like the server (`text.length`). The server README says "characters";
   the code counts units, and the client follows the code.
4. `ts` is chosen by the client: the local clock, never before the author's last message in
   the room (`nextTs`). REF-A has no server clamp: it rejects a `ts` more than 2 s ahead of
   its clock and a `ts` before the author's last message.
5. Clients order by `ts`, then `id`, comparing strings by Unicode code point
   (ARCHITECTURE 6.2). Pending outbox entries use the same rule.
6. The server rejects an update as a whole. The outbox sends one message per update; close
   code 4400 or 4403 marks that message as rejected and the drain continues with the next
   one, 4401, 4429 and network failures stop the drain and keep the order.

## Further assumptions

1. The outbox is drained only while the user who wrote it is signed in; sign out deletes
   every locally stored entry (MEASUREMENT 1.6).
2. Message text is rendered as text only (React text nodes, no `dangerouslySetInnerHTML`,
   no automatic links).
3. New messages with unpaired surrogates are refused locally, although the server accepts
   them.
4. Source files import each other with explicit `.ts` extensions so that Node runs them
   directly; the TypeScript build uses `allowImportingTsExtensions` with a bundler.
5. Part 2 must keep a rejected update out of the Yjs document that the provider syncs
   (for example by writing to the shared document only after the outbox send), because
   y-websocket would resend the rejected content on every reconnect.
