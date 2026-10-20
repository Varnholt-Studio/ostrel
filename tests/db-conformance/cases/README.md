# DB conformance cases

Adapter independent cases for the driver contract in `crates/ostrel_db/src/api.rs`. The harness in
`tests/db-conformance/harness/` runs every case against every adapter; `cases.test.mjs` checks the
format and runs every case against a small reference model of the contract, so a wrong
expectation fails in the gate before any adapter exists.

| File | AC | Covers |
|---|---|---|
| `ac_10_replay.json` | AC-10 | final outcomes of ops (D110): replay after a lost ack or a lost reject returns the first outcome, an op id has one outcome only, `last_seq` per replica only grows and rolls back with its transaction, reject reason rules |
| `ac_31_no_upsert.json` | AC-31 | insert of an existing or deleted id, in the same or another model, fails and changes nothing; update and delete never create or resurrect a row |
| `ac_37_injection.json` | AC-37 | SQL and query metacharacters, NUL, U+0001, Unicode edge cases and 1 MiB strings round trip through insert and update in every field; Text order is code point order (D50, D55) |

## Format (version 1)

A file is one JSON object: `format` (1), `ac` (`"AC-NN"`), `schema` and `cases`. Files are ASCII;
every other character is written as a JSON escape.

* `schema`: list of models `{model, name, fields}`; `model` is the `ModelId`, numbered from 0.
  Each field is `{field, name, type, optional}` with `field` the `FieldId` numbered from 0, `type`
  one of `Text`, `Int`, `Bool`, and `optional` false when absent.
* `cases`: list of `{name, doc, steps}`. `name` is the test name `ac_NN_<topic>` (MEASUREMENT 5.1).
  Every case starts on a new, empty database migrated to the schema.
* A step is either a transaction or a query:
  * `{"tx": [write, ...], "expect": "ok"}`: begin, apply the writes in order, commit. Every call
    must succeed.
  * `{"tx": [write, ...], "expect": {"error": "Conflict"}}`: begin, apply the writes in order; one
    of them must fail with that `DbError` variant (`Conflict`, `VersionMismatch` or `NotFound`);
    then roll back.
  * `{"last_seq": "<replica>", "expect": n}`: begin, `Transaction::last_seq` of that replica,
    roll back. The result must be `n`.
  * `{"replay": {"replica", "seq"}, "expect": outcome}`: begin, `Transaction::replay_outcome`,
    roll back. `outcome` is `null` (no final outcome), `{"rejected": "<reason>"}` or
    `{"accepted": {"position": n, "op": op}}` with the log position and the stored op.
  * `{"query": {"model", "order", "limit"}, "expect": [row, ...]}`: `Connection::query` on
    committed state. `order` is a list of `[FieldId, "asc" | "desc"]`, `limit` a number or `null`.
    The result must equal the expected rows in order.
* Writes: `{"insert": {model, row, fields}}`, `{"update": {model, row, expect_version, fields}}`,
  `{"delete": {model, row, expect_version}}`. An insert sets every field of the model. Each write
  is one `apply` call. A transaction may also hold `{"append_ops": [op, ...]}` (one
  `append_ops` call with all ops) and `{"reject": {replica, seq, reason}}` (one
  `record_reject` call). The error of a transaction may also be `Invalid`; its rule text is not
  compared.
* Ops: `{replica, seq, wall, model, row, body}`. `replica` is a `ReplicaId` as a decimal string,
  `seq` a u32, `wall` the wall time in ms (at most 2^48 - 1) of the accepted stamp
  `Hlc(wall, 0, replica)`, `body` a string stored as its UTF-8 bytes.
* Rows: `{id, version, fields}`; the fields of a returned row are compared as a map by `FieldId`.
* `row` and `id` are `RowId` values as decimal strings (u128 does not fit a JSON number). `fields`
  is an object from `FieldId` (as string) to a value.
* Values: `null` (none), `true` or `false`, `{"int": "<decimal>"}` (an `Int` as string, at most
  2^53 - 1 in magnitude, ARCHITECTURE 7.4), `{"text": "..."}`, or
  `{"text_repeat": ["<unit>", count]}` for the unit repeated `count` times.

## Not covered yet

Equality filters on Text (D55) need `Query.filter`, which comes with the query IR; `Float`, time,
bytes, references, `Set` and `Map` values need the `ostrel_core` value type. Both extend these
files when the contract has them.
