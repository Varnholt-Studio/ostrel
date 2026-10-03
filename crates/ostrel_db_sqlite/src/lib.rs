//! SQLite driver for the database interface of the Ostrel programming language.
//!
//! The driver serves the URL scheme `sqlite:`. `sqlite:<path>` opens or creates the database
//! file at `<path>`; the path is a plain file name, never an SQLite URI, so no query parameter
//! can change how the file is opened. `sqlite::memory:` opens a new private database that lives
//! as long as its connection. It links the system `libsqlite3` (ARCHITECTURE 11, at least 3.45,
//! checked on connect).
//!
//! Behaviour follows the contract in [`ostrel_db::api`]:
//!
//! * Every transaction starts with `BEGIN IMMEDIATE`, so it holds the write lock from its first
//!   statement. A second connection that wants to write waits up to the busy timeout and then
//!   fails with [`DbError::Backend`]; nothing is half applied. `query` outside a transaction
//!   reads one consistent snapshot of committed state.
//! * Each `apply` and `append_ops` call runs in a savepoint: a refused call changes nothing.
//!   After a call inside a transaction failed, every further call except rollback fails too.
//! * Dropping a transaction without commit rolls it back.
//!
//! Storage layout (docs/db-mapping.md describes the target layout): no migration step exists
//! yet, so the driver cannot create tables per model. Rows, column values and collection entries
//! are stored schema free in adapter tables (`store` module), every value in an exact stored
//! form and every `Set` element and `Map` key also as an order key whose byte order is
//! [`ostrel_db::api::compare_key`] (D61). Filters, sort and cursor are evaluated by the driver in
//! Rust over the live rows of the queried model (`eval` module), with the same rules as the in
//! memory driver; the translation to SQL comes with the per model tables. Ids, stamps and the op
//! log follow docs/db-mapping.md 2.3 and 4.
//!
//! The driver is blocking: every future is ready when first polled. ARCHITECTURE 4.1 runs
//! blocking drivers on the blocking pool of the server.

mod codec;
mod eval;
mod store;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ostrel_db::api::{
    AppliedSchema, BoxFuture, Capabilities, Connection, DbError, Driver, EnumColumn, MigrationPlan,
    NewOp, Query, Rows, ServerSeq, StoredOp, Transaction, Write, check_writes,
};
use rusqlite::OpenFlags;

use store::backend;

/// The URL scheme this driver serves.
pub const SCHEME: &str = "sqlite:";

/// The URL path of a private in memory database.
pub const MEMORY: &str = ":memory:";

/// Oldest SQLite this driver runs on (docs/db-mapping.md: `STRICT`, `RETURNING`, row values).
pub const MIN_SQLITE: i32 = 3_045_000;

/// How long a connection waits for the write lock of another one (docs/db-mapping.md 7).
pub const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Entry point of the SQLite driver.
#[derive(Clone, Debug)]
pub struct SqliteDriver {
    busy_timeout: Duration,
}

impl Default for SqliteDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl SqliteDriver {
    pub fn new() -> Self {
        Self {
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
        }
    }

    /// A driver whose connections wait at most `timeout` for the write lock.
    pub fn with_busy_timeout(timeout: Duration) -> Self {
        Self {
            busy_timeout: timeout,
        }
    }

    fn open(&self, url: &str) -> Result<SqliteConnection, DbError> {
        let path = url
            .strip_prefix(SCHEME)
            .ok_or(DbError::Unsupported("url scheme, expected sqlite:"))?;
        if path.is_empty() {
            return Err(DbError::Unsupported("sqlite url without a path"));
        }
        if rusqlite::version_number() < MIN_SQLITE {
            return Err(DbError::Unsupported("sqlite older than 3.45"));
        }
        // No SQLITE_OPEN_URI: the path is a file name, whatever it looks like.
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = if path == MEMORY {
            rusqlite::Connection::open_in_memory_with_flags(flags)
        } else {
            rusqlite::Connection::open_with_flags(path, flags)
        }
        .map_err(backend)?;
        conn.busy_timeout(self.busy_timeout).map_err(backend)?;
        // A file from elsewhere must not run functions through views or triggers.
        conn.execute_batch(
            "PRAGMA trusted_schema = OFF; PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;",
        )
        .map_err(backend)?;
        // WAL lets readers go on while one connection writes. An in memory database answers
        // `memory`, which is fine.
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .map_err(backend)?;
        let mut c = SqliteConnection { conn };
        c.in_write(store::prepare)?;
        Ok(c)
    }
}

