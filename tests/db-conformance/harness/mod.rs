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
//! database for every call; the harness migrates it with [`case_plan`] and runs one case on it. Connections of one
//! driver share a database, so [`new_driver_per_case`] builds a new driver for every case. A
//! driver whose futures wait on I/O runs `run_dir` on its own runtime instead of [`block_on`].
//! The tests of the harness itself (`selftest.rs`) run in every test target that includes it.
//!
//! Besides the case files, `run_dir` runs the built in cases of [`run_builtin`], which a case
//! file cannot state, so every driver that includes the harness runs them too.

mod cases;
mod json;
#[cfg(test)]
mod selftest;

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::task::{Context, Poll, Waker};

use ostrel_db::api::{
    BoxFuture, Connection, DbError, Driver, Hlc, MigrationPlan, NewOp, OpId, ReplayOutcome,
    ReplicaId, Row, RowId, SchemaHash, ServerSeq, StoredOp, Transaction, Value,
};

use cases::{Action, Call, Case, Expect};

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
    run_builtin(fresh, &mut report).await;
    report
}

/// File name under which the built in cases report.
pub const BUILTIN_FILE: &str = "harness";

/// Test name of the built in case for the outcome contract of `commit`.
pub const COMMIT_CASE: &str = "ac_10_commit_error_is_final";

/// Runs the built in cases and adds them to `report` under [`BUILTIN_FILE`]. They need a
/// result that depends on the driver (whether a commit is refused), which the fixed
/// expectations of a case file cannot state.
pub async fn run_builtin(fresh: &Fresh<'_>, report: &mut Report) {
    if !report.names.insert(COMMIT_CASE.to_string()) {
        return report.fail(
            BUILTIN_FILE,
            COMMIT_CASE,
            0,
            "test name used twice".to_string(),
        );
    }
    report.cases += 1;
    for (step, message) in commit_error_is_final(fresh).await {
        report.fail(BUILTIN_FILE, COMMIT_CASE, step, message);
    }
}

/// An op of the built in cases.
fn builtin_op(replica: u64) -> NewOp {
    NewOp {
        id: OpId {
            replica: ReplicaId(replica),
            seq: 1,
        },
        hlc: Hlc::new(1, 0, ReplicaId(replica)).expect("valid stamp"),
        model: 0,
        row: RowId::from_hex(&format!("{replica:032x}")).expect("valid row id"),
        body: b"{}".to_vec(),
    }
}

/// Outcome contract of `commit` (D156, D159): `Ok` means committed, and a transaction whose
/// commit returned `Err` never becomes visible, neither at once nor after a later commit on the
/// same connection.
///
/// The fault every driver can show through the trait is a commit after a failed call: the
/// module rules let a driver refuse every call after an error, and a driver that refuses the
/// commit must leave nothing of the transaction behind. A driver that commits the calls that
/// succeeded passes too, but then they must be visible. Step 0 appends op A, appends A again
/// (which must give `Conflict`) and commits; steps 1 and 3 read the outcome of A, step 2
/// commits op B on the same connection.
async fn commit_error_is_final(fresh: &Fresh<'_>) -> Vec<(usize, String)> {
    let mut conn = match fresh().await {
        Ok(c) => c,
        Err(e) => return vec![(0, format!("connect failed with {e:?}"))],
    };
    if let Err(e) = conn.migrate(&case_plan()).await {
        return vec![(0, format!("migrate failed with {e:?}"))];
    }
    let (a, b) = (builtin_op(1), builtin_op(2));
    let committed = {
        let mut tx = match conn.begin().await {
            Ok(tx) => tx,
            Err(e) => return vec![(0, format!("begin failed with {e:?}"))],
        };
        let first = tx.append_ops(std::slice::from_ref(&a)).await;
        let again = tx.append_ops(std::slice::from_ref(&a)).await;
        if first.is_err() || again != Err(DbError::Conflict) {
            let _ = tx.rollback().await;
            return vec![(
                0,
                format!("expected a position, then Conflict, got {first:?}, then {again:?}"),
            )];
        }
        tx.commit().await.is_ok()
    };
    let mut out = Vec::new();
    if let Some(m) = check_final(conn.as_mut(), &a, committed).await {
        out.push((1, m));
    }
    if let Err(m) = commit_one(conn.as_mut(), &b).await {
        out.push((2, m));
        return out;
    }
    if let Some(m) = check_final(conn.as_mut(), &a, committed).await {
        out.push((3, m));
    }
    out
}

