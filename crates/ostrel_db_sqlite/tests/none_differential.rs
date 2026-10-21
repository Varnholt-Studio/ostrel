//! Evidence for AC-33 (b), (e) on the SQLite driver: filters with `none` select exactly the
//! rows the two valued reference of `tests/db-conformance/none-differential/` selects (D125).

#[path = "../../../tests/db-conformance/none-differential/mod.rs"]
mod none_differential;

use ostrel_db_sqlite::SqliteDriver;

#[test]
fn ac_33_see_none_differential_sqlite() {
    none_differential::run(&SqliteDriver::new(), "sqlite::memory:");
}
