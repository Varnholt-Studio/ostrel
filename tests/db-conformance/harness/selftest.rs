//! Tests of the harness itself. They run in every test target that includes the harness.
//!
//! A conformance harness that cannot fail proves nothing, so most tests here run small suites
//! against a reference driver with one injected fault each and check that the harness reports
//! exactly the case that the fault breaks.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use ostrel_db::api::{
    BoxFuture, Capabilities, Connection, DbError, Dir, Driver, FieldId, MigrationPlan, ModelId,
    Query, Row, RowId, Rows, ServerSeq, StoredOp, Transaction, Value, Write,
};

use super::cases::{self, Expect};
use super::json::{self, Json};
use super::{Report, block_on, new_driver_per_case, run_dir, run_text};

// ---------------------------------------------------------------------------------------------
// Reference driver with injectable faults
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Faults {
    /// Insert of an existing live id overwrites it.
    upsert: bool,
    /// Insert of a deleted id succeeds.
    reuse_tombstone: bool,
    /// Update does not increase the version.
    no_version_bump: bool,
    /// Text loses U+0000 on the way in.
    strip_nul: bool,
    /// Text is ordered by UTF-16 code units instead of code points.
    utf16_order: bool,
    /// Rollback keeps the writes made before the failing one.
    rollback_commits: bool,
    /// Query ignores the limit.
    ignore_limit: bool,
}

#[derive(Clone)]
struct Stored {
    model: ModelId,
    version: u64,
    deleted: bool,
    fields: BTreeMap<FieldId, Value>,
}

type State = BTreeMap<RowId, Stored>;

/// A connection. Connections opened by one [`RefDriver`] share its state, as the connections of
/// a real driver share one database.
struct RefConn {
    faults: Faults,
    state: Rc<RefCell<State>>,
}

struct RefTx<'c> {
    conn: &'c mut RefConn,
    work: State,
}

fn fresh_ref(faults: Faults) -> BoxFuture<'static, Result<Box<dyn Connection>, DbError>> {
    Box::pin(async move {
        Ok(Box::new(RefConn {
            faults,
            state: Rc::default(),
        }) as Box<dyn Connection>)
    })
}

/// Reference driver: one database, shared by every connection it opens.
#[derive(Default)]
struct RefDriver {
    state: Rc<RefCell<State>>,
}

impl Driver for RefDriver {
    fn name(&self) -> &'static str {
        "ref"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transactions: true,
            sequences: false,
            subqueries: false,
            json_fields: false,
            max_in_list: 1000,
        }
    }
    fn connect<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Box<dyn Connection>, DbError>> {
        Box::pin(async move {
            if url != "ref:" {
                return Err(DbError::Unsupported("url"));
            }
            Ok(Box::new(RefConn {
                faults: Faults::default(),
                state: Rc::clone(&self.state),
            }) as Box<dyn Connection>)
        })
    }
}

fn cmp_value(a: &Value, b: &Value, utf16: bool) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Text(x), Value::Text(y)) if utf16 => x.encode_utf16().cmp(y.encode_utf16()),
        (Value::Text(x), Value::Text(y)) => x.cmp(y),
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Less,
        (_, Value::Null) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    }
}

fn scan(state: &State, q: &Query, faults: Faults) -> Rows {
    let mut rows: Vec<Row> = state
        .iter()
        .filter(|(_, r)| r.model == q.model && !r.deleted)
        .map(|(id, r)| Row {
            id: *id,
            version: r.version,
            fields: r.fields.iter().map(|(f, v)| (*f, v.clone())).collect(),
        })
        .collect();
    let get = |r: &Row, f: FieldId| {
        r.fields
            .iter()
            .find(|(k, _)| *k == f)
            .map_or(Value::Null, |(_, v)| v.clone())
    };
    let tie = q.order.last().map_or(Dir::Asc, |(_, d)| *d);
    rows.sort_by(|a, b| {
        for (f, dir) in &q.order {
            let o = cmp_value(&get(a, *f), &get(b, *f), faults.utf16_order);
            let o = if *dir == Dir::Desc { o.reverse() } else { o };
            if o.is_ne() {
                return o;
            }
        }
        if tie == Dir::Desc {
            b.id.cmp(&a.id)
        } else {
            a.id.cmp(&b.id)
        }
    });
    if !faults.ignore_limit {
        rows.truncate(q.limit.map_or(usize::MAX, |n| n as usize));
    }
    Rows(rows)
}

