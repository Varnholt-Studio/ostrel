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
//!   (first committer wins), and the caller retries. A migration counts as such a change.
//! * After a call inside a transaction failed, every further call except rollback fails too, so
//!   half applied writes can never be committed by mistake.
//! * The applied schema and its enum columns are part of the database, so every connection of
//!   the driver sees them (D87).
//! * Final outcomes of ops (D110) are the op log plus a map of rejected op ids; `last_seq` per
//!   replica is kept next to them and changes in the same transaction.
//!
//! * Live rows are shared [`LiveRow`]s, so copying the state for a transaction copies no row;
//!   a write copies the one row it changes. Deleted rows leave only their id behind.
//!
//! Filters, ordering, cursor and limit of queries, the enum check of writes, the rules of `Set`
//! and `Map` changes and the transitions of one row ([`LiveRow::inserted`], [`LiveRow::check`],
//! [`LiveRow::update`]) are those of [`ostrel_db::eval`], shared with every driver that keeps
//! rows outside SQL (D129, D139). What stays here is the storage: the map of live rows, the ids
//! of deleted rows and the rule that an id stays taken, the op log and the transactions. A new
//! driver of that kind keeps its rows its own way and uses the shared rules the same way.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use ostrel_db::api::{
    AppliedSchema, BoxFuture, Capabilities, Connection, DbError, Driver, EnumColumn, MigrationPlan,
    NewOp, OpId, Query, ReplayOutcome, ReplicaId, RowId, Rows, ServerSeq, StoredOp, Transaction,
    Write, check_reject_reason, check_writes,
};
use ostrel_db::eval::{self, LiveRow, Lookup};

/// The URL scheme this driver serves.
pub const SCHEME: &str = "memory:";

/// Everything a transaction or a migration may change. Cloned at the start of every
/// transaction.
#[derive(Clone, Debug, Default)]
struct State {
    /// Live rows of every model.
    rows: BTreeMap<RowId, Rc<LiveRow>>,
    /// Ids of deleted rows: they stay taken (rule "ids are never reused").
    deleted: BTreeSet<RowId>,
    /// The op log. The op at index `i` has the position `i + 1`.
    ops: Vec<StoredOp>,
    /// The final outcome of every op in the log or rejected for good (D110).
    outcomes: BTreeMap<OpId, ReplayOutcome>,
    /// Highest `seq` with a final outcome, per replica.
    last_seqs: BTreeMap<ReplicaId, u32>,
    sequences: BTreeMap<String, u64>,
    /// What the last successful migration stored; `None` before the first one.
    schema: Option<AppliedSchema>,
    /// Enum columns of the applied schema, stored with it (D87).
    enums: Vec<EnumColumn>,
}

impl State {
    /// Stores the final outcome of `op` and raises `last_seq` of its replica to its `seq`.
    fn decide(&mut self, op: OpId, outcome: ReplayOutcome) {
        self.outcomes.insert(op, outcome);
        let last = self.last_seqs.entry(op.replica).or_insert(0);
        *last = (*last).max(op.seq);
    }
}

