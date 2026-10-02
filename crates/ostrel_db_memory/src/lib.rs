//! In memory driver for the database interface of the Ostrel programming language.
//!
//! The driver serves the URL scheme `memory:`. It keeps all data in the process and loses it when
//! the last connection is dropped, which makes it the driver for tests and quick local runs. It is
//! also the template for community drivers (AC-18): every rule of the contract in
//! [`ostrel_db::api`] is implemented here in plain Rust, in the order a new driver meets them.
//!
//! Model of the data:
//!
//! * All connections opened from one [`MemoryDriver`] share one database. A connection reads
//!   committed state.
//! * A transaction works on a private copy of the committed state and replaces it on commit, so
//!   it sees its own writes and nobody else does. Two transactions that both change data and
//!   overlap in time cannot both commit: the second commit fails with [`DbError::Backend`]
//!   (first committer wins), and the caller retries.
//! * After a call inside a transaction failed, every further call except rollback fails too, so
//!   half applied writes can never be committed by mistake.
//!
//! Ordering of query results: `Null` and missing fields first, then `Bool` (`false` before
//! `true`), then `Int` numerically, then `Text` by Unicode code point (D50, D61). Rows equal on
//! all sort keys are ordered by id in the direction of the last key.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::rc::Rc;

use ostrel_db::api::{
    BoxFuture, Capabilities, Connection, DbError, Dir, Driver, FieldId, MigrationPlan, ModelId,
    Query, Row, RowId, Rows, ServerSeq, StoredOp, Transaction, Value, Write,
};

/// The URL scheme this driver serves.
pub const SCHEME: &str = "memory:";

/// One stored row. `fields` is `None` once the row is deleted: the tombstone keeps the id taken
/// (rule "ids are never reused").
#[derive(Clone, Debug)]
struct Stored {
    model: ModelId,
    version: u64,
    fields: Option<BTreeMap<FieldId, Value>>,
}

/// Everything a transaction may change. Cloned at the start of every transaction.
#[derive(Clone, Debug, Default)]
struct State {
    rows: BTreeMap<RowId, Stored>,
    /// The op log. The op at index `i` has the position `i + 1`.
    ops: Vec<StoredOp>,
    sequences: BTreeMap<String, u64>,
}

/// The committed state plus a counter of commits that changed it.
#[derive(Debug, Default)]
struct Database {
    state: State,
    generation: u64,
}

/// Entry point of the in memory driver.
#[derive(Debug, Default)]
pub struct MemoryDriver {
    db: Rc<RefCell<Database>>,
}

impl MemoryDriver {
    /// A driver with a new, empty database.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Driver for MemoryDriver {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transactions: true,
            sequences: true,
            subqueries: false,
            json_fields: false,
            max_in_list: u32::MAX,
        }
    }

    fn connect<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Box<dyn Connection>, DbError>> {
        Box::pin(async move {
            if !url.starts_with(SCHEME) {
                return Err(DbError::Unsupported("url scheme, expected memory:"));
            }
            let db = Rc::clone(&self.db);
            Ok(Box::new(MemoryConnection { db }) as Box<dyn Connection>)
        })
    }
}

/// An open connection to the shared database of one [`MemoryDriver`].
#[derive(Debug)]
pub struct MemoryConnection {
    db: Rc<RefCell<Database>>,
}

impl Connection for MemoryConnection {
    fn migrate<'a>(&'a mut self, plan: &'a MigrationPlan) -> BoxFuture<'a, Result<(), DbError>> {
        // Rows are schema free maps, so there is nothing to create. A step this driver does not
        // know must not be skipped silently once steps exist.
        Box::pin(async move {
            match plan.steps.first() {
                None => Ok(()),
                Some(step) => match *step {},
            }
        })
    }

    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move { Ok(run_query(&self.db.borrow().state, q)) })
    }

    fn begin<'c>(&'c mut self) -> BoxFuture<'c, Result<Box<dyn Transaction<'c> + 'c>, DbError>> {
        Box::pin(async move {
            let (work, base) = {
                let db = self.db.borrow();
                (db.state.clone(), db.generation)
            };
            let tx = MemoryTransaction {
                conn: self,
                work,
                base,
                dirty: false,
                failed: false,
            };
            Ok(Box::new(tx) as Box<dyn Transaction<'c> + 'c>)
        })
    }
}

/// A transaction: a private copy of the state, written back on commit.
struct MemoryTransaction<'c> {
    conn: &'c mut MemoryConnection,
    work: State,
    /// Generation of the committed state the copy was taken from.
    base: u64,
    /// Whether this transaction changed anything.
    dirty: bool,
    /// Whether a call failed; then only rollback is allowed.
    failed: bool,
}

impl MemoryTransaction<'_> {
    /// Runs `f` on the working copy unless an earlier call failed, and remembers a failure.
    fn step<T>(&mut self, f: impl FnOnce(&mut State) -> Result<T, DbError>) -> Result<T, DbError> {
        if self.failed {
            return Err(failed_earlier());
        }
        let result = f(&mut self.work);
        self.failed = result.is_err();
        result
    }
}

