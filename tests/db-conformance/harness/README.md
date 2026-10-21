# DB conformance harness

Runs the case files of `tests/db-conformance/cases/` against a driver of the contract in
`crates/ostrel_db/src/api.rs`. A driver conforms when the report has no failures.

## Use from a driver crate

The harness is a Rust module, not a crate, so it needs no workspace entry and no dependency. A
driver crate in `crates/<name>/` includes it from one integration test:

```rust
#[path = "../../../tests/db-conformance/harness/mod.rs"]
mod harness;

use ostrel_db_memory::MemoryDriver;

#[test]
fn ac_18_conformance_memory() {
    let dir = harness::cases_dir();
    let fresh = harness::new_driver_per_case(MemoryDriver::default, "memory:");
    harness::block_on(harness::run_dir(&dir, &fresh)).assert_passed();
}
```

* `run_dir` takes a closure that returns a connection to a new, empty database on every call.
  The harness calls `migrate` with `case_plan()` (a new database to a schema without enum
  columns) and runs one case on it.
* All connections of one driver share a database, so one driver must not serve two cases.
  `new_driver_per_case` builds a new driver for every case and connects it to the URL. A driver
  that can create an empty database per connection (for example a new schema in PostgreSQL)
  may pass its own closure instead.
* `block_on` serves drivers whose futures complete without waiting (in memory, SQLite). It
  panics if a future waits; a driver with real I/O (PostgreSQL) runs `run_dir` on its own
  runtime.
* `cases_dir()` resolves from the including crate's manifest folder, so the driver crate must
  live directly in `crates/`.

## What the harness checks

* Each `tx` step runs as begin, one `apply` call per write, then commit (`"ok"`) or rollback
  (`{"error": ...}`). Writes after the first failing one are not sent. A write that fails when
  `"ok"` is expected, a transaction that succeeds when an error is expected, and a different
  error variant are reported.
* `append_ops` and `reject` items of a `tx` step are one `append_ops` or `record_reject` call
  each, in order with the writes. An expected `Invalid` matches any rule text.
* Every `append_ops` call is checked against the log as the transaction reads it with
  `ops_since` before and after the call: the ops of the call follow the earlier log in order
  with strictly increasing positions, and the returned position is the one of the last op, or
  the last position of the log for a call without ops. A wrong result fails the step and the
  transaction is rolled back.
* A `tx` step with `"rollback"` runs every call, which must succeed, and then rolls back.
* Each `ops_since` step runs begin, `ops_since`, rollback and compares the ops in order; the
  positions must be above the given one and strictly increase, their values are not compared.
* Each `last_seq` step runs begin, `last_seq`, rollback; each `replay` step runs
  begin, `Transaction::replay_outcome`, rollback and compares the whole outcome, for an
  accepted op the position and every field of the stored op.
* Each `query` step runs `Connection::query` on committed state and compares the rows in order:
  id, version, the fields as a map by `FieldId`, and the collections (none in format 1). A returned row must have exactly the fields
  listed in the expectation, so a driver that drops `None` fields fails.
* Failures do not stop the run. A failed connect, migrate, begin, commit or rollback ends that
  case; a wrong result does not.
* The run fails if the folder has no case files, a file is invalid, or a test name repeats
  across files.

## Built in cases

`run_dir` also runs the cases of `run_builtin`, reported under the file name `harness`. They
need a result that depends on the driver, which the fixed expectations of a case file cannot
state.

* `ac_10_commit_error_is_final`, outcome contract of `commit` (D156, D159; doc comment of
  `Transaction::commit`). It appends op A, appends A again (must give `Conflict`) and commits.
  A driver may refuse calls after an error, so the commit may give `Err`; then A must be
  nowhere (no replay outcome, `last_seq` 0, not in the log), also after the next commit on the
  same connection. If the commit gives `Ok`, A must be committed. The memory driver and SQLite
  refuse the commit, so they run the `Err` branch. Commit errors a driver has beyond this one
  (a stale copy in memory, a lost connection in PostgreSQL) are tested by the driver crate.

The case file reader is strict: unknown or missing keys, lone surrogates, duplicate object keys,
values that do not fit the field type and `text_repeat` values over 16 MiB make the file invalid.

## Tests of the harness

`selftest.rs` runs in every test target that includes the harness. It runs small suites against
a reference driver with one injected fault each (upsert, reused tombstone id, missing version
increase, lost NUL, UTF-16 order, rollback that keeps writes, ignored limit, `last_seq` not
raised by `append_ops`, a second outcome for one op, committed positions given again, a log
read in the wrong order, a wrong position returned by `append_ops`, a refused commit that is
visible at once or after the next commit) and checks that exactly the affected cases fail. A driver that leaves
a gap after a rollback, as a PostgreSQL sequence does, must still pass. It also runs the shared
case files against the fault free reference driver, once through the entry shown above.
