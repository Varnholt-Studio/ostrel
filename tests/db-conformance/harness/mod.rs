//! Conformance harness for the DB driver contract (`ostrel_db::api`, ARCHITECTURE 6.1).
//!
//! The harness reads every case file in `tests/db-conformance/cases/` (format 1, see the README
//! there), runs each case on a new connection and reports every deviation from the expected
//! results. A driver conforms when the report is empty.
//!
//! It is not a crate of its own. A driver crate includes it from one integration test:
//!
//! ```ignore
//! #[path = "../../../tests/db-conformance/harness/mod.rs"]
//! mod harness;
//!
//! use ostrel_db_memory::MemoryDriver;
//!
//! #[test]
//! fn ac_18_conformance_memory() {
//!     let dir = harness::cases_dir();
//!     let fresh = harness::new_driver_per_case(MemoryDriver::default, "memory:");
//!     harness::block_on(harness::run_dir(&dir, &fresh)).assert_passed();
//! }
//! ```
//!
//! [`run_dir`] takes a [`Fresh`] closure that must return a connection to a new, empty
//! database for every call; the harness migrates it and runs one case on it. Connections of one
//! driver share a database, so [`new_driver_per_case`] builds a new driver for every case. A
//! driver whose futures wait on I/O runs `run_dir` on its own runtime instead of [`block_on`].
//! The tests of the harness itself (`selftest.rs`) run in every test target that includes it.

mod cases;
mod json;
#[cfg(test)]
mod selftest;

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::task::{Context, Poll, Waker};

use ostrel_db::api::{BoxFuture, Connection, DbError, Driver, MigrationPlan, Row, Value};

use cases::{Action, Case, Expect};

/// Opens a connection to a new, empty database.
pub type Fresh<'a> = dyn Fn() -> BoxFuture<'a, Result<Box<dyn Connection>, DbError>> + 'a;

/// A [`Fresh`] closure that builds a new driver with `make` for every case and connects it to
/// `url`. Use it for a driver whose connections share one database, as the in memory driver.
pub fn new_driver_per_case<'a, D: Driver + 'a>(
    make: impl Fn() -> D + 'a,
    url: &'a str,
) -> impl Fn() -> BoxFuture<'a, Result<Box<dyn Connection>, DbError>> + 'a {
    move || {
        let driver = make();
        Box::pin(async move { driver.connect(url).await })
    }
}

/// Longest rendering of a value in a failure message.
const SHOW_MAX: usize = 120;

/// The shared case files, for a driver crate in `crates/<name>/`.
pub fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/db-conformance/cases")
}

/// One deviation from a case file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub file: String,
    /// Test name of the case; empty when the whole file failed.
    pub case: String,
    /// Index of the failing step in the case.
    pub step: usize,
    pub message: String,
}

/// Result of a run. It passes when at least one case ran and nothing failed.
#[derive(Debug, Default)]
pub struct Report {
    /// Case files read, by file name.
    pub files: Vec<String>,
    /// Number of cases run.
    pub cases: usize,
    pub failures: Vec<Failure>,
    names: BTreeSet<String>,
}

impl Report {
    pub fn is_ok(&self) -> bool {
        self.cases > 0 && self.failures.is_empty()
    }

    /// Panics with every failure unless the run passed.
    pub fn assert_passed(&self) {
        assert!(self.is_ok(), "conformance failures\n{self}");
    }

    fn fail(&mut self, file: &str, case: &str, step: usize, message: String) {
        self.failures.push(Failure {
            file: file.to_string(),
            case: case.to_string(),
            step,
            message,
        });
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} cases from {} files, {} failures",
            self.cases,
            self.files.len(),
            self.failures.len()
        )?;
        for x in &self.failures {
            writeln!(f, "  {} {} step {}: {}", x.file, x.case, x.step, x.message)?;
        }
        Ok(())
    }
}

/// Runs every `*.json` file of `dir` in file name order.
pub async fn run_dir(dir: &Path, fresh: &Fresh<'_>) -> Report {
    let mut report = Report::default();
    let shown = dir.display().to_string();
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect(),
        Err(e) => {
            report.fail(&shown, "", 0, format!("cannot read the case folder: {e}"));
            return report;
        }
    };
    paths.sort();
    if paths.is_empty() {
        report.fail(&shown, "", 0, "no case files".to_string());
    }
    for path in paths {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        match std::fs::read_to_string(&path) {
            Ok(text) => run_text(&name, &text, fresh, &mut report).await,
            Err(e) => report.fail(&name, "", 0, format!("cannot read: {e}")),
        }
    }
    report
}

/// Runs the cases of one file and adds the outcome to `report`.
pub async fn run_text(file: &str, text: &str, fresh: &Fresh<'_>, report: &mut Report) {
    report.files.push(file.to_string());
    let suite = match json::parse(text)
        .map_err(|e| e.to_string())
        .and_then(|j| cases::decode(&j))
    {
        Ok(suite) => suite,
        Err(e) => return report.fail(file, "", 0, format!("invalid case file: {e}")),
    };
    for case in &suite.cases {
        if !report.names.insert(case.name.clone()) {
            report.fail(file, &case.name, 0, "test name used twice".to_string());
            continue;
        }
        report.cases += 1;
        for (step, message) in run_case(case, fresh).await {
            report.fail(file, &case.name, step, message);
        }
    }
}