fn store(v: &Value, faults: Faults) -> Value {
    match v {
        Value::Text(s) if faults.strip_nul => Value::Text(s.replace('\0', "")),
        other => other.clone(),
    }
}

fn apply_one(state: &mut State, w: &Write, faults: Faults) -> Result<(), DbError> {
    match w {
        Write::Insert { model, row, fields } => {
            if let Some(old) = state.get(row) {
                let allowed = if old.deleted {
                    faults.reuse_tombstone
                } else {
                    faults.upsert
                };
                if !allowed {
                    return Err(DbError::Conflict);
                }
            }
            let fields = fields.iter().map(|(f, v)| (*f, store(v, faults))).collect();
            let stored = Stored {
                model: *model,
                version: 1,
                deleted: false,
                fields,
            };
            state.insert(*row, stored);
        }
        Write::Update {
            model,
            row,
            expect_version,
            fields,
        } => {
            let r = live(state, *model, *row)?;
            if r.version != *expect_version {
                return Err(DbError::VersionMismatch);
            }
            if !faults.no_version_bump {
                r.version += 1;
            }
            for (f, v) in fields {
                r.fields.insert(*f, store(v, faults));
            }
        }
        Write::Delete {
            model,
            row,
            expect_version,
        } => {
            let r = live(state, *model, *row)?;
            if r.version != *expect_version {
                return Err(DbError::VersionMismatch);
            }
            r.version += 1;
            r.deleted = true;
            r.fields.clear();
        }
    }
    Ok(())
}

fn live(state: &mut State, model: ModelId, row: RowId) -> Result<&mut Stored, DbError> {
    state
        .get_mut(&row)
        .filter(|r| r.model == model && !r.deleted)
        .ok_or(DbError::NotFound)
}

impl Connection for RefConn {
    fn migrate<'a>(&'a mut self, _p: &'a MigrationPlan) -> BoxFuture<'a, Result<(), DbError>> {
        Box::pin(async { Ok(()) })
    }
    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move { Ok(scan(&self.state.borrow(), q, self.faults)) })
    }
    fn begin<'c>(&'c mut self) -> BoxFuture<'c, Result<Box<dyn Transaction<'c> + 'c>, DbError>> {
        Box::pin(async move {
            let work = self.state.borrow().clone();
            Ok(Box::new(RefTx { conn: self, work }) as Box<dyn Transaction<'c> + 'c>)
        })
    }
}

