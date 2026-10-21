# Sync client

The sync client of one tab (ARCHITECTURE 5.4, D32). It wires the tab leader
(`runtime/js/sync/leader/`), the shared outbox (`runtime/js/sync/outbox/`) and the WebSocket
together: the tab that leads opens the socket and drains the outbox; every tab can write.

## Files

| File | Content |
|---|---|
| `client.mjs` | `createSyncClient(...)`: leader election, sessions one after another, reconnects, messages between tabs |
| `session.mjs` | `runSession(...)`: one WebSocket from `Hello` to its close; drains the outbox and handles `Ack`, `Reject`, `Defer` and `Error` |
| `frame.mjs` | Frame codec: reads server frames, writes client frames, with the key rules of `crates/ostrel_sync/src/wire/`; snapshot rows, server ops and call results typed by the schema |
| `value.mjs` | Typed values (`readValue`, `writeValue`) and the lookups of the compiled schema (`compileSchema`), as `wire/value.rs` |
| `subscribe.mjs` | `SubscriptionTable`: the subscriptions of one session, their ids, `subscribe`, `held` and `unsubscribe`, and the events of `snapshot`, `evict`, `reset` and `error` |
| `call.mjs` | `CallTable`: the server function calls of one session, their ids, timeout and outcome |
| `json.mjs` | Strict JSON reader: no repeated key, depth limit, no unpaired surrogate, number token form |
| `events.mjs` | Event queue of a session and the WebSocket hookup |
| `testing/fake_server.mjs` | Fake WebSocket, server frames and an in-memory identity store for Node tests |

## Behaviour

* **Typed messages (D132).** `decodeServer` with `schema` reads the rows of a `snapshot`
  against the model of its subscription, the ops of a server `ops` message against their model
  and fields, and the result of a `return` against its function's result type (`null` for a
  `Unit` function, D193). A value that does not fit, an unknown model or field, or an op that
  writes `made`, `changed` or `author` closes the connection, because client and server share
  one compiled schema. Without `schema` (only model names) these three messages stay
  `Unsupported`, as before.
* **Subscriptions.** Ids rise per session and are never used again (D155). `held` goes in
  pages of `ids_per_page` ids, the last one flagged. A `snapshot`, `evict`, `reset` or `error`
  for an id that is not open is dropped. `reset` and an `error` `RateLimited` subscribe the same
  query again under a new id, `RateLimited` after `retry_after_ms` (D159); any other `error`
  with the id ends the subscription. An `evict` naming another model than its subscription
  closes the connection.
* **Calls (SPEC 12.7).** Ids rise per session and are never used again. A call settles once:
  with its `return` (`{ ok: true, value }` or `{ ok: false, kind }`), with `Offline` when its
  frame never reached the socket or no session is open, or with `OutcomeUnknown` when the
  session ends or `call_timeout_ms` passes after it was sent. It is never sent twice; a late
  `return` is dropped.
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
  the tab stop leading, so it opens no session until it leads again (Q27 (b)). `Unauthorized`
  (an expired token, D194) leaves the outbox as it is: the client signs in again with the device
  key (`identity.signIn()`) and the next session sends the same ops with the same `seq`. It
  signs in once per refusal; a new token refused again, a failed sign in, `ReplicaExpired` and an
  outbox whose oldest op is over the op or frame limit keep the tab offline until the app calls
  `reconnect()`; `onNotice` reports them.

## Running the checks

* Node (part of the gate): `node --test runtime/js/sync/client/*.test.mjs`

## Assumptions

* The outbox stores an op as `{ model, row, body }` in its wire form (names, values in their
  JSON form); the outbox adds `id` and `hlc`. The generated client writes that form.
* The replica clock (`seq`, HLC) lives in the outbox database, which sign out does not delete
  (5.6 deletes the data store), so a reused replica continues its `seq` (paula #348).
* The compiled schema lists server functions as
  `functions: [{ name, params: [type, ...], result: type | 'Unit' }]` in the type form of
  `crates/ostrel_codegen_js/src/schema.rs` (proposed to T235, #2986). Rows may carry `made`,
  `changed` and, in an app with `auth`, `author`.
* The generated client builds the query of a `subscribe` in its wire form (5.4 "Query"); the
  client checks its head and model, the server the rest.
* A `call` whose frame would be over the frame limit is not sent and settles with `Limit`.
* `identity.signIn()` gets a new token with the device key (ARCHITECTURE 8) and stores it for
  the next `load()`; it resolves to whether it succeeded. Without it, `Unauthorized` keeps the
  tab offline as before.
* The session discards foreign entries before it reads `pending`, so the limit that
  `outbox.pending` applies before its user filter (dora #442, `outbox.mjs:105`) cannot hide
  pending ops here. The fix in the outbox itself is open (other path).

## Not part of this change

* Wiring the two tables into `session.mjs` (create them at `Welcome`, pass `schema` and their
  `requests` to `decodeServer`, route `snapshot`, `evict`, `reset`, `error` with a subscription
  and `return` to them, `close()` both when the session ends) and the client API on top: the
  queries the store keeps across sessions with their `held` rows, and calls from follower tabs
  through the leader (SPEC 12.7: only the caller's tab sees the outcome). `session.mjs` belongs
  to T227 at the time of writing.
* Applying confirmed ops and snapshot rows to the local replica, `applied_server_seq`, and
  rolling back a rejected op in the view: T227 (`apply.mjs`, `LocalReplica.applyOps`,
  `applySnapshot`, `evict`). The session wiring above passes the decoded `snapshot`, `evict`
  and server `ops` to it.
* A browser check with two real tabs (AC-59) is T75.
