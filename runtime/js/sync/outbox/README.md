# Outbox

Durable queue of local ops for the sync client (ARCHITECTURE 5.4, D32). All tabs of one origin
share it. The tab leader (`runtime/js/sync/leader/`) drains it; this module does not touch the
network and does not elect a leader.

## Files

| File | Content |
|---|---|
| `clock.mjs` | Pure replica clock: next `seq`, hybrid logical clock, wire forms of `OpId` and `Hlc` (ARCHITECTURE 5.3) |
| `outbox.mjs` | IndexedDB outbox: `append`, `pending`, `remove`, `discardForeign`, `observe`, `resumeAfter` |
| `testing/memory_indexeddb.mjs` | In-memory stand in for the IndexedDB subset used here, for Node tests |
| `browser/` | Same checks against the real IndexedDB in headless Chromium, two iframes as two tabs |

## Guarantees

* `append` allocates the op's `seq`, advances the HLC and stores the entry in one IndexedDB
  transaction. IndexedDB serialises overlapping read/write transactions across tabs, so two
  tabs never get the same `OpId`. A failed append stores nothing and leaves no gap in `seq`.
* Entries carry `user` and `replica`. `pending` returns only entries of the given pair, oldest
  first. `discardForeign` deletes every other entry and returns how many, for the visible
  notice of AC-43.
* `remove` is idempotent, so a repeated `Ack` is harmless. Removing entries never frees seqs.
* `observe` moves the HLC past a stamp seen elsewhere (remote op, re-stamped `Ack`);
  `resumeAfter` raises `seq` to the last seq the server accepted.

## Running the checks

* Node (part of the gate): `node --test runtime/js/sync/outbox/*.test.mjs`
* Chromium (local, not part of the offline gate):
  `OSTREL_CHROMIUM=/path/to/chrome node runtime/js/sync/outbox/browser/run_chromium.mjs`

## Open points

* The protocol stub (T5-2) fixes the final shape of the stored op and of `Ack`. Until then the
  op is stored as given and `remove` takes the acknowledged `OpId`s.
* The seq of a replica must outlive a deleted data store, otherwise a reused replica would send
  a lower `seq` than the server already accepted. `resumeAfter` is the hook for that; where the
  last accepted seq comes from (for example `Welcome`) is open.
