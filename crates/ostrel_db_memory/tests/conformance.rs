//! The adapter independent conformance cases of `tests/db-conformance/` against the in memory
//! driver. Including the harness here also runs its own tests (`selftest.rs`). The evidence
//! test for AC-18 comes with the adapter guide (T71).

#[path = "../../../tests/db-conformance/harness/mod.rs"]
mod harness;

use ostrel_db_memory::MemoryDriver;

#[test]
fn shared_conformance_cases_pass_on_the_memory_driver() {
    let dir = harness::cases_dir();
    let fresh = harness::new_driver_per_case(MemoryDriver::default, "memory:");
    harness::block_on(harness::run_dir(&dir, &fresh)).assert_passed();
}
