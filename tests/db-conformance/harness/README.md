# DB conformance harness

Runs the case files of `tests/db-conformance/cases/` against a driver of the contract in
`crates/ostrel_db/src/api.rs`. A driver conforms when the report has no failures.

## Use from a driver crate

The harness is a Rust module, not a crate, so it needs no workspace entry and no dependency. A
driver crate in `crates/<name>/` includes it from one integration test:

```rust
#[path = "../../../tests/db-conformance/harness/mod.rs"]
mod harness;

#[test]
fn ac_18_conformance_memory() {
    let driver = ostrel_db_memory::MemoryDriver::default();
    let run = harness::run_dir(&harness::cases_dir(), &|| driver.connect("memory:"));
    harness::block_on(run).assert_passed();
}
```

* The closure returns a connection to a new, empty database on every call. The harness calls
  `migrate` with an empty plan and runs one case on it.
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
* Each `query` step runs `Connection::query` on committed state and compares the rows in order:
  id, version, and the fields as a map by `FieldId`. A returned row must have exactly the fields
  listed in the expectation, so a driver that drops `None` fields fails.
* Failures do not stop the run. A failed connect, migrate, begin, commit or rollback ends that
  case; a wrong result does not.
* The run fails if the folder has no case files, a file is invalid, or a test name repeats
  across files.

The case file reader is strict: unknown or missing keys, lone surrogates, duplicate object keys,
values that do not fit the field type and `text_repeat` values over 16 MiB make the file invalid.

## Tests of the harness

`selftest.rs` runs in every test target that includes the harness. It runs small suites against
a reference driver with one injected fault each (upsert, reused tombstone id, missing version
increase, lost NUL, UTF-16 order, rollback that keeps writes, ignored limit) and checks that
exactly the affected cases fail. It also runs the shared case files against the fault free
reference driver.
