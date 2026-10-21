# Sync client

The sync client of one tab (ARCHITECTURE 5.4, D32). It wires the tab leader
(`runtime/js/sync/leader/`), the shared outbox (`runtime/js/sync/outbox/`) and the WebSocket
together: the tab that leads opens the socket and drains the outbox; every tab can write.

## Files

| File | Content |
|---|---|
| `client.mjs` | `createSyncClient(...)`: leader election, sessions one after another, reconnects, messages between tabs |
| `session.mjs` | `runSession(...)`: one WebSocket from `Hello` to its close; drains the outbox and handles `Ack`, `Reject`, `Defer` and `Error` |
| `frame.mjs` | Frame codec: reads server frames, writes client frames, with the key rules of `crates/ostrel_sync/src/wire/` |
| `json.mjs` | Strict JSON reader: no repeated key, depth limit, no unpaired surrogate, number token form |
| `events.mjs` | Event queue of a session and the WebSocket hookup |
| `testing/fake_server.mjs` | Fake WebSocket, server frames and an in-memory identity store for Node tests |

## Behaviour

* **Frames.** Every key of a frame is required; an absent optional value is `null`. So an
  `error` frame without `retry_after_ms` is a fault (`MissingKey`) exactly like one without
  `sub` (D159, `wire/mod.rs`). `retry_after_ms` is set only for `RateLimited`, which also
  names its subscription; the session codes (`Superseded`, `TooManySessions`, `Overloaded`)
  never name one (`IllFormedError` otherwise). Faults close the connection, except a
  `snapshot` or `return` for a request the client does not have, which is dropped (D155).
* **Hello and Welcome.** `Hello` carries what the identity store holds (token, replica, its
  secret, last applied log position). A newly issued replica and its secret are stored. A
  `Welcome` for another replica than the one named in `Hello` is a fault. After the `Welcome`
  the outbox entries of any other (user, replica) pair are discarded, and `onNotice` gets
  `{ kind: 'foreign-ops-discarded', count }` for the visible notice of AC-43.
* **Draining.** The session sends the oldest pending ops as one `ops` frame, at most
  `ops_per_batch` ops and never over the frame limit, and sends the next batch only when
  every op of this one has its outcome. One batch in flight also keeps the client below
  `outbox_before_ack`.
* **Outcomes.** `Ack` confirms every waiting op of the batch up to its op (cumulative); its
  re-stamped HLCs of this replica move the replica clock (`outbox.observe`). `Reject` decides
  its op. Confirmed and rejected ops leave the outbox. `Defer` leaves its op and the later
  ones in the outbox, and they are sent again after `retry_after_ms` (D108). The server sends
  the outcomes of a batch in the order of D182 (ARCHITECTURE 5.4): every `Reject`, then the
  `Defer`, then the `Ack` frames. A `Reject` for an op that an `Ack` of this session already
  confirmed breaks that order: the session calls `onReject` for it (the server is
  authoritative, the app rolls the op back) and ends with a fault, so the client reconnects
  and catches up. It is never ignored. Any other outcome for an op that does not wait in the
  current batch is ignored. The leader passes every outcome to the other tabs (`onAck`,
  `onReject` run in every tab).
* **Writes in any tab.** A tab appends to the outbox and calls `changed()`. On the leader this
  wakes the session; on a follower it posts to the leader over the BroadcastChannel.
* **End of a session.** A closed socket, a faulty frame and the fatal codes `Protocol`,
  `FrameTooLarge`, `TooManySessions` and `Overloaded` reconnect after a delay that doubles from
  `reconnectMs` up to `maxReconnectMs` and starts again after a `Welcome`. `Superseded` makes
  the tab stop leading, so it opens no session until it leads again (Q27 (b)). `Unauthorized`,
  `ReplicaExpired` and an outbox whose oldest op is over the op or frame limit keep the tab
  offline until the app calls `reconnect()`; `onNotice` reports them.

## Running the checks

* Node (part of the gate): `node --test runtime/js/sync/client/*.test.mjs`

## Assumptions

* The outbox stores an op as `{ model, row, body }` in its wire form (names, values in their
  JSON form); the outbox adds `id` and `hlc`. The generated client writes that form.
* The replica clock (`seq`, HLC) lives in the outbox database, which sign out does not delete
  (5.6 deletes the data store), so a reused replica continues its `seq` (paula #348).
* The session discards foreign entries before it reads `pending`, so the limit that
  `outbox.pending` applies before its user filter (dora #442, `outbox.mjs:105`) cannot hide
  pending ops here. The fix in the outbox itself is open (other path).

## Not part of this change

* Subscriptions (`Subscribe`, `Held`, `Snapshot`, `Evict`, `Reset`, `ops` from the server)
  and server function calls (`Call`, `Return`): they need the compiled schema on the client
  to read and write typed values (D132). The decoder answers them with `Unsupported`; the
  subscription and call ids then come from one counter per session that never reuses an id
  (D155).
* Applying confirmed ops and snapshot rows to the local replica, `applied_server_seq`, and
  rolling back a rejected op in the view: the hooks `onAck` and `onReject` are the place.
* A browser check with two real tabs (AC-59) is T75.
