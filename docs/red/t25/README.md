# Red re-check of the sync protocol stub (T25)

Task: T145. Object under review: `crates/ostrel_sync/src/protocol.rs` at c9d04cd
(branch `t5/dana/25-protocol-stub`, base dev c4d3d8d), read against ARCHITECTURE v1.13
sections 5.3 to 5.6 and 5.9, D96, D101 to D103, D106 and AC-22, AC-34, AC-57.
No resubmission of T25 existed at the time of the check (dev 908a56d).

Out of scope here, because the architect already set them as conditions for the freeze
(review of T25 at c9d04cd): tags of initial `Set` elements in `OpBody::Make`, removal of
`Hello.version`, one `Reject Limit` per op of an over long batch, no `Ack` without an
accepted op. They are not repeated as findings.

Result: 3 findings S2, 1 finding S3, 1 finding S4. Recommendation: do not freeze the stub
before F1 to F4 have a decision, because each one changes a message or a limit of the
contract.

## F1 (S2): honest offline writes are rejected by the rate limit

References: ARCHITECTURE.md:379, :380, :388, :322 (a `Reject` rolls the change back),
`protocol.rs:302` to `:308`, `:321`, `:343` to `:346`, `:354`; AC-57 (a).

Steps: a client edits offline and queues 300 ops. On reconnect it drains the outbox. The
limits allow it to send up to 10 000 ops before the first `Ack` (`outbox_before_ack`) and up
to 500 ops per batch (`ops_per_batch`), but the server accepts only 200 ops per principal
in a burst (`ops_burst`) and then 50 per second.

Expected: every queued op is accepted (AC-57 (a): "they are accepted"; the reason AC-45 was
withdrawn is that it "rejects every honest offline write after a reconnect").
Actual by the contract: ops 201 to 300 of the first batch get `Reject Limit`. A `Reject`
rolls the change back and sets `rejected` (5.4); `Limit` has no retry semantics anywhere in
ARCHITECTURE, SPEC or `runtime/js/api.d.ts:125`. The same holds for more than 1 000 rows
created offline in one model (`rows_per_hour`). The limits contradict each other: the
client side limits invite a burst that the server side limit punishes with data loss.

The stub's own test checks `ops_burst >= ops_per_second` but not
`ops_per_batch <= ops_burst` or the outbox drain rate (`protocol.rs:378` to `:382`).

Proposed decision (architect): either `Limit` on a client op means "retry later" (op stays
in the outbox, not rolled back, a separate reason such as `Busy`), or the rate limit
applies to a different unit than outbox replay. Test for T124: offline 300 ops, reconnect,
all 300 accepted, none rejected.

## F2 (S2): one valid op does not fit one frame, the outbox stalls

References: ARCHITECTURE.md:376, :382, 5.3 canonical encoding; `protocol.rs:298`,
`:311`, `:154`; reproduction `docs/red/t25/frame_probe.rs`.

Steps: a row holds a `Text` field of 262 144 bytes made of control characters (allowed:
the limit is 256 KiB of UTF-8). JSON escapes each as `\u0001`. Alternatively a `make` of a
model with five ASCII `Text` fields of 256 KiB each.

Expected: an op that passes every value limit can be sent.
Actual (probe on c9d04cd, `ostrel_core::canon::encode`): one escaped value is 1 572 866
bytes, five ASCII fields are 1 310 730 bytes, the frame limit is 1 048 576. The frame is
a fatal `Error` and the connection closes (ARCHITECTURE.md:376). On reconnect the client
drains the outbox again and sends the same op first. Because `seq` must be strictly
increasing and nothing tells the client to drop this op, every later op of the replica is
blocked for good. A value of 256 KiB of `"` already needs 524 290 bytes, so two such
fields in one `make` reach the same state.

Proposed decision: the server answers an over long frame that is a parsable `Ops` batch
with `Reject Invalid` for the op instead of a fatal close, or the value limits are defined
on the encoded size so that every valid op fits one frame with room for the envelope; in
both cases a hostile test with a valid but over long op (AC-34).