/// Appends `op` in a transaction of its own and commits it.
async fn commit_one(conn: &mut dyn Connection, op: &NewOp) -> Result<(), String> {
    let mut tx = conn
        .begin()
        .await
        .map_err(|e| format!("begin failed with {e:?}"))?;
    if let Err(e) = tx.append_ops(std::slice::from_ref(op)).await {
        let _ = tx.rollback().await;
        return Err(format!("append_ops failed with {e:?}"));
    }
    tx.commit()
        .await
        .map_err(|e| format!("commit failed with {e:?}"))
}

/// Checks that `op` is in the log with its outcome and `last_seq` if its commit returned `Ok`,
/// and nowhere if it returned `Err`.
async fn check_final(conn: &mut dyn Connection, op: &NewOp, committed: bool) -> Option<String> {
    let mut tx = match conn.begin().await {
        Ok(tx) => tx,
        Err(e) => return Some(format!("begin failed with {e:?}")),
    };
    let outcome = tx.replay_outcome(op.id).await;
    let last = tx.last_seq(op.id.replica).await;
    let log = tx.ops_since(ServerSeq(0), LOG_READ_MAX).await;
    let _ = tx.rollback().await;
    let (outcome, last, log) = match (outcome, last, log) {
        (Ok(o), Ok(l), Ok(g)) => (o, l, g),
        (o, l, g) => return Some(format!("reads failed: {o:?}, {l:?}, {g:?}")),
    };
    let in_log = log.iter().filter(|s| s.op.id == op.id).count();
    let accepted = matches!(&outcome, Some(ReplayOutcome::Accepted(s)) if s.op == *op);
    if committed && !(accepted && in_log == 1 && last == op.id.seq) {
        return Some(format!(
            "commit returned Ok, but the op is not committed: outcome {}, {in_log} times in \
             the log, last_seq {last}",
            show(&format!("{outcome:?}"))
        ));
    }
    if !committed && (outcome.is_some() || in_log > 0 || last != 0) {
        return Some(format!(
            "commit returned Err, but the op is visible: outcome {}, {in_log} times in the \
             log, last_seq {last}",
            show(&format!("{outcome:?}"))
        ));
    }
    None
}