impl<'c> Transaction<'c> for RefTx<'c> {
    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move { Ok(scan(&self.work, q, self.conn.faults)) })
    }
    fn apply<'a>(&'a mut self, ws: &'a [Write]) -> BoxFuture<'a, Result<(), DbError>> {
        let faults = self.conn.faults;
        Box::pin(async move {
            ws.iter()
                .try_for_each(|w| apply_one(&mut self.work, w, faults))
        })
    }
    fn append_ops<'a>(
        &'a mut self,
        _ops: &'a [StoredOp],
    ) -> BoxFuture<'a, Result<ServerSeq, DbError>> {
        Box::pin(async { Err(DbError::Unsupported("op log")) })
    }
    fn ops_since<'a>(
        &'a mut self,
        _after: ServerSeq,
        _limit: u32,
    ) -> BoxFuture<'a, Result<Vec<StoredOp>, DbError>> {
        Box::pin(async { Err(DbError::Unsupported("op log")) })
    }
    fn next_in_sequence<'a>(&'a mut self, _key: &'a str) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async { Err(DbError::Unsupported("sequences")) })
    }
    fn commit(self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>> {
        Box::pin(async move {
            let tx = *self;
            *tx.conn.state.borrow_mut() = tx.work;
            Ok(())
        })
    }
    fn rollback(self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>> {
        Box::pin(async move {
            let tx = *self;
            if tx.conn.faults.rollback_commits {
                *tx.conn.state.borrow_mut() = tx.work;
            }
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// A small suite in format 1 that a correct driver passes. Each case targets one rule, so a
/// fault in the reference driver breaks a known set of cases.
const SUITE: &str = r#"{
  "format": 1,
  "ac": "AC-00",
  "schema": [
    {"model": 0, "name": "Note", "fields": [
      {"field": 0, "name": "title", "type": "Text"},
      {"field": 1, "name": "rank", "type": "Int"},
      {"field": 2, "name": "done", "type": "Bool", "optional": true}
    ]},
    {"model": 1, "name": "Tag", "fields": [{"field": 0, "name": "label", "type": "Text"}]}
  ],
  "cases": [
    {"name": "ac_00_insert_existing", "doc": "Insert of a live id fails.", "steps": [
      {"tx": [{"insert": {"model": 0, "row": "1",
        "fields": {"0": {"text": "a"}, "1": {"int": "1"}, "2": null}}}], "expect": "ok"},
      {"tx": [{"insert": {"model": 1, "row": "1", "fields": {"0": {"text": "b"}}}}],
        "expect": {"error": "Conflict"}},
      {"query": {"model": 0, "order": [], "limit": null}, "expect": [
        {"id": "1", "version": 1, "fields": {"0": {"text": "a"}, "1": {"int": "1"}, "2": null}}]}
    ]},
    {"name": "ac_00_insert_tombstone", "doc": "Insert of a deleted id fails.", "steps": [
      {"tx": [{"insert": {"model": 1, "row": "7", "fields": {"0": {"text": "x"}}}},
              {"delete": {"model": 1, "row": "7", "expect_version": 1}}], "expect": "ok"},
      {"tx": [{"insert": {"model": 1, "row": "7", "fields": {"0": {"text": "y"}}}}],
        "expect": {"error": "Conflict"}},
      {"query": {"model": 1, "order": [], "limit": null}, "expect": []}
    ]},
    {"name": "ac_00_update_version", "doc": "Update increases the version.", "steps": [
      {"tx": [{"insert": {"model": 1, "row": "2", "fields": {"0": {"text": "x"}}}}],
        "expect": "ok"},
      {"tx": [{"update": {"model": 1, "row": "2", "expect_version": 1,
        "fields": {"0": {"text": "z"}}}}], "expect": "ok"},
      {"tx": [{"update": {"model": 1, "row": "2", "expect_version": 1, "fields": {}}}],
        "expect": {"error": "VersionMismatch"}},
      {"query": {"model": 1, "order": [], "limit": null}, "expect": [
        {"id": "2", "version": 2, "fields": {"0": {"text": "z"}}}]}
    ]},
    {"name": "ac_00_failed_tx_changes_nothing", "doc": "A failing write rolls back.", "steps": [
      {"tx": [{"insert": {"model": 1, "row": "3", "fields": {"0": {"text": "x"}}}},
              {"delete": {"model": 1, "row": "4", "expect_version": 1}}],
        "expect": {"error": "NotFound"}},
      {"query": {"model": 1, "order": [], "limit": null}, "expect": []}
    ]},
    {"name": "ac_00_nul_round_trip", "doc": "NUL and U+0001 round trip.", "steps": [
      {"tx": [{"insert": {"model": 1, "row": "5", "fields": {"0": {"text": "a\u0000b\u0001"}}}}],
        "expect": "ok"},
      {"query": {"model": 1, "order": [], "limit": null}, "expect": [
        {"id": "5", "version": 1, "fields": {"0": {"text": "a\u0000b\u0001"}}}]}
    ]},
    {"name": "ac_00_code_point_order", "doc": "U+FF21 sorts before U+1F600.", "steps": [
      {"tx": [{"insert": {"model": 1, "row": "8", "fields": {"0": {"text": "😀"}}}},
              {"insert": {"model": 1, "row": "9", "fields": {"0": {"text": "Ａ"}}}}],
        "expect": "ok"},
      {"query": {"model": 1, "order": [[0, "asc"]], "limit": 1}, "expect": [
        {"id": "9", "version": 1, "fields": {"0": {"text": "Ａ"}}}]}
    ]},
    {"name": "ac_00_big_text", "doc": "A 1 MiB value round trips.", "steps": [
      {"tx": [{"insert": {"model": 1, "row": "10",
        "fields": {"0": {"text_repeat": ["ab", 524288]}}}}], "expect": "ok"},
      {"query": {"model": 1, "order": [], "limit": null}, "expect": [
        {"id": "10", "version": 1, "fields": {"0": {"text_repeat": ["ab", 524288]}}}]}
    ]}
  ]
}"#;

fn run_ref(text: &str, faults: Faults) -> Report {
    let mut report = Report::default();
    block_on(run_text(
        "suite.json",
        text,
        &|| fresh_ref(faults),
        &mut report,
    ));
    report
}

/// Names of the failed cases, sorted and deduplicated.
fn failed(report: &Report) -> Vec<String> {
    let mut names: Vec<String> = report.failures.iter().map(|f| f.case.clone()).collect();
    names.sort();
    names.dedup();
    names
}

fn with_case(steps: &str) -> String {
    format!(
        r#"{{"format": 1, "ac": "AC-00",
           "schema": [{{"model": 0, "name": "T", "fields": [{{"field": 0, "name": "t", "type": "Text"}}]}}],
           "cases": [{{"name": "ac_00_x", "doc": "", "steps": [{steps}]}}]}}"#
    )
}

fn decode_err(text: &str) -> String {
    match json::parse(text)
        .map_err(|e| e.to_string())
        .and_then(|j| cases::decode(&j).map(|_| ()).map_err(|e| e.to_string()))
    {
        Ok(()) => panic!("decoded although invalid: {text}"),
        Err(e) => e,
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ostrel-db-conformance-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------------------------
// JSON reader
// ---------------------------------------------------------------------------------------------

#[test]
fn json_reads_escapes_and_surrogate_pairs() {
    let j = json::parse(r#"{"a": "q\"\\\/\b\f\n\r\t\u0000é😀", "b": [true, false, null, -0, 12]}"#)
        .unwrap();
    let Json::Object(members) = j else {
        panic!("not an object")
    };
    assert_eq!(
        members[0],
        (
            "a".to_string(),
            Json::String("q\"\\/\u{8}\u{c}\n\r\t\0\u{e9}\u{1f600}".to_string())
        )
    );
    assert_eq!(
        members[1].1,
        Json::Array(vec![
            Json::Bool(true),
            Json::Bool(false),
            Json::Null,
            Json::Number("-0".to_string()),
            Json::Number("12".to_string()),
        ])
    );
}

#[test]
fn json_rejects_malformed_input() {
    let bad = [
        "",
        "{",
        r#"{"a": 1,}"#,
        "[1,]",
        r#"{"a": 1} x"#,
        r#"{"a": 1, "a": 2}"#,
        r#""\ud83d""#,
        r#""\ude00""#,
        r#""\ud83dA""#,
        "\"tab\there\"",
        r#""\x""#,
        "01",
        "1.",
        "-",
        "1e",
        "tru",
        r#"{1: 2}"#,
    ];
    for text in bad {
        assert!(json::parse(text).is_err(), "accepted: {text:?}");
    }
}

#[test]
fn json_limits_nesting_depth() {
    let ok = format!(
        "{}{}",
        "[".repeat(json::MAX_DEPTH),
        "]".repeat(json::MAX_DEPTH)
    );
    assert!(json::parse(&ok).is_ok());
    let deep = format!(
        "{}{}",
        "[".repeat(json::MAX_DEPTH + 1),
        "]".repeat(json::MAX_DEPTH + 1)
    );
    assert!(json::parse(&deep).is_err());
}

// ---------------------------------------------------------------------------------------------
// Case file decoding
// ---------------------------------------------------------------------------------------------

#[test]
fn decode_reads_the_example_suite() {
    let suite = cases::decode(&json::parse(SUITE).unwrap()).unwrap();
    assert_eq!(suite.cases.len(), 7);
    let first = &suite.cases[0];
    assert_eq!(first.name, "ac_00_insert_existing");
    assert!(matches!(first.steps[0].expect, Expect::Ok));
    assert!(matches!(
        first.steps[1].expect,
        Expect::Error(DbError::Conflict)
    ));
    let big = &suite.cases[6];
    let cases::Action::Tx(writes) = &big.steps[0].action else {
        panic!("not a tx")
    };
    let Write::Insert { fields, .. } = &writes[0] else {
        panic!("not an insert")
    };
    assert_eq!(fields[0].1, Value::Text("ab".repeat(524_288)));
}

#[test]
fn decode_rejects_invalid_files() {
    let insert = |fields: &str| {
        with_case(&format!(
            r#"{{"tx": [{{"insert": {{"model": 0, "row": "1", "fields": {fields}}}}}], "expect": "ok"}}"#
        ))
    };
    let query = |q: &str| with_case(&format!(r#"{{"query": {q}, "expect": []}}"#));
    let cases = [
        // format and top level
        SUITE.replacen("\"format\": 1", "\"format\": 2", 1),
        SUITE.replacen("\"ac\": \"AC-00\"", "\"ac\": \"AC-00\", \"extra\": 1", 1),
        r#"{"format": 1, "ac": "AC-00", "schema": [], "cases": []}"#.to_string(),
        // case names
        SUITE.replacen("ac_00_insert_tombstone", "insert_tombstone", 1),
        SUITE.replacen("ac_00_insert_tombstone", "ac_00_insert_existing", 1),
        // values against the schema
        insert(r#"{"0": {"int": "1"}}"#),
        insert(r#"{"0": null}"#),
        insert(r#"{}"#),
        insert(r#"{"1": {"text": "a"}, "0": {"text": "a"}}"#),
        insert(r#"{"0": {"text": "a", "int": "1"}}"#),
        insert(r#"{"0": {"text_repeat": ["", 3]}}"#),
        insert(r#"{"0": {"text_repeat": ["a", 0]}}"#),
        insert(r#"{"0": {"text_repeat": ["a", 1e3]}}"#),
        insert(r#"{"0": {"text_repeat": ["abcdefgh", 4194304]}}"#),
        // ids and numbers
        with_case(
            r#"{"tx": [{"insert": {"model": 0, "row": "01", "fields": {"0": {"text": "a"}}}}], "expect": "ok"}"#,
        ),
        with_case(
            r#"{"tx": [{"insert": {"model": 0, "row": "-1", "fields": {"0": {"text": "a"}}}}], "expect": "ok"}"#,
        ),
        with_case(
            r#"{"tx": [{"insert": {"model": 0, "row": "340282366920938463463374607431768211456", "fields": {"0": {"text": "a"}}}}], "expect": "ok"}"#,
        ),
        with_case(
            r#"{"tx": [{"insert": {"model": 9, "row": "1", "fields": {"0": {"text": "a"}}}}], "expect": "ok"}"#,
        ),
        with_case(
            r#"{"tx": [{"update": {"model": 0, "row": "1", "expect_version": 1.5, "fields": {}}}], "expect": "ok"}"#,
        ),
        with_case(
            r#"{"tx": [{"delete": {"model": 0, "row": "1", "expect_version": 1, "fields": {}}}], "expect": "ok"}"#,
        ),
        // steps and expectations
        with_case(r#"{"tx": [], "expect": "ok"}"#),
        with_case(r#"{"tx": [{"upsert": {}}], "expect": "ok"}"#),
        with_case(
            r#"{"tx": [{"delete": {"model": 0, "row": "1", "expect_version": 1}}], "expect": {"error": "Backend"}}"#,
        ),
        with_case(
            r#"{"tx": [{"delete": {"model": 0, "row": "1", "expect_version": 1}}], "expect": "fine"}"#,
        ),
        query(r#"{"model": 0, "order": [[0, "up"]], "limit": null}"#),
        query(r#"{"model": 0, "order": [[3, "asc"]], "limit": null}"#),
        query(r#"{"model": 0, "order": [], "limit": -1}"#),
        query(r#"{"model": 0, "order": []}"#),
        with_case(
            r#"{"query": {"model": 0, "order": [], "limit": null}, "expect": [{"id": "1", "version": 1, "fields": {"0": {"int": "9223372036854775808"}}}]}"#,
        ),
    ];
    for (i, text) in cases.iter().enumerate() {
        let err = decode_err(text);
        assert!(!err.is_empty(), "case {i} gave an empty error");
    }
}

#[test]
fn decode_accepts_int_bounds() {
    let text = r#"{"format": 1, "ac": "AC-00",
      "schema": [{"model": 0, "name": "T", "fields": [{"field": 0, "name": "n", "type": "Int"}]}],
      "cases": [{"name": "ac_00_x", "doc": "", "steps": [
        {"tx": [{"insert": {"model": 0, "row": "340282366920938463463374607431768211455",
          "fields": {"0": {"int": "-9223372036854775808"}}}},
          {"insert": {"model": 0, "row": "0", "fields": {"0": {"int": "9223372036854775807"}}}}],
         "expect": "ok"}]}]}"#;
    let suite = cases::decode(&json::parse(text).unwrap()).unwrap();
    let cases::Action::Tx(writes) = &suite.cases[0].steps[0].action else {
        panic!("not a tx")
    };
    assert_eq!(
        writes[0],
        Write::Insert {
            model: 0,
            row: RowId(u128::MAX),
            fields: vec![(0, Value::Int(i64::MIN))],
        }
    );
    assert!(
        matches!(&writes[1], Write::Insert { fields, .. } if fields[0].1 == Value::Int(i64::MAX))
    );
}

// ---------------------------------------------------------------------------------------------
// Runner against the reference driver
// ---------------------------------------------------------------------------------------------

#[test]
fn correct_driver_passes_every_case() {
    let report = run_ref(SUITE, Faults::default());
    assert_eq!(report.failures, Vec::new());
    assert_eq!(report.cases, 7);
    report.assert_passed();
}

#[test]
fn each_fault_fails_exactly_its_cases() {
    let table: [(&str, Faults, &[&str]); 7] = [
        (
            "upsert",
            Faults {
                upsert: true,
                ..Faults::default()
            },
            &["ac_00_insert_existing"],
        ),
        (
            "reuse_tombstone",
            Faults {
                reuse_tombstone: true,
                ..Faults::default()
            },
            &["ac_00_insert_tombstone"],
        ),
        (
            "no_version_bump",
            Faults {
                no_version_bump: true,
                ..Faults::default()
            },
            &["ac_00_update_version"],
        ),
        (
            "rollback_commits",
            Faults {
                rollback_commits: true,
                ..Faults::default()
            },
            &["ac_00_failed_tx_changes_nothing"],
        ),
        (
            "strip_nul",
            Faults {
                strip_nul: true,
                ..Faults::default()
            },
            &["ac_00_nul_round_trip"],
        ),
        (
            "utf16_order",
            Faults {
                utf16_order: true,
                ..Faults::default()
            },
            &["ac_00_code_point_order"],
        ),
        (
            "ignore_limit",
            Faults {
                ignore_limit: true,
                ..Faults::default()
            },
            &["ac_00_code_point_order"],
        ),
    ];
    for (label, faults, expected) in table {
        let report = run_ref(SUITE, faults);
        assert_eq!(failed(&report), expected, "fault {label}: {report}");
    }
}

#[test]
fn reports_unexpected_success_and_wrong_error() {
    let text = with_case(
        r#"{"tx": [{"insert": {"model": 0, "row": "1", "fields": {"0": {"text": "a"}}}}], "expect": {"error": "Conflict"}},
           {"tx": [{"delete": {"model": 0, "row": "1", "expect_version": 5}}], "expect": {"error": "VersionMismatch"}},
           {"tx": [{"delete": {"model": 0, "row": "2", "expect_version": 1}}], "expect": "ok"}"#,
    );
    let report = run_ref(&text, Faults::default());
    let messages: Vec<&str> = report.failures.iter().map(|f| f.message.as_str()).collect();
    assert_eq!(messages.len(), 3, "{report}");
    assert!(messages[0].contains("expected Conflict, every write succeeded"));
    // The first step rolled back, so row 1 does not exist.
    assert!(messages[1].contains("expected VersionMismatch, write 0 failed with NotFound"));
    assert!(messages[2].contains("write 0 failed with NotFound"));
    assert_eq!(report.failures[1].step, 1);
}

#[test]
fn compares_returned_fields_as_a_map() {
    // The reference driver returns fields in FieldId order; the expectation lists them the
    // other way round and still matches.
    let text = SUITE.replacen(
        r#"{"0": {"text": "a"}, "1": {"int": "1"}, "2": null}}]}"#,
        r#"{"2": null, "1": {"int": "1"}, "0": {"text": "a"}}}]}"#,
        1,
    );
    assert_ne!(text, SUITE);
    run_ref(&text, Faults::default()).assert_passed();
    // A different value is a mismatch of that case only.
    let other = SUITE.replacen(
        r#"{"0": {"text": "a"}, "1": {"int": "1"}, "2": null}}]}"#,
        r#"{"0": {"text": "a"}, "1": {"int": "1"}, "2": false}}]}"#,
        1,
    );
    assert_eq!(
        failed(&run_ref(&other, Faults::default())),
        ["ac_00_insert_existing"]
    );
    // An expected row must list every field, so a missing field is caught in the file.
    let missing = SUITE.replacen(
        r#"{"0": {"text": "a"}, "1": {"int": "1"}, "2": null}}]}"#,
        r#"{"0": {"text": "a"}, "1": {"int": "1"}}}]}"#,
        1,
    );
    let report = run_ref(&missing, Faults::default());
    assert_eq!(failed(&report), [""]);
    assert!(
        report.failures[0].message.contains("every field"),
        "{report}"
    );
}

#[test]
fn a_connection_error_fails_the_case_not_the_run() {
    let calls = Cell::new(0);
    let mut report = Report::default();
    let fresh = &|| {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            Box::pin(async { Err(DbError::Backend("down".to_string())) })
        } else {
            fresh_ref(Faults::default())
        }
    };
    block_on(run_text("suite.json", SUITE, fresh, &mut report));
    assert_eq!(failed(&report), ["ac_00_insert_tombstone"]);
    assert_eq!(report.cases, 7);
}

#[test]
fn invalid_file_is_a_failure() {
    let report = run_ref("{\"format\": 1}", Faults::default());
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].file, "suite.json");
    assert!(!report.is_ok());
}

#[test]
fn run_dir_reads_every_json_file_in_order() {
    let dir = temp_dir("dir");
    std::fs::write(dir.join("b.json"), SUITE.replace("ac_00_", "ac_01_")).unwrap();
    std::fs::write(dir.join("a.json"), SUITE).unwrap();
    std::fs::write(dir.join("README.md"), "not a case file").unwrap();
    let report = block_on(run_dir(&dir, &|| fresh_ref(Faults::default())));
    assert_eq!(report.files, ["a.json", "b.json"]);
    assert_eq!(report.cases, 14);
    report.assert_passed();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_dir_rejects_duplicate_names_across_files() {
    let dir = temp_dir("dup");
    std::fs::write(dir.join("a.json"), SUITE).unwrap();
    std::fs::write(dir.join("b.json"), SUITE).unwrap();
    let report = block_on(run_dir(&dir, &|| fresh_ref(Faults::default())));
    assert_eq!(report.failures.len(), 7, "{report}");
    assert!(report.failures.iter().all(|f| f.file == "b.json"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn empty_or_missing_dir_fails() {
    let dir = temp_dir("empty");
    let report = block_on(run_dir(&dir, &|| fresh_ref(Faults::default())));
    assert!(!report.is_ok());
    std::fs::remove_dir_all(&dir).unwrap();
    let report = block_on(run_dir(&dir, &|| fresh_ref(Faults::default())));
    assert!(!report.is_ok());
}

#[test]
#[should_panic(expected = "conformance failures")]
fn assert_passed_panics_on_failure() {
    run_ref(
        SUITE,
        Faults {
            upsert: true,
            ..Faults::default()
        },
    )
    .assert_passed();
}

#[test]
#[should_panic(expected = "waited")]
fn block_on_refuses_a_waiting_future() {
    block_on(std::future::pending::<()>());
}

/// The shared case files pass on the reference driver. This keeps the harness and the cases of
/// `tests/db-conformance/cases/` consistent with each other.
#[test]
fn shared_cases_pass_on_the_reference_driver() {
    let report = block_on(run_dir(&super::cases_dir(), &|| {
        fresh_ref(Faults::default())
    }));
    report.assert_passed();
}

/// The entry documented in the README and in `mod.rs`, written the same way with the reference
/// driver. Its connections share one database, so this passes only if every case gets a driver
/// of its own.
#[test]
fn documented_entry_runs_the_shared_cases() {
    let dir = super::cases_dir();
    let fresh = new_driver_per_case(RefDriver::default, "ref:");
    block_on(run_dir(&dir, &fresh)).assert_passed();
}

/// Connections of one driver share a database, so reusing one driver for every case lets
/// earlier cases leak into later ones. This is why the entry builds a driver per case.
#[test]
fn one_driver_for_every_case_fails() {
    let driver = RefDriver::default();
    let mut report = Report::default();
    block_on(run_text(
        "suite.json",
        SUITE,
        &|| driver.connect("ref:"),
        &mut report,
    ));
    assert!(!report.is_ok(), "{report}");
}

#[test]
fn new_driver_per_case_passes_the_connect_error_on() {
    let fresh = new_driver_per_case(RefDriver::default, "other:");
    let mut report = Report::default();
    block_on(run_text("suite.json", SUITE, &fresh, &mut report));
    assert_eq!(report.failures.len(), 7, "{report}");
    assert!(report.failures[0].message.contains("connect failed with Unsupported"));
}
