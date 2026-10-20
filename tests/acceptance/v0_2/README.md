# Rule bypass suite (v0.2)

Crafted requests that bypass the generated client and talk to the server directly over the
sync protocol (ARCHITECTURE 5.4). Every case either must be denied with an empty database
diff, or is the allowed control that proves the same rule is not simply denying everything.

| Criterion | Case file | Parts |
|---|---|---|
| AC-11 | `cases/ac_11.json` | wrong principal, forged owner, unknown fields and schema faults (D116, D132), payload of 10 MiB, malformed encoding, read path |
| AC-31 | `cases/ac_31.json` | `make` on an existing or tombstoned id, in the same and in another model |
| AC-32 | `cases/ac_32.json` | crafted `Call` with another principal's data, function without `call` rule, bulk `drop` with one denied row, argument faults |
| AC-33 | `cases/ac_33.json` | (a) to (e), on the apps of `examples/v0_2/rules_fail_closed/` |
| AC-54 | `cases/ac_54.json` | crafted `Call` of an `as system` function by a principal for whom the `call` rule is false |
| AC-55 | `cases/ac_55.json` | join, leave, add other, hijack (as one and as several ops), op shape; cases of `examples/v0_2/chat/EXPECTED.md` |
| AC-57 | `cases/ac_57.json` | the two cases SPEC 5.0 assigns to this suite: foreign `ReplicaId` taken from a logged op, 300 offline ops all accepted; plus `seq` order |

The compile goldens of AC-54 (`ac_54_call_rule_me_eq_me` and the others of SPEC 12.6) and
of AC-33 (a) belong to the checker goldens under `tests/errors/`; `cases/ac_33.json` only
points to the expected file of (a), so the suite names every part of AC-33.

## Status

There is no server binary yet. `bypass_cases.test.mjs` checks the case data only: every case
is well formed, names only models, fields and functions its app declares unless it expects
exactly that fault, uses only reject reasons, error codes and call errors that
`crates/ostrel_sync/src/protocol.rs` defines, binds every alias before use, and every must
deny case has an allowed control on the same app. Those tests deliberately carry no
`ac_NN_` title: a case is evidence for its criterion only once the runner below sends it to a
built server, so AC-11, AC-32, AC-33, AC-54 and AC-55 stay on the pending list of
`ci/checks/70_ac_coverage.sh` until then.

## Layout

* `apps/teams.ostl`: fixture app for AC-11, AC-31, AC-32 and AC-54 (teams, tasks with an
  owner, a lock, and three server functions: with a `call` rule, without one, elevated).
* `apps/teams_v2.ostl`: the same app after a schema change (field `title` removed, `heading`
  added in its place), for the offline op case of D132.
* `fixtures/NAME.json`: the app of a fixture, its users (or, in an app without `auth`, its
  replicas) and the honest writes that build the state before every case.
* `cases/ac_NN.json`: the cases of one criterion. A file names a default fixture; a case may
  name its own.

Case data is written by hand from SPEC 5 and 12, ARCHITECTURE 5.4 to 5.9 and the decisions
cited in each case, never from server output. If the server disagrees, the server is wrong
until a reviewed change of this directory says otherwise.

## Fixtures

```
{"app": PATH, "users": [HANDLE...], "replicas": [NAME...], "given": [ENTRY...]}
```

Each `given` entry is an honest write by the principal in `as`, sent through the protocol
and expected to be acknowledged: `make` (with `alias` and every declared field, as the
generated client sends them), `put` and `add` (one map key or set element of an aliased
row) or `drop` (an alias). A case may add its own `given` entries after those of the
fixture.

## Cases

| Key | Meaning |
|---|---|
| `name` | test name `ac_NN_<topic>` (SPEC 2 "Test name"), unique in the suite |
| `covers` | the part of the criterion the case belongs to (`bypass_cases.test.mjs`, `REQUIRED`) |
| `kind` | `ops`, `call`, `subscribe`, `session` (handshake: the steps send `Hello` themselves, the runner sends none), `migration` (server restarts with `app_after` between writing and sending the op), `scenario` (honest client behaviour), `compile` |
| `verdict` | `deny` (must be refused) or `allow` (control or honest behaviour) |
| `as` | principal of the session: a user handle, `HANDLE@DEVICE` for a second device of the same user, a replica name in an app without `auth`, or `anonymous` (no identity) |
| `control` | for `deny`: the allowed case on the same app and rule |
| `steps` | frames to send, each with its `expect` list |
| `db` | `"unchanged"`, or `{"expect": TEXT}` for an allowed case and for a deny case marked `partial` (an allowed op in the same batch) |
| `source`, `assumption` | where the expectation comes from, and readings not stated yet (see Open points) |
| `known_red` | why the current code fails this case and which task fixes it; the runner still runs and reports it, it is never skipped or weakened |

A step sends exactly one of:

* `send`: a frame as JSON, with placeholders;
* `send_raw`: the frame text exactly as given (after placeholders), for frames that are not
  valid JSON or repeat a key;