/// The plan every case database is migrated with: from a new database to a schema without
/// enum columns. The case files need nothing else from the schema.
pub fn case_plan() -> MigrationPlan {
    MigrationPlan {
        from: None,
        to: SchemaHash([0; 32]),
        schema: "{}".to_string(),
        enums: Vec::new(),
        steps: Vec::new(),
    }
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
    if let Err(e) = conn.migrate(&case_plan()).await {
        return vec![(0, format!("migrate failed with {e:?}"))];
    }
    for (i, step) in case.steps.iter().enumerate() {
        let result = match &step.action {
            Action::Tx(calls) => run_tx(conn.as_mut(), calls, &step.expect).await,
            Action::LastSeq(replica) => run_last_seq(conn.as_mut(), *replica, &step.expect).await,
            Action::Replay(op) => run_replay(conn.as_mut(), *op, &step.expect).await,
            Action::OpsSince(after) => run_ops_since(conn.as_mut(), *after, &step.expect).await,
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

/// Same variant; the rule text of [`DbError::Invalid`] differs between drivers.
fn same_error(got: &DbError, want: &DbError) -> bool {
    match (got, want) {
        (DbError::Invalid(_), DbError::Invalid(_)) => true,
        _ => got == want,
    }
}

/// Runs one transaction step. `Ok(Some)` is a wrong result, `Err` a failure that ends the case.
async fn run_tx(
    conn: &mut dyn Connection,
    calls: &[Call],
    expect: &Expect,
) -> Result<Option<String>, String> {
    let mut tx = conn
        .begin()
        .await
        .map_err(|e| format!("begin failed with {e:?}"))?;
    let mut failed = None;
    let mut wrong = None;
    for (j, call) in calls.iter().enumerate() {
        let result = match call {
            Call::Write(w) => tx.apply(std::slice::from_ref(w)).await,
            Call::AppendOps(ops) => match append_checked(tx.as_mut(), ops).await {
                Ok(None) => Ok(()),
                Ok(Some(m)) => {
                    wrong = Some(format!("call {j}: {m}"));
                    break;
                }
                Err(e) => Err(e),
            },
            Call::Reject(op, reason) => tx.record_reject(*op, reason).await,
        };
        if let Err(e) = result {
            failed = Some((j, e));
            break;
        }
    }
    let verdict = match (expect, &failed) {
        _ if wrong.is_some() => wrong,
        (Expect::Ok, None) => {
            tx.commit()
                .await
                .map_err(|e| format!("commit failed with {e:?}"))?;
            return Ok(None);
        }
        (Expect::Ok | Expect::Rollback, Some((j, e))) => {
            Some(format!("write {j} failed with {e:?}"))
        }
        (Expect::Rollback, None) => None,
        (Expect::Error(want), Some((_, e))) if same_error(e, want) => None,
        (Expect::Error(want), Some((j, e))) => {
            Some(format!("expected {want:?}, write {j} failed with {e:?}"))
        }
        (Expect::Error(want), None) => Some(format!("expected {want:?}, every write succeeded")),
        (_, _) => Some("a transaction expects \"ok\", \"rollback\" or an error".to_string()),
    };
    tx.rollback()
        .await
        .map_err(|e| format!("rollback failed with {e:?}"))?;
    Ok(verdict)
}

/// Reads `last_seq` in a transaction of its own and rolls it back.
async fn run_last_seq(
    conn: &mut dyn Connection,
    replica: ostrel_db::api::ReplicaId,
    expect: &Expect,
) -> Result<Option<String>, String> {
    let mut tx = conn
        .begin()
        .await
        .map_err(|e| format!("begin failed with {e:?}"))?;
    let verdict = match (tx.last_seq(replica).await, expect) {
        (Ok(got), Expect::LastSeq(want)) if got == *want => None,
        (Ok(got), Expect::LastSeq(want)) => Some(format!("expected last_seq {want}, got {got}")),
        (Err(e), _) => Some(format!("last_seq failed with {e:?}")),
        (Ok(_), _) => Some("a last_seq step expects a number".to_string()),
    };
    tx.rollback()
        .await
        .map_err(|e| format!("rollback failed with {e:?}"))?;
    Ok(verdict)
}

/// Runs `Transaction::replay_outcome` in a transaction that is rolled back, so it reads the
/// committed outcomes.
async fn run_replay(
    conn: &mut dyn Connection,
    op: ostrel_db::api::OpId,
    expect: &Expect,
) -> Result<Option<String>, String> {
    let mut tx = conn
        .begin()
        .await
        .map_err(|e| format!("begin failed with {e:?}"))?;
    let verdict = match (tx.replay_outcome(op).await, expect) {
        (Ok(got), Expect::Outcome(want)) if got == *want => None,
        (Ok(got), Expect::Outcome(want)) => Some(format!(
            "expected outcome {}, got {}",
            show(&format!("{want:?}")),
            show(&format!("{got:?}"))
        )),
        (Err(e), _) => Some(format!("replay_outcome failed with {e:?}")),
        (Ok(_), _) => Some("a replay expects an outcome".to_string()),
    };
    tx.rollback()
        .await
        .map_err(|e| format!("rollback failed with {e:?}"))?;
    Ok(verdict)
}

/// Calls `append_ops` and checks its result against the log as the transaction reads it before
/// and after the call: the ops of the call follow the earlier log in order, with positions that
/// strictly increase, and the result is the position of the last of them, or the last position
/// of the log for a call without ops. `Ok(Some)` is a wrong result, `Err` the error of
/// `append_ops`. The log of a case stays far below [`LOG_READ_MAX`].
async fn append_checked(
    tx: &mut (dyn Transaction<'_> + '_),
    ops: &[NewOp],
) -> Result<Option<String>, DbError> {
    let head = match tx.ops_since(ServerSeq(0), LOG_READ_MAX).await {
        Ok(log) => log.iter().map(|o| o.seq).max().unwrap_or(ServerSeq(0)),
        Err(e) => {
            return Ok(Some(format!(
                "ops_since before append_ops failed with {e:?}"
            )));
        }
    };
    let got = tx.append_ops(ops).await?;
    let added = match tx.ops_since(head, LOG_READ_MAX).await {
        Ok(added) => added,
        Err(e) => {
            return Ok(Some(format!(
                "ops_since after append_ops failed with {e:?}"
            )));
        }
    };
    if let Some(m) = compare_log(head, &added, ops) {
        return Ok(Some(format!("log after append_ops: {m}")));
    }
    let want = added.last().map_or(head, |o| o.seq);
    Ok((got != want).then(|| {
        format!(
            "append_ops returned position {}, but the log ends at {}",
            got.0, want.0
        )
    }))
}

/// Most ops an `ops_since` step reads. A case file holds far fewer, so a driver that returns
/// more than the expected ops is caught without reading an unbounded log.
const LOG_READ_MAX: u32 = 1000;

/// Runs `Transaction::ops_since` in a transaction that is rolled back and compares the ops in
/// order. Positions must be above `after` and strictly increasing; their values are not
/// compared, because a position of a rolled back transaction may be given again or be left out
/// (the position contract of `append_ops`, D154).
async fn run_ops_since(
    conn: &mut dyn Connection,
    after: ostrel_db::api::ServerSeq,
    expect: &Expect,
) -> Result<Option<String>, String> {
    let mut tx = conn
        .begin()
        .await
        .map_err(|e| format!("begin failed with {e:?}"))?;
    let verdict = match (tx.ops_since(after, LOG_READ_MAX).await, expect) {
        (Ok(got), Expect::Log(want)) => compare_log(after, &got, want),
        (Err(e), _) => Some(format!("ops_since failed with {e:?}")),
        (Ok(_), _) => Some("an ops_since step expects a list of ops".to_string()),
    };
    tx.rollback()
        .await
        .map_err(|e| format!("rollback failed with {e:?}"))?;
    Ok(verdict)
}

fn compare_log(
    after: ostrel_db::api::ServerSeq,
    got: &[StoredOp],
    want: &[NewOp],
) -> Option<String> {
    let positions: Vec<u64> = got.iter().map(|o| o.seq.0).collect();
    let ids: Vec<String> = got
        .iter()
        .map(|o| format!("{}/{}", o.op.id.replica.0, o.op.id.seq))
        .collect();
    let mut last = after.0;
    for (k, o) in got.iter().enumerate() {
        if o.seq.0 <= last {
            return Some(format!(
                "op {k}: position {} is not above {last} (positions {})",
                o.seq.0,
                show(&format!("{positions:?}"))
            ));
        }
        last = o.seq.0;
    }
    if got.len() != want.len() {
        return Some(format!(
            "expected {} ops, got {} (ops {})",
            want.len(),
            got.len(),
            show(&format!("{ids:?}"))
        ));
    }
    for (k, (g, w)) in got.iter().zip(want).enumerate() {
        if g.op != *w {
            return Some(format!(
                "op {k}: expected {}, got {}",
                show(&format!("{w:?}")),
                show(&format!("{:?}", g.op))
            ));
        }
    }
    None
}

/// Compares returned rows with the expected rows in order; fields as a map by `FieldId`.
fn compare_rows(got: &[Row], want: &[Row]) -> Option<String> {
    if got.len() != want.len() {
        let ids: Vec<u128> = got.iter().map(|r| r.id.as_u128()).collect();
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
                w.id.as_u128(),
                w.version,
                g.id.as_u128(),
                g.version
            ));
        }
        if g.collections != w.collections {
            return Some(format!(
                "row {k} (id {}): expected collections {}, got {}",
                w.id.as_u128(),
                show(&format!("{:?}", w.collections)),
                show(&format!("{:?}", g.collections))
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
                w.id.as_u128(),
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
