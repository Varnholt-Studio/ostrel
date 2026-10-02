# Tab leader

Leader election between the tabs of one origin (ARCHITECTURE 5.4, D32). All tabs share one
replica and one outbox (`runtime/js/sync/outbox/`). Exactly one tab, the leader, holds the Web
Lock `ostrel-sync`, owns the WebSocket and drains the outbox. Other tabs wait in the lock queue
and exchange messages with the leader over the BroadcastChannel `ostrel-sync`.

## Files

| File | Content |
|---|---|
| `leader.mjs` | `createTabLeader({ tabId, locks, openChannel, onLead, onMessage, onError })` |
| `testing/memory_locks.mjs` | In-memory stand in for `navigator.locks.request` (exclusive, FIFO, abort, tab kill) |

## Behaviour

* `start()` joins the election: opens the channel, asks for the current leader (`who`) and
  queues a request for the lock. When the lock is granted, `onLead({ signal })` runs; the tab
  leads until `signal` aborts or `onLead` settles.
* When the leader stops, closes or crashes, the browser releases the lock and grants it to the
  next waiting tab, which then resumes from the outbox.
* If `onLead` settles on its own (for example it throws because the socket failed), the error
  goes to `onError`, the tab gives the lock up and asks again after `retryMs` (default 1 s),
  so another tab can take over and a failing tab does not spin.
* `post(body)` sends to the leader. It is handled by whichever tab holds the lock when the
  message arrives, so routing never depends on a stale leader id. On the leader it is
  delivered locally. A message sent while no tab leads is lost; ops are safe because they are
  in the durable outbox and the next leader drains it on takeover.
* `broadcast(body)` sends from the leader to all followers; it returns false on a follower.
* `leaderId()` is the last announced leader and is informational only. A `resign` from a tab
  that is not the known leader is ignored.
* Channel messages are untrusted; malformed ones are dropped.
* `onError` is a sink. If it throws, the exception is swallowed; leadership is reset in every
  case (`isLeader()` turns false, `resign` is sent) before the lock is handed on, so a failing
  error handler can never leave two tabs leading.
* `stop()` withdraws a queued request, or aborts `onLead`, waits for it to settle (handing the
  lock on) and closes the channel. A stopped leader cannot be started again.

## Running the checks

* Node (part of the gate): `node --test runtime/js/sync/leader/*.test.mjs runtime/js/sync/leader/testing/*.test.mjs`

## Open points

* A `down` message is accepted from any sender and updates `leaderId()`. The channel is
  same origin, but the consumer must still validate `body` like any other untrusted input.
* If `locks.request` fails with an error other than `AbortError`, the error is reported and
  the tab does not ask for the lock again; it stays a follower until it is restarted.
* `stop()` waits for `onLead` to settle. An `onLead` that ignores its `signal` keeps `stop()`
  pending.

* Wiring with the outbox (`pending`, `remove`, `discardForeign`) and the socket belongs to the
  sync client once the protocol stub (T5-2) is fixed; `onLead` is the hook for it.
* A browser check in headless Chromium with two real tabs (real Web Locks and
  BroadcastChannel, AC-59) is not part of this change.