* `send_bytes_hex`: raw bytes, for frames that are not valid UTF-8;
* `send_generated`: a frame the runner builds:
  * `make_with_text`: a `make` whose Text field is padded so that the frame has
    `frame_bytes` bytes, or whose Text value has `text_bytes` bytes;
  * `nested_arrays`: an `ops` frame whose array is nested `depth` levels deep;
  * `remove_with_tags`: a set `remove` naming `tags` distinct op ids of the session;
  * `offline_outbox`: the session goes offline, an honest client queues `ops` makes, comes
    back and drains the outbox in batches of at most the batch limit, resending after every
    `Defer` once `retry_after_ms` has passed.

### Placeholders

A JSON string that starts with `$` is a placeholder; the runner replaces it before sending.

| Placeholder | Value |
|---|---|
| `$op`, `$op+K`, `$op-K` | `OpId` of the session's replica with `seq = next + K` (see below) |
| `$opof:P` | next `OpId` of the replica of principal `P` |
| `$hlc` | a current stamp of the session's replica |
| `$user:H` | row id of the std `User` row of handle `H` |
| `$row:A` | row id bound to alias `A` |
| `$newrow:A` | a fresh row id of the session's replica, bound to alias `A` |
| `$foreignrow:P` | a fresh row id of the replica of principal `P` (exists nowhere) |
| `$tag:A.F.H`, `$tag:A.F.H@D` | add tag of the live element `H` in set field `F` of row `A` (from device `D`) |
| `$replica:P`, `$secret:P` | replica id and `replica_secret` that `Welcome` gave to `P` |
| `$loggedreplica:A` | replica id read from the logged `make` of row `A` as a subscriber received it |

`next` is `last_seq + 1` of the session's replica as the server keeps it (D110): it moves only
when the op at `next` gets a final outcome (`Ack` or a `Reject` other than `Forged`). A
`Defer`, a `Forged` op and a frame refused as a whole do not move it.

### Expectations

The entries of one step are compared as a set, not in order.

| Entry | Meaning |
|---|---|
| `{"ack": OP}` | `Ack` that confirms `OP` |
| `{"reject": REASON, "op": OP}` | `Reject` of `OP` with that reason |
| `{"error": CODE, "fatal": true}` | `Error` without subscription id, then the server closes the connection |
| `{"close": true, "error_if_sent": CODE}` | the server closes the connection; if it sends an `Error` first, it has this code (transport limits may close before the codec runs) |
| `{"return": {"ok": VALUE}}`, `{"return": {"error": KIND}}` | `Return` of the call |
| `{"snapshot": {"rows": [ALIAS...]}}` | the rows the subscription delivers, exactly these, in this order |
| `{"welcome": {"replica": R}}` | `Welcome` that confirms replica `R` |
| `{"all_acked": N, "rejects": 0}` | every one of `N` ops is acknowledged, none rejected |
| `{"expected_file": PATH}` | compile case: compare with the hand written expected file |

## Runner

Not written yet (follows with `ostrel_server`, owner T4 with QA). Its contract:

1. Every case starts from a fresh server of the debug build (AC-12) with an empty database,
   seeded by the fixture and case `given` entries. Principals are development identities.
2. Before the first step the runner snapshots every table of the database: rows, tombstones,
   op log, reject table, replica table and the audit log.
3. Except for kind `session`, it opens a session as `as` (`Hello`, `Welcome`); it sends the
   steps and compares the answers with `expect`. No other answer may arrive for the step.
4. For `"db": "unchanged"` the snapshot after the last step must equal the one before it,
   byte for byte. A denied call writes no audit entry (D140).
5. After a fatal error or close, the runner opens a new session of the same principal and
   runs the case named in `control`: the server keeps serving (AC-34, D132).
6. A case passes only with every step matched and the database check met. The runner names
   its tests by the case `name`.

## Open points

* Decided by D147 (1): in an app with `auth` a `Hello` without a token is a fatal
  `Error Unauthorized`; there is no session, so no op or call can be sent. ARCHITECTURE 5.5
  step 1 (`Reject Denied`) applies only to a session whose token became invalid after
  `Hello`; `examples/v0_2/chat/EXPECTED.md` M-10 stays the rule level case. Cases:
  `ac_11_wrong_principal_no_identity`, `ac_54_elevated_call_without_identity`,
  `ac_55_no_principal`. The consistency test refuses any other expectation for `anonymous`
  in an app with `auth`.
* Decided by D147 (2): a body object with two or more keys, all of them known kinds, is
  `Reject Invalid` for that op only (ARCHITECTURE 5.6, D97); an unknown kind or a body that is
  no object stays a fatal `Error Protocol`. Cases: `ac_11_malformed_body_two_kinds`,
  `ac_55_hijack_one_op_both_directions`, `ac_55_shape_add_and_remove_self` (known red, see
  `known_red`), `ac_11_malformed_body_unknown_kind`.
* Decided by D147 (3): a client `Call` of a `server fn` without `call` rule is `Denied`; an
  unreadable model argument gives `NotFound` first (D99). Case: `ac_32_call_without_call_rule`.
* ASSUMPTION: `last n` reaches the server as the reversed order with `limit n` (the query IR
  has no `last`). Case: `ac_33_e_last_counts_readable_rows`.
* Functions travel by name in `call` (D132); `protocol.rs` still has a numeric `FnId` until
  the `Call` message gets its wire form.
* Not covered yet: an offline op whose field was retyped (D132 names removal and retyping;
  only removal has a case), the `Unavailable` audit failure (owner T4, AC-32 test list).