/// Runs one case on a new database. Returns the failures as (step, message). A failure of the
/// connection or transaction machinery ends the case; a wrong result does not.
async fn run_case(case: &Case, fresh: &Fresh<'_>) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut conn = match fresh().await {
        Ok(c) => c,
        Err(e) => return vec![(0, format!("connect failed with {e:?}"))],
    };
    if let Err(e) = conn.migrate(&MigrationPlan::default()).await {
        return vec![(0, format!("migrate failed with {e:?}"))];
    }
    for (i, step) in case.steps.iter().enumerate() {
        let result = match &step.action {
            Action::Tx(writes) => run_tx(conn.as_mut(), writes, &step.expect).await,
            Action::Query(q) => match (conn.query(q).await, &step.expect) {
                (Ok(rows), Expect::Rows(want)) => Ok(compare_rows(&rows.0, want)),
                (Err(e), _) => Ok(Some(format!("query failed with {e:?}"))),
                (Ok(_), _) => Ok(Some("a query expects rows".to_string())),
            },
        };
        match result {
            Ok(None) => {}
            Ok(Some(message)) => out.push((i, message)),
            Err(message) => {
                out.push((i, message));
                break;
            }
        }
    }
    out
}

/// Runs one transaction step. `Ok(Some)` is a wrong result, `Err` a failure that ends the case.
async fn run_tx(
    conn: &mut dyn Connection,
    writes: &[ostrel_db::api::Write],
    expect: &Expect,
) -> Result<Option<String>, String> {
    let mut tx = conn
        .begin()
        .await
        .map_err(|e| format!("begin failed with {e:?}"))?;
    let mut failed = None;
    for (j, w) in writes.iter().enumerate() {
        if let Err(e) = tx.apply(std::slice::from_ref(w)).await {
            failed = Some((j, e));
            break;
        }
    }
    let verdict = match (expect, &failed) {
        (Expect::Ok, None) => {
            tx.commit()
                .await
                .map_err(|e| format!("commit failed with {e:?}"))?;
            return Ok(None);
        }
        (Expect::Ok, Some((j, e))) => Some(format!("write {j} failed with {e:?}")),
        (Expect::Error(want), Some((_, e))) if e == want => None,
        (Expect::Error(want), Some((j, e))) => {
            Some(format!("expected {want:?}, write {j} failed with {e:?}"))
        }
        (Expect::Error(want), None) => Some(format!("expected {want:?}, every write succeeded")),
        (Expect::Rows(_), _) => Some("a transaction expects \"ok\" or an error".to_string()),
    };
    tx.rollback()
        .await
        .map_err(|e| format!("rollback failed with {e:?}"))?;
    Ok(verdict)
}

/// Compares returned rows with the expected rows in order; fields as a map by `FieldId`.
fn compare_rows(got: &[Row], want: &[Row]) -> Option<String> {
    if got.len() != want.len() {
        let ids: Vec<u128> = got.iter().map(|r| r.id.0).collect();
        return Some(format!(
            "expected {} rows, got {} (ids {})",
            want.len(),
            got.len(),
            show(&format!("{ids:?}"))
        ));
    }
    for (k, (g, w)) in got.iter().zip(want).enumerate() {
        if g.id != w.id || g.version != w.version {
            return Some(format!(
                "row {k}: expected id {} version {}, got id {} version {}",
                w.id.0, w.version, g.id.0, g.version
            ));
        }
        let mut seen = BTreeSet::new();
        if let Some((f, _)) = g.fields.iter().find(|(f, _)| !seen.insert(*f)) {
            return Some(format!("row {k}: field {f} returned twice"));
        }
        let mut gf: Vec<&(u32, Value)> = g.fields.iter().collect();
        gf.sort_by_key(|(f, _)| *f);
        // Expected fields are sorted by the decoder.
        if gf.len() != w.fields.len() || gf.iter().zip(&w.fields).any(|(a, b)| *a != b) {
            return Some(format!(
                "row {k} (id {}): expected fields {}, got {}",
                w.id.0,
                show(&format!("{:?}", w.fields)),
                show(&format!("{gf:?}"))
            ));
        }
    }
    None
}

/// Shortens a rendering for a failure message.
fn show(s: &str) -> String {
    match s.char_indices().nth(SHOW_MAX) {
        Some((cut, _)) => format!("{}... ({} bytes)", &s[..cut], s.len()),
        None => s.to_string(),
    }
}

/// Runs a future whose every await completes at once, as with the in memory driver.
///
/// # Panics
///
/// If the future waits: such a driver needs its own runtime.
pub fn block_on<T>(f: impl Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("future waited; run the suite on the driver's runtime"),
    }
}