impl Driver for SqliteDriver {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transactions: true,
            sequences: false,
            subqueries: true,
            json_fields: true,
            max_in_list: 1000,
        }
    }

    fn connect<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Box<dyn Connection>, DbError>> {
        Box::pin(async move { Ok(Box::new(self.open(url)?) as Box<dyn Connection>) })
    }
}

/// An open connection to one SQLite database.
pub struct SqliteConnection {
    conn: rusqlite::Connection,
}

impl std::fmt::Debug for SqliteConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteConnection").finish_non_exhaustive()
    }
}

impl SqliteConnection {
    /// Runs `f` in a write transaction that commits when `f` succeeds.
    fn in_write<T>(
        &mut self,
        f: impl FnOnce(&rusqlite::Connection) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        self.within("BEGIN IMMEDIATE", f)
    }

    /// Runs `f` in a read transaction, so that it sees one snapshot of committed state.
    fn in_read<T>(
        &mut self,
        f: impl FnOnce(&rusqlite::Connection) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        self.within("BEGIN DEFERRED", f)
    }

    fn within<T>(
        &mut self,
        begin: &str,
        f: impl FnOnce(&rusqlite::Connection) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        self.conn.execute_batch(begin).map_err(backend)?;
        let result = f(&self.conn).and_then(|v| {
            self.conn.execute_batch("COMMIT").map_err(backend)?;
            Ok(v)
        });
        if result.is_err() && !self.conn.is_autocommit() {
            // The error that matters is the first one; a failed rollback leaves the connection
            // in a transaction, which the next BEGIN reports.
            let _ = self.conn.execute_batch("ROLLBACK");
        }
        result
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl Connection for SqliteConnection {
    fn migrate<'a>(&'a mut self, plan: &'a MigrationPlan) -> BoxFuture<'a, Result<(), DbError>> {
        Box::pin(async move {
            plan.check()?;
            self.in_write(|conn| {
                let stored = store::applied_schema(conn)?.map(|a| a.hash);
                if plan.from != stored {
                    return Err(DbError::SchemaMismatch);
                }
                // A plan from the stored schema to itself with no steps changes nothing, not
                // even the stored schema text or the enum columns.
                if plan.from == Some(plan.to) && plan.steps.is_empty() {
                    return Ok(());
                }
                // Rows are stored schema free, so there is nothing to create. A step this
                // driver does not know must not be skipped silently once steps exist.
                if let Some(step) = plan.steps.first() {
                    match *step {}
                }
                store::store_schema(conn, plan, now_ms())
            })
        })
    }

    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move {
            self.in_read(|conn| {
                let enums = store::load_enums(conn)?;
                store::query(conn, &enums, q)
            })
        })
    }

    fn applied_schema<'a>(&'a mut self) -> BoxFuture<'a, Result<Option<AppliedSchema>, DbError>> {
        Box::pin(async move { store::applied_schema(&self.conn) })
    }

    fn begin<'c>(&'c mut self) -> BoxFuture<'c, Result<Box<dyn Transaction<'c> + 'c>, DbError>> {
        Box::pin(async move {
            self.conn
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(backend)?;
            let mut tx = SqliteTransaction {
                conn: self,
                enums: Vec::new(),
                open: true,
                failed: false,
            };
            // The enum columns cannot change while this transaction holds the write lock.
            tx.enums = store::load_enums(&tx.conn.conn)?;
            Ok(Box::new(tx) as Box<dyn Transaction<'c> + 'c>)
        })
    }
}

