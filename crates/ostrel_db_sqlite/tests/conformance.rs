//! Evidence for AC-10 (SQLite part) and AC-31 on the SQLite driver.
//!
//! AC-10: the driver passes every case of the adapter independent conformance suite in
//! `tests/db-conformance/`, and the DB interface crate depends on no database driver.
//! AC-31: an insert of an id that exists, or existed and was deleted, fails in every model and
//! changes nothing. Including the harness also runs its own tests (`selftest.rs`).

#[path = "../../../tests/db-conformance/harness/mod.rs"]
mod harness;

use std::collections::{BTreeMap, BTreeSet};

use ostrel_db::api::{DbError, Driver, Query, RowId, Write};
use ostrel_db_sqlite::SqliteDriver;

#[test]
fn ac_10_conformance_sqlite() {
    let dir = harness::cases_dir();
    // Every connection to `sqlite::memory:` is a new, empty database.
    let fresh = harness::new_driver_per_case(SqliteDriver::new, "sqlite::memory:");
    harness::block_on(harness::run_dir(&dir, &fresh)).assert_passed();
}

/// Packages the lockfile lists as dependencies of each package (by name).
fn lock_graph(lock: &str) -> BTreeMap<String, Vec<String>> {
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut name = String::new();
    let mut in_deps = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            in_deps = false;
        } else if let Some(n) = line.strip_prefix("name = ") {
            name = n.trim_matches('"').to_string();
            graph.entry(name.clone()).or_default();
        } else if line == "dependencies = [" {
            in_deps = true;
        } else if in_deps && line == "]" {
            in_deps = false;
        } else if in_deps {
            // `"name"` or `"name version"` or `"name version (source)"`.
            let dep = line.trim_matches(|c| c == '"' || c == ',');
            let dep = dep.split(' ').next().unwrap_or_default().to_string();
            graph.entry(name.clone()).or_default().push(dep);
        }
    }
    graph
}

#[test]
fn ac_10_db_interface_depends_on_no_database_driver() {
    let lock =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock")).unwrap();
    let graph = lock_graph(&lock);
    assert!(
        graph.contains_key("ostrel_db"),
        "ostrel_db not in Cargo.lock"
    );
    assert!(
        graph
            .get("ostrel_db_sqlite")
            .is_some_and(|d| d.iter().any(|x| x == "rusqlite")),
        "the lockfile reader does not see the driver's own dependency"
    );
    let mut seen = BTreeSet::new();
    let mut todo = vec!["ostrel_db".to_string()];
    while let Some(p) = todo.pop() {
        if seen.insert(p.clone()) {
            todo.extend(graph.get(&p).cloned().unwrap_or_default());
        }
    }
    let drivers = [
        "rusqlite",
        "libsqlite3-sys",
        "tokio-postgres",
        "postgres",
        "ostrel_db_sqlite",
        "ostrel_db_postgres",
        "ostrel_db_memory",
    ];
    for d in drivers {
        assert!(!seen.contains(d), "ostrel_db depends on {d}: {seen:?}");
    }
}

fn rid(n: u128) -> RowId {
    RowId::from_hex(&format!("{n:032x}")).unwrap()
}

fn insert(model: u32, row: u128) -> Write {
    Write::Insert {
        model,
        row: rid(row),
        fields: vec![],
        collections: vec![],
    }
}

#[test]
fn ac_31_insert_of_an_existing_or_tombstoned_id_fails_in_every_model() {
    harness::block_on(async {
        let driver = SqliteDriver::new();
        let mut conn = driver.connect("sqlite::memory:").await.unwrap();
        let mut tx = conn.begin().await.unwrap();
        tx.apply(&[insert(1, 1), insert(1, 2)]).await.unwrap();
        let delete = Write::Delete {
            model: 1,
            row: rid(2),
            expect_version: 1,
        };
        tx.apply(&[delete]).await.unwrap();
        tx.commit().await.unwrap();
        let before = conn.query(&Query::all(1)).await.unwrap();
        for (model, row) in [(1, 1), (2, 1), (1, 2), (2, 2)] {
            let mut tx = conn.begin().await.unwrap();
            assert_eq!(
                tx.apply(&[insert(model, row)]).await,
                Err(DbError::Conflict),
                "model {model} row {row}"
            );
            tx.rollback().await.unwrap();
            assert_eq!(conn.query(&Query::all(1)).await.unwrap(), before);
            assert!(conn.query(&Query::all(2)).await.unwrap().0.is_empty());
        }
    });
}
