# REF-A client

Browser client of the reference stack REF-A (TypeScript, Express, Prisma, Yjs with
y-websocket, React), used as the comparison baseline for AC-28 (SPEC 5, MEASUREMENT 4).
It implements the same chat app as the Ostrel chat example and must pass the same black box
acceptance suite (`bench/apps/chat/acceptance`).

## Status

Part 1: the dependency free client core with unit tests.

| File | Purpose |
|---|---|
| `src/message.ts` | Message shape and validation of untrusted values (network, local storage) |
| `src/order.ts` | Display order by `(made, id)` with code point string order, pending messages last |
| `src/outbox.ts` | Offline outbox bound to the signed in user, wipe on sign out |
| `test/core.test.ts` | Unit tests for the three modules |

Still to do: the React UI, the Yjs document and y-websocket provider, IndexedDB storage,
PASETO sign in, the production build, and the `package.json` entries in `bench/`.

## Running the tests

    bash run-tests.sh

Needs Node 22 (type stripping, no install step). These tests are not part of the offline
gate, which skips `bench/`; they run in the bench job.

## Contract assumptions

These hold until `bench/apps/chat/CONTRACT.md` and the REF-A server protocol exist; the
client follows those documents once they are written.

1. Order follows the Ostrel runtime (ARCHITECTURE 5.2, 5.5): the server clamps the creation
   time into its window and sets `made`; clients sort by `(made, id)` and ignore any
   client supplied `made`. Strings compare by Unicode code point (ARCHITECTURE 6.2).
2. Ids of messages, rooms and authors are ASCII `[A-Za-z0-9_-]`, 1 to 64 characters; text
   is 1 to 4000 UTF-16 units. The author is set by the server from the verified token.
3. The outbox is drained only while the user who wrote it is signed in; sign out deletes
   every locally stored entry (MEASUREMENT 1.6).
4. Message text is rendered as text only (React text nodes, no `dangerouslySetInnerHTML`,
   no automatic links).
5. Source files import each other with explicit `.ts` extensions so that Node runs them
   directly; the TypeScript build uses `allowImportingTsExtensions` with a bundler.