impl<'c> Transaction<'c> for MemoryTransaction<'c> {
    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move { self.step(|s| Ok(run_query(s, q))) })
    }

    fn apply<'a>(&'a mut self, w: &'a [Write]) -> BoxFuture<'a, Result<(), DbError>> {
        self.dirty = true;
        Box::pin(async move { self.step(|s| w.iter().try_for_each(|w| apply_write(s, w))) })
    }

    fn append_ops<'a>(
        &'a mut self,
        ops: &'a [StoredOp],
    ) -> BoxFuture<'a, Result<ServerSeq, DbError>> {
        self.dirty = true;
        Box::pin(async move {
            self.step(|s| {
                for op in ops {
                    // The driver assigns the position; a `seq` given by the caller is ignored.
                    let seq = Some(ServerSeq(s.ops.len() as u64 + 1));
                    s.ops.push(StoredOp { seq, ..op.clone() });
                }
                Ok(ServerSeq(s.ops.len() as u64))
            })
        })
    }

    fn ops_since<'a>(
        &'a mut self,
        after: ServerSeq,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<StoredOp>, DbError>> {
        Box::pin(async move {
            self.step(|s| {
                // Positions start at 1, so the ops after position `after` start at index `after`.
                let start = usize::try_from(after.0).unwrap_or(usize::MAX);
                let rest = s.ops.get(start..).unwrap_or_default();
                Ok(rest.iter().take(limit as usize).cloned().collect())
            })
        })
    }

    fn next_in_sequence<'a>(&'a mut self, key: &'a str) -> BoxFuture<'a, Result<u64, DbError>> {
        self.dirty = true;
        Box::pin(async move {
            self.step(|s| {
                let n = s.sequences.entry(key.to_owned()).or_insert(0);
                *n = n
                    .checked_add(1)
                    .ok_or(DbError::Backend("sequence exhausted".into()))?;
                Ok(*n)
            })
        })
    }

    fn commit(self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>> {
        Box::pin(async move {
            if self.failed {
                return Err(failed_earlier());
            }
            if !self.dirty {
                return Ok(());
            }
            let mut db = self.conn.db.borrow_mut();
            if db.generation != self.base {
                return Err(DbError::Backend("concurrent commit, retry".into()));
            }
            db.state = self.work;
            db.generation += 1;
            Ok(())
        })
    }

    fn rollback(self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>> {
        // The working copy is simply dropped; dropping without commit does the same.
        Box::pin(async move { Ok(()) })
    }
}

fn failed_earlier() -> DbError {
    DbError::Backend("transaction failed earlier, roll it back".into())
}

/// Applies one write to the state, following the rules of [`Write`].
fn apply_write(s: &mut State, w: &Write) -> Result<(), DbError> {
    match w {
        Write::Insert { model, row, fields } => {
            // The id is taken while a live row or a tombstone holds it, in any model.
            if s.rows.contains_key(row) {
                return Err(DbError::Conflict);
            }
            let fields = Some(fields.iter().cloned().collect());
            let stored = Stored {
                model: *model,
                version: 1,
                fields,
            };
            s.rows.insert(*row, stored);
        }
        Write::Update {
            model,
            row,
            expect_version,
            fields,
        } => {
            let (version, current) = live_row(s, *model, *row, *expect_version)?;
            current.extend(fields.iter().cloned());
            *version += 1;
        }
        Write::Delete {
            model,
            row,
            expect_version,
        } => {
            let (version, _) = live_row(s, *model, *row, *expect_version)?;
            *version += 1;
            if let Some(stored) = s.rows.get_mut(row) {
                stored.fields = None;
            }
        }
    }
    Ok(())
}

/// The version and fields of a live row of `model`, checked against `expect_version`.
fn live_row(
    s: &mut State,
    model: ModelId,
    row: RowId,
    expect_version: u64,
) -> Result<(&mut u64, &mut BTreeMap<FieldId, Value>), DbError> {
    let stored = s
        .rows
        .get_mut(&row)
        .filter(|r| r.model == model)
        .ok_or(DbError::NotFound)?;
    let fields = stored.fields.as_mut().ok_or(DbError::NotFound)?;
    if stored.version != expect_version {
        return Err(DbError::VersionMismatch);
    }
    Ok((&mut stored.version, fields))
}

/// Live rows of the queried model in query order, at most `limit`.
fn run_query(s: &State, q: &Query) -> Rows {
    let mut rows: Vec<(RowId, u64, &BTreeMap<FieldId, Value>)> = s
        .rows
        .iter()
        .filter(|(_, r)| r.model == q.model)
        .filter_map(|(id, r)| r.fields.as_ref().map(|f| (*id, r.version, f)))
        .collect();
    let tie = q.order.last().map_or(Dir::Asc, |(_, dir)| *dir);
    rows.sort_by(|a, b| {
        let keys = q
            .order
            .iter()
            .map(|(f, dir)| directed(*dir, compare(a.2.get(f), b.2.get(f))));
        let by_id = directed(tie, a.0.cmp(&b.0));
        keys.fold(Ordering::Equal, Ordering::then).then(by_id)
    });
    let limit = q.limit.map_or(usize::MAX, |n| n as usize);
    let rows = rows
        .into_iter()
        .take(limit)
        .map(|(id, version, fields)| Row {
            id,
            version,
            fields: fields.iter().map(|(f, v)| (*f, v.clone())).collect(),
        });
    Rows(rows.collect())
}

fn directed(dir: Dir, o: Ordering) -> Ordering {
    match dir {
        Dir::Asc => o,
        Dir::Desc => o.reverse(),
    }
}

/// Total order of stored values; a missing field counts as `Null`.
fn compare(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    fn rank(v: Option<&Value>) -> u8 {
        match v {
            None | Some(Value::Null) => 0,
            Some(Value::Bool(_)) => 1,
            Some(Value::Int(_)) => 2,
            Some(Value::Text(_)) => 3,
        }
    }
    match (a, b) {
        (Some(Value::Bool(x)), Some(Value::Bool(y))) => x.cmp(y),
        (Some(Value::Int(x)), Some(Value::Int(y))) => x.cmp(y),
        // UTF-8 byte order is Unicode code point order (D50).
        (Some(Value::Text(x)), Some(Value::Text(y))) => x.as_bytes().cmp(y.as_bytes()),
        _ => rank(a).cmp(&rank(b)),
    }
}