## F3 (S2): no per replica record for `seq` and the replay `Ack`

References: ARCHITECTURE.md:281 (`replicas(replica, user, device_key, issued_at,
last_ack_hlc)`), :333, :334; D103 (b); `crates/ostrel_db/src/api.rs:276` to `:282`
(only `append_ops` and `ops_since`); `protocol.rs:178` to `:187`; AC-22, AC-57 (c), (d).

Steps: a client with a clock 5 s ahead sends an op. The server re-stamps it, commits it and
the connection drops before the `Ack` arrives. The client reconnects and resends the op.

Expected: step 2 of 5.5 acknowledges it again without effect, and the client learns the
accepted stamp, so `made`, `changed` and every LWW tie are the same on all replicas.
Actual by the contract: the record of a replica holds no highest accepted `seq`, and DB API
v1 has no lookup of an op by `OpId` (the same gap D103 (b) names). The server can neither
decide "already accepted" against "lower unseen, `Forged`" after a restart, nor fill
`Ack.seq` and `Ack.restamped` for the replayed op. If it answers with the client stamp or
none, the client keeps its own stamp while the server and every other replica hold the
re-stamped one: the canonical encodings differ and stay different (AC-22). Catch up does not
repair it, because the client recognises its own ops by `OpId` and does not apply them again
(5.4, "Exactly once delivery").

Proposed decision: add `last_seq` (and the stamp of that op) to the replica record, or a
`Connection` method to read one op by `OpId`; define `Ack` for a replay as carrying the
logged `ServerSeq` and accepted stamp. Simulator case for T5: lost `Ack` after a re-stamp,
then reconnect; replicas converge.

## F4 (S3): replicas in apps without `auth` are not bound to anything

References: ARCHITECTURE.md:281 (confirmation compares the token's user and device key),
:332, :583 (an app without `auth` has no keys or tables); `protocol.rs:15` to `:19`,
`:110` to `:113`, `:139` to `:144`.

Steps: in an app without `auth`, a client subscribes and receives `Ops` of other clients;
each `LoggedOp` carries the `OpId` and thus the other client's `ReplicaId`. It opens a new
connection with `Hello { replica: Some(victim) }` and sends one op with `seq` 4 294 967 295.

Expected: a replica can be used only by the client it was issued to.
Actual by the contract: there is no token, so nothing can mismatch; the stub and 5.3 do
not say whether such a `Hello` is confirmed. If it is, the server's highest `seq` of the
victim's replica is now the maximum, and every later op of the victim is "lower unseen" and
`Forged` (5.5 step 2): all writes of that client are lost. Severity S3 because apps without
`auth` have no rules that depend on `me`, but availability and data of other clients are
still hit.

Proposed decision: in apps without `auth`, `Welcome` hands out a random secret with the
replica that `Hello` must repeat, or every `Hello` gets a new replica.

## F5 (S4): limits table and stub disagree on over long batches

References: ARCHITECTURE.md:378 ("`Reject Limit` for the batch") against :314 and D106
("one `Reject Limit` per op"); `protocol.rs:302`. The stub part is already a condition of
the architect review; the table row in ARCHITECTURE 5.9 needs the same change so that 5.4
and 5.9 say one thing.

Also noted, no finding: a replica that expired after 30 days (ARCHITECTURE.md:341) has no
own signal in the protocol; the only matching code is the fatal `Unauthorized`
(`protocol.rs:252` to `:254`), which the client cannot tell apart from a revoked device
key. Worth one sentence in `ErrorCode` when F3 is decided.

## Not found

No issue in the reject reasons (`protocol.rs:191` to `:198` match 5.5 and
`runtime/js/api.d.ts:125`), the D96 wording of `SetRemove`, the D101 wording of
`Subscribe`, the D103 wording of `Hello`, or the values of the limits table against 5.9.
