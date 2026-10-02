//! The driver contract (ARCHITECTURE 6.1).
//!
//! All futures are boxed and not `Send`, matching the single threaded server runtime. A
//! transaction borrows its connection mutably for its whole life, so nothing else can use the
//! connection until the transaction is committed, rolled back or dropped.
//!
//! Rules every driver follows:
//!
//! * A row id is unique across all models and is never reused: once a row was deleted, its id
//!   stays taken (tombstone).
//! * Every row has a version. [`Write::Insert`] creates version 1; every successful
//!   [`Write::Update`] or [`Write::Delete`] increases it by one.
//! * After a transaction method returned an error, the caller rolls the transaction back. A
//!   driver may refuse every further call except [`Transaction::rollback`].
//! * Dropping a transaction without commit has the effect of a rollback.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// A boxed future that may borrow for `'a`. Not `Send`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Index of a model in the compiled schema.
pub type ModelId = u32;

/// Index of a field within its model in the compiled schema.
pub type FieldId = u32;

/// Identity of a row. Stand in until `ostrel_core` provides the shared type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(pub u128);

/// Position in the server op log. The first appended op has position 1; position 0 means
/// "before the first op". Stand in until `ostrel_core` provides the shared type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServerSeq(pub u64);

/// A stored field value. Stand in for the persisted value of `ostrel_core`, which also carries
/// floats, times, bytes, references and collections.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Text(String),
}

/// Errors a driver reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DbError {
    /// [`Write::Insert`] of an id that exists or existed in any model.
    Conflict,
    /// [`Write::Update`] or [`Write::Delete`] whose `expect_version` is not the stored version.
    VersionMismatch,
    /// [`Write::Update`] or [`Write::Delete`] of a row that does not exist in that model or was
    /// deleted.
    NotFound,
    /// The driver does not support the requested operation or connection URL.
    Unsupported(&'static str),
    /// Any other failure of the database. The text is for logs, not for end users.
    Backend(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Conflict => f.write_str("row id exists or existed"),
            DbError::VersionMismatch => f.write_str("row version does not match"),
            DbError::NotFound => f.write_str("row not found"),
            DbError::Unsupported(what) => write!(f, "unsupported: {what}"),
            DbError::Backend(msg) => write!(f, "database error: {msg}"),
        }
    }
}

impl std::error::Error for DbError {}

/// One entry of the server op log. `seq` is `None` when the op is appended and is set by the
/// driver.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredOp {
    pub seq: Option<ServerSeq>,
    pub model: ModelId,
    pub row: RowId,
    pub payload: Vec<u8>,
}

/// Sort direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Asc,
    Desc,
}

/// A query over the live rows of one model. The filter expression and the keyset cursor of
/// ARCHITECTURE 6.1 are added with the query IR.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub model: ModelId,
    /// Sort keys in priority order. Rows equal on all keys are ordered by id in the direction
    /// of the last key, ascending when there is no key.
    pub order: Vec<(FieldId, Dir)>,
    /// At most this many rows. `Some(0)` returns no rows.
    pub limit: Option<u32>,
}

/// One live row as returned by a query.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: RowId,
    pub version: u64,
    pub fields: Vec<(FieldId, Value)>,
}

/// Result of a query, in query order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rows(pub Vec<Row>);

/// One step of a migration. No steps are defined yet; they are computed by the planner from
/// the model schema and executed by the driver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrationStep {}

/// Steps that bring a database from one schema to the next.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MigrationPlan {
    pub steps: Vec<MigrationStep>,
}

/// What a driver does natively. The planner emulates what is missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub transactions: bool,
    /// When `false`, the driver emulates [`Transaction::next_in_sequence`] inside the
    /// transaction; the method works either way.
    pub sequences: bool,
    pub subqueries: bool,
    pub json_fields: bool,
    /// Largest number of values in one `in` list.
    pub max_in_list: u32,
}

/// A change to materialised rows, applied inside a transaction.
#[derive(Clone, Debug, PartialEq)]
pub enum Write {
    /// Creates the row with version 1. Fails with [`DbError::Conflict`] if `row` exists or
    /// existed in any model.
    Insert {
        model: ModelId,
        row: RowId,
        fields: Vec<(FieldId, Value)>,
    },
    /// Sets the listed fields and increases the version; fields not listed keep their value.
    /// Fails with [`DbError::NotFound`] or [`DbError::VersionMismatch`].
    Update {
        model: ModelId,
        row: RowId,
        expect_version: u64,
        fields: Vec<(FieldId, Value)>,
    },
    /// Deletes the row and leaves a tombstone, so the id is never reused. Fails with
    /// [`DbError::NotFound`] or [`DbError::VersionMismatch`].
    Delete {
        model: ModelId,
        row: RowId,
        expect_version: u64,
    },
}

/// Entry point of a driver. Selected by the URL scheme of `OSTREL_DB`.
pub trait Driver {
    /// Short lowercase name, for example `memory`.
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    /// Opens a connection. A URL the driver cannot serve gives [`DbError::Unsupported`].
    fn connect<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Box<dyn Connection>, DbError>>;
}

/// An open connection.
pub trait Connection {
    fn migrate<'a>(&'a mut self, plan: &'a MigrationPlan) -> BoxFuture<'a, Result<(), DbError>>;
    /// Reads committed state.
    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>>;
    /// The transaction borrows the connection for `'c`; nothing else can use it meanwhile.
    fn begin<'c>(&'c mut self) -> BoxFuture<'c, Result<Box<dyn Transaction<'c> + 'c>, DbError>>;
}

/// A transaction on a borrowed connection.
pub trait Transaction<'c> {
    /// Reads committed state plus the writes of this transaction.
    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>>;
    /// Applies the writes in order.
    fn apply<'a>(&'a mut self, w: &'a [Write]) -> BoxFuture<'a, Result<(), DbError>>;
    /// Appends the ops in order and returns the position of the last one. With no ops it
    /// returns the current last position, `ServerSeq(0)` for an empty log.
    fn append_ops<'a>(
        &'a mut self,
        ops: &'a [StoredOp],
    ) -> BoxFuture<'a, Result<ServerSeq, DbError>>;
    /// Returns at most `limit` ops with a position greater than `after`, in position order,
    /// each with `seq` set.
    fn ops_since<'a>(
        &'a mut self,
        after: ServerSeq,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<StoredOp>, DbError>>;
    /// Next number of the sequence `key`, starting at 1. A number handed out by a transaction
    /// that is rolled back may be handed out again.
    fn next_in_sequence<'a>(&'a mut self, key: &'a str) -> BoxFuture<'a, Result<u64, DbError>>;
    fn commit(self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>>;
    fn rollback(self: Box<Self>) -> BoxFuture<'c, Result<(), DbError>>;
}