/// A transaction on a borrowed connection. It holds the write lock of the database file.
struct SqliteTransaction<'c> {
    conn: &'c mut SqliteConnection,
    /// Enum columns of the applied schema, loaded at begin.
    enums: Vec<EnumColumn>,
    /// Whether the SQLite transaction is still open, so that drop must roll it back.
    open: bool,
    /// Whether a call failed; then only rollback is allowed.
    failed: bool,
}

impl SqliteTransaction<'_> {
    /// Runs `f` unless an earlier call failed, and remembers a failure.
    fn step<T>(
        &mut self,
        f: impl FnOnce(&rusqlite::Connection, &[EnumColumn]) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        if self.failed {
            return Err(failed_earlier());
        }
        let result = f(&self.conn.conn, &self.enums);
        self.failed = result.is_err();
        result
    }

    /// Like [`Self::step`], inside a savepoint that is undone when `f` fails.
    fn atomic<T>(
        &mut self,
        f: impl FnOnce(&rusqlite::Connection, &[EnumColumn]) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        self.step(|conn, enums| {
            conn.execute_batch("SAVEPOINT ostrel_call")
                .map_err(backend)?;
            match f(conn, enums) {
                Ok(v) => {
                    conn.execute_batch("RELEASE ostrel_call").map_err(backend)?;
                    Ok(v)
                }
                Err(e) => {
                    // The transaction is marked failed either way; the first error is the one
                    // to report.
                    let _ = conn.execute_batch("ROLLBACK TO ostrel_call; RELEASE ostrel_call");
                    Err(e)
                }
            }
        })
    }

    fn end(&mut self, sql: &str) -> Result<(), DbError> {
        let result = self.conn.conn.execute_batch(sql).map_err(backend);
        if result.is_ok() || self.conn.conn.is_autocommit() {
            self.open = false;
        }
        result
    }
}

impl Drop for SqliteTransaction<'_> {
    fn drop(&mut self) {
        if self.open && !self.conn.conn.is_autocommit() {
            let _ = self.conn.conn.execute_batch("ROLLBACK");
        }
    }
}

fn failed_earlier() -> DbError {
    DbError::Backend("transaction failed earlier, roll it back".into())
}

impl<'c> Transaction<'c> for SqliteTransaction<'c> {
    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>> {
        Box::pin(async move { self.step(|conn, enums| store::query(conn, enums, q)) })
    }

    fn apply<'a>(&'a mut self, w: &'a [Write]) -> BoxFuture<'a, Result<(), DbError>> {
        Box::pin(async move {
            self.atomic(|conn, enums| {
                // All checks run before the first write.
                check_writes(w)?;
                store::check_enums(enums, w)?;
                store::check_storable(w)?;
                w.iter().try_for_each(|w| store::apply_write(conn, w))
            })
        })
    }

    fn append_ops<'a>(&'a mut self, ops: &'a [NewOp]) -> BoxFuture<'a, Result<ServerSeq, DbError>> {
        Box::pin(async move { self.atomic(|conn, _| store::append_ops(conn, ops)) })
    }

    fn ops_since<'a>(
        &'a mut self,
        after: ServerSeq,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<StoredOp>, DbError>> {
        Box::pin(async move { self.step(|conn, _| store::ops_since(conn, after, limit)) })
    }

    fn next_in_sequence<'a>(&'a mut self, key: &'a str) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async move { self.atomic(|conn, _| store::next_in_sequence(conn, key)) })
    }

    fn commit(mut self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>> {
        Box::pin(async move {
            if self.failed {
                return Err(failed_earlier());
            }
            // A failed COMMIT leaves the transaction open; drop rolls it back.
            self.end("COMMIT")
        })
    }

    fn rollback(mut self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>> {
        Box::pin(async move { self.end("ROLLBACK") })
    }
}

#[cfg(test)]
mod tests;