impl Lookup for State {
    fn live(&self, id: RowId) -> Result<Option<Rc<LiveRow>>, DbError> {
        Ok(self.rows.get(&id).cloned())
    }
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
        Box::pin(async move {
            plan.check()?;
            let mut db = self.db.borrow_mut();
            if plan.from != db.state.schema.as_ref().map(|a| a.hash) {
                return Err(DbError::SchemaMismatch);
            }
            // A plan from the stored schema to itself with no steps changes nothing, not even
            // the stored schema text or the enum columns.
            if plan.from == Some(plan.to) && plan.steps.is_empty() {
                return Ok(());
            }
            // Rows are schema free maps, so there is nothing to create. A step this driver does
            // not know must not be skipped silently once steps exist.
            if let Some(step) = plan.steps.first() {
                match *step {}
            }
            db.state.schema = Some(AppliedSchema {
                hash: plan.to,
                schema: plan.schema.clone(),
            });
            db.state.enums = plan.enums.clone();
            // An open transaction copied the old schema; it must not write it back.
            db.generation += 1;
            Ok(())
        })
    }

    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move { run_query(&self.db.borrow().state, q) })
    }

    fn applied_schema<'a>(&'a mut self) -> BoxFuture<'a, Result<Option<AppliedSchema>, DbError>> {
        Box::pin(async move { Ok(self.db.borrow().state.schema.clone()) })
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
        Box::pin(async move { self.step(|s| run_query(s, q)) })
    }

    fn apply<'a>(&'a mut self, w: &'a [Write]) -> BoxFuture<'a, Result<(), DbError>> {
        self.dirty = true;
        Box::pin(async move {
            self.step(|s| {
                // Both checks run before the first write, so a refused batch changes nothing.
                check_writes(w)?;
                eval::check_enums(&s.enums, w)?;
                w.iter().try_for_each(|w| apply_write(s, w))
            })
        })
    }

    fn append_ops<'a>(&'a mut self, ops: &'a [NewOp]) -> BoxFuture<'a, Result<ServerSeq, DbError>> {
        self.dirty = true;
        Box::pin(async move {
            self.step(|s| {
                // Every id is checked before the first op is appended, so a refused call
                // appends nothing. An id twice in the call conflicts like a decided one.
                let mut seen = BTreeSet::new();
                if ops
                    .iter()
                    .any(|o| s.outcomes.contains_key(&o.id) || !seen.insert(o.id))
                {
                    return Err(DbError::Conflict);
                }
                for op in ops {
                    // The driver assigns the position.
                    let seq = ServerSeq(s.ops.len() as u64 + 1);
                    let stored = StoredOp {
                        seq,
                        op: op.clone(),
                    };
                    s.decide(op.id, ReplayOutcome::Accepted(stored.clone()));
                    s.ops.push(stored);
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

    fn record_reject<'a>(
        &'a mut self,
        op: OpId,
        reason: &'a str,
    ) -> BoxFuture<'a, Result<(), DbError>> {
        self.dirty = true;
        Box::pin(async move {
            self.step(|s| {
                check_reject_reason(reason)?;
                if s.outcomes.contains_key(&op) {
                    return Err(DbError::Conflict);
                }
                s.decide(op, ReplayOutcome::Rejected(reason.to_owned()));
                Ok(())
            })
        })
    }

    fn last_seq<'a>(&'a mut self, replica: ReplicaId) -> BoxFuture<'a, Result<u32, DbError>> {
        Box::pin(async move { self.step(|s| Ok(s.last_seqs.get(&replica).copied().unwrap_or(0))) })
    }

    fn replay_outcome<'a>(
        &'a mut self,
        op: OpId,
    ) -> BoxFuture<'a, Result<Option<ReplayOutcome>, DbError>> {
        Box::pin(async move { self.step(|s| Ok(s.outcomes.get(&op).cloned())) })
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

// ---------------------------------------------------------------------------------------------
// Writes and queries
// ---------------------------------------------------------------------------------------------

/// Applies one write to the state, following the rules of [`Write`].
fn apply_write(s: &mut State, w: &Write) -> Result<(), DbError> {
    match w {
        Write::Insert {
            model,
            row,
            fields,
            collections,
        } => {
            // The id is taken while a live row or a deleted one holds it, in any model.
            if s.rows.contains_key(row) || s.deleted.contains(row) {
                return Err(DbError::Conflict);
            }
            let new = LiveRow::inserted(*model, fields, collections);
            s.rows.insert(*row, Rc::new(new));
        }
        Write::Update {
            model,
            row,
            expect_version,
            fields,
            collections,
        } => {
            // A deleted row is no longer in `rows`, so it is not found like a missing one.
            let r = s.rows.get_mut(row).ok_or(DbError::NotFound)?;
            r.check(*model, *expect_version)?;
            // Copies the row only if a copy of the state still shares it.
            Rc::make_mut(r).update(fields, collections);
        }
        Write::Delete {
            model,
            row,
            expect_version,
        } => {
            let r = s.rows.get(row).ok_or(DbError::NotFound)?;
            r.check(*model, *expect_version)?;
            s.rows.remove(row);
            s.deleted.insert(*row);
        }
    }
    Ok(())
}

/// Runs a query on the state through [`eval::run`].
fn run_query(s: &State, q: &Query) -> Result<Rows, DbError> {
    let rows = s.rows.iter().filter(|(_, r)| r.model == q.model);
    let rows = rows.map(|(id, r)| (*id, Rc::clone(r))).collect();
    eval::run(&s.enums, s, q, rows)
}
