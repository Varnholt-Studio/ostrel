# Red review T147: SQLite driver (T114) against upsert and resurrection

Subject: `crates/ostrel_db_sqlite` at 5e105a4 (branch `t6/viktor/114-sqlite-driver`, base c4d3d8d).
Criteria: SPEC AC-31 (D20), AC-10 as far as it concerns AC-31, ARCHITECTURE 6.1 and 6.2,
`crates/ostrel_db/src/api/write.rs` (contract of `Write`), `docs/db-mapping.md` 2.4, 2.5 and 4.

Result: **no S1 and no S2.** One S3 (evidence and sign-off guard), one S4 (test gap).

## 1. What AC-31 requires, sentence by sentence

| AC-31 text | Driver at 5e105a4 | Evidence |
|---|---|---|
| "A create operation carrying an ID that exists ... in the same or another model" is rejected | `Insert` runs `INSERT ... ON CONFLICT (id) DO NOTHING` on `ostrel_rows`, whose primary key is the id alone, and returns `Conflict` when no row was added (`store.rs:527-537`) | conformance `ac_31_insert_existing_id_same_model`, `..._other_model`; probe P2 |
| "... or is tombstoned" | `Delete` sets `deleted = 1` and keeps the id row (`store.rs:563`, `bump_version`); the same primary key holds the id | conformance `ac_31_insert_deleted_id_*`; probes P1, P5 |
| "the existing row is unchanged" | the conflict is detected before any field or collection write; `apply` runs in a savepoint that is rolled back on error (`lib.rs:273-293`, `lib.rs:321-331`) | probe P2: fields, version and all adapter tables unchanged after insert, update and delete from a foreign model |
| "a tombstoned row is not resurrected" | `Update` and `Delete` go through `bump_version`, which accepts only `deleted = 0` and the same model, else `NotFound` (`store.rs:588-598`) | conformance `ac_31_update_deleted_row_is_not_upsert`, `ac_31_delete_deleted_row`; probe P1 |
| "The DB interface distinguishes insert (fails on conflict) from update" | `Write::Insert` / `Write::Update` / `Write::Delete` in `ostrel_db::api` | `write.rs:14-43` |
| "rejected by the server" | not part of this task; no server exists at 5e105a4 | see F1 |

## 2. Attacks tried

All probes are in `probes.rs` in this directory and pass on 5e105a4 (5 of 5). Each probe also
counts the rows of every adapter table through a separate `rusqlite` connection, so a write that
the query path would hide (for example an orphan field or set tag of a tombstoned id) is visible.

* **P1** Tombstone in model 1, then from models 1, 2 and 3: `Insert` with a field, a `Set` add and
  a `Map` entry; `Update` with the old and the new version; `Delete`. Expected and observed:
  `Conflict` for every insert, `NotFound` for every update and delete, no live row in any model,
  adapter table counts unchanged. Same after closing and reopening the file.
* **P2** Live row in model 1; insert, update and delete of the same id in model 2. Expected and
  observed: `Conflict`, `NotFound`, `NotFound`; the row in model 1 keeps version 1 and its field.
* **P3** One batch `[Insert m1, Delete m1, Insert m2]` of the same id: `Conflict`, nothing stored,
  the id stays free. The same sequence in three `apply` calls of one transaction: third call
  `Conflict`, `commit` refused, nothing stored. Both ids are insertable afterwards.
* **P4** Two connections to one file: while A holds the write transaction, B cannot begin
  (`Backend` after the busy timeout, documented in `lib.rs:11-13`); after A commits, B's insert of
  the id in another model is `Conflict`; after A deletes it, B's insert is still `Conflict`.
* **P5** Boundary ids 0, 1, 2^64 - 1, 2^64, 2^128 - 1: delete then insert in another model is
  `Conflict` for each, so the 16 byte encoding does not collapse ids.

Mutation probes on `store.rs` (not committed), run against the existing driver test suite only:

| Mutation | Result |
|---|---|
| `bump_version` ignores the model of the stored row | conformance suite red (`ac_31_update_in_other_model_is_not_upsert`) |
| `Insert` resurrects a tombstone: `ON CONFLICT (id) DO UPDATE SET deleted = 0, model = excluded.model, version = 1 WHERE deleted = 1` | conformance suite red (2 cases) |
| as above, but only in the same model | conformance suite red (2 cases) |

The existing tests therefore detect each way of breaking AC-31 at the driver level that I found.

Checked and not a finding:

* Reference hops in queries filter on the hop model (`eval.rs:216-217`), so a live id of another
  model is not read through a reference.
* `Map` entries are written without a stamp comparison. This is the contract (`write.rs:1-6`,
  `docs/db-mapping.md` 3.3: the merge happens before the driver); a stale merge is refused by
  `expect_version`, which every `Update` carries. Element level replay of `Set` adds is ARCHITECTURE
  6.2 "Replay" (D62) and belongs to T146.

## 3. Findings

### F1 (S3): AC-31 and AC-10 leave the sign-off guard although half of their check is missing

* Where: `ci/checks/70_ac_coverage.pending` in 5e105a4 removes `AC-10` and `AC-31`.
* SPEC AC-31, "Check": "crafted request tests in the gate; conformance case ... on every adapter".
  SPEC AC-10, "Check": "... restart integration test".
* Expected: a criterion leaves the pending list once its check is evidenced. The list header says
  "A milestone cannot be signed off while any of its criteria is listed here".
* Actual: the list is a name check (any test named `ac_31_*` counts) and fails when a listed
  criterion gets a test, so T114 had to remove both entries. After the merge of T114 nothing
  mechanical keeps v0.2 from being signed off without the crafted server request test of AC-31
  and the restart test of AC-10. carlo named the gap in his review of T114 (F1, not blocking); it
  has no owner and no task.
* Repro: on 5e105a4, `grep -rn ac_31 --include=*.rs .` finds only adapter tests; no server test.
* Proposal: a task for the crafted `make` with an existing or tombstoned id against
  `ostrel_server` (owner T4 or T124, name `ac_31_crafted_make_existing_id`), and the restart test
  of AC-10, both due before the v0.2 sign-off; or a QA lead decision (#57) that keeps partially
  covered criteria visible in the list. No change to T114 needed.

### F2 (S4): no driver test inserts collections onto a taken id

* Where: `crates/ostrel_db_sqlite/tests/driver.rs:276` and the conformance cases insert without
  collections; the harness cannot express collections (D105).
* Risk: a later change that writes side tables before the id check (for example the per model
  layout of `docs/db-mapping.md` 2.5) would leave orphan set tags or map entries that no current
  test sees, because queries join on live rows.
* Proposal: T70 adds an AC-31 case with a `Set` and a `Map` change to the harness extension it
  already owns under D105. Probe P1 shows the shape.

## 4. Verdict

T114 at 5e105a4 meets AC-31 at the level of the DB interface and the SQLite adapter. No finding
blocks its approval. F1 must be closed before the v0.2 sign-off, independently of T114.
