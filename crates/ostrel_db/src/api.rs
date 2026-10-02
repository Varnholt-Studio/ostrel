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
//!   [`Write::Update`] or [`Write::Delete`] increases it by one, also when the update only
//!   changes collection entries or nothing at all.
//! * A driver checks every write batch with [`check_writes`] and every query with
//!   [`Query::check`] before it touches the database, so all drivers refuse the same malformed
//!   input with the same [`DbError::Invalid`].
//! * Enum typed columns are the ones listed in [`MigrationPlan::enums`] of the applied schema.
//!   A write that puts a value other than a declared variant name (or `Null`) into such a
//!   column, or into the value of such a `Map` field, fails with [`DbError::Invalid`] and
//!   changes nothing.
//! * After a transaction method returned an error, the caller rolls the transaction back. A
//!   driver may refuse every further call except [`Transaction::rollback`].
//! * Dropping a transaction without commit has the effect of a rollback.
//!
//! Ids and values are the shared types of `ostrel_core` (F1b), re-exported here so that a
//! driver crate needs only this crate.

mod query;
mod write;

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub use ostrel_core::ids::{Hlc, OpId, ReplicaId, RowId, ServerSeq};
pub use ostrel_core::value::{Value, compare_key};

pub use query::{CmpOp, Cursor, Dir, Expr, Hop, MAX_FILTER_NODES, MAX_HOPS, Path, Query};
pub use write::{CollectionChange, MapEntry, SetChange, Write, check_writes};

/// A boxed future that may borrow for `'a`. Not `Send`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Index of a model in the compiled schema.
pub type ModelId = u32;

/// Index of a field within its model in the compiled schema. The implicit fields `made`,
/// `changed` and `author` have field ids like every declared field; the row id has none and is
/// addressed by [`Expr::RowId`] and [`Row::id`].
pub type FieldId = u32;

/// Errors a driver reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DbError {
    /// [`Write::Insert`] of an id that exists or existed in any model, or
    /// [`Transaction::append_ops`] of an op id that is already in the log or twice in the call.
    Conflict,
    /// [`Write::Update`] or [`Write::Delete`] whose `expect_version` is not the stored version.
    VersionMismatch,
    /// [`Write::Update`] or [`Write::Delete`] of a row that does not exist in that model or was
    /// deleted.
    NotFound,
    /// [`Connection::migrate`] with a plan whose `from` is not the schema hash stored in the
    /// database. Nothing was changed.
    SchemaMismatch,
    /// A write or query that breaks a rule of this contract, found by [`check_writes`] or
    /// [`Query::check`]. The text names the rule; it is for logs, not for end users.
    Invalid(&'static str),
    /// The driver does not support the requested operation or connection URL.
    Unsupported(&'static str),
    /// Any other failure of the database. The text is for logs, not for end users.
    Backend(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Conflict => f.write_str("id exists or existed"),
            DbError::VersionMismatch => f.write_str("row version does not match"),
            DbError::NotFound => f.write_str("row not found"),
            DbError::SchemaMismatch => f.write_str("migration plan starts from another schema"),
            DbError::Invalid(rule) => write!(f, "invalid request: {rule}"),
            DbError::Unsupported(what) => write!(f, "unsupported: {what}"),
            DbError::Backend(msg) => write!(f, "database error: {msg}"),
        }
    }
}

impl std::error::Error for DbError {}

/// An op to append to the server op log. It has no position yet; the driver assigns one.
#[derive(Clone, Debug, PartialEq)]
pub struct NewOp {
    /// Identity of the op. Unique in the log.
    pub id: OpId,
    /// Server accepted stamp of the op.
    pub hlc: Hlc,
    pub model: ModelId,
    pub row: RowId,
    /// Canonical JSON of the op (ARCHITECTURE 5.3), stored and returned unchanged.
    pub body: Vec<u8>,
}

/// One entry of the server op log, with the position the driver assigned.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredOp {
    pub seq: ServerSeq,
    pub op: NewOp,
}

/// One live tag of a `Set` field: the element and the id of the op that added it (D49).
#[derive(Clone, Debug, PartialEq)]
pub struct SetTag {
    pub elem: Value,
    pub tag: OpId,
}

/// Stored state of one collection field of a row.
#[derive(Clone, Debug, PartialEq)]
pub enum CollectionState {
    /// Live tags, ordered by element with [`compare_key`], then by tag. An element is in the
    /// set while it has at least one live tag. Empty when every element was removed.
    Set(Vec<SetTag>),
    /// Every entry ever written, including removed ones (`value` is `None`), ordered by key
    /// with [`compare_key`].
    Map(Vec<MapEntry>),
}

/// One live row as returned by a query.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: RowId,
    pub version: u64,
    /// Every column field that was ever written for this row, also when it was written as
    /// [`Value::Null`], in ascending field order. A field that was never written is absent and
    /// counts as [`Value::Null`] in filters and sorting.
    pub fields: Vec<(FieldId, Value)>,
    /// Every collection field that was ever changed for this row, in ascending field order.
    pub collections: Vec<(FieldId, CollectionState)>,
}

/// Result of a query, in query order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rows(pub Vec<Row>);

/// Hash of a compiled model schema, computed by the planner (ARCHITECTURE 6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SchemaHash(pub [u8; 32]);

/// One step of a migration. No steps are defined yet; they are computed by the planner from
/// the model schema and executed by the driver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrationStep {}

/// Steps that bring a database from the schema `from` to the schema `to`.
///
/// [`Connection::migrate`] first checks the plan with [`MigrationPlan::check`] and then
/// compares `from` with the hash stored by the last successful migration (`None` for a
/// database that was never migrated). If they differ it returns [`DbError::SchemaMismatch`]
/// and changes nothing. Otherwise it runs every step and stores `to`, `schema` and `enums` in
/// one transaction, so that [`Connection::applied_schema`] returns them. A plan with
/// `from == Some(to)` and no steps succeeds without changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationPlan {
    pub from: Option<SchemaHash>,
    pub to: SchemaHash,
    /// Canonical JSON of the target schema, stored for the planner and for readers of the
    /// database.
    pub schema: String,
    /// Every enum typed column of the target schema with its variants (D79). This is where a
    /// driver learns the declaration order it sorts and compares enum columns by; it never
    /// parses `schema`.
    pub enums: Vec<EnumColumn>,
    pub steps: Vec<MigrationStep>,
}

/// An enum typed column of a model: a field whose type is an enum or an optional enum, or a
/// `Map` field whose value type is one (D79: map values are stored like columns). `Set`
/// elements and `Map` keys are not listed here; they are ordered by variant name (D61).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumColumn {
    pub model: ModelId,
    pub field: FieldId,
    /// Variant names in declaration order. The position is the ordinal a driver may store.
    pub variants: Vec<String>,
}

impl MigrationPlan {
    /// Checks the rules a driver must not have to guess about: every [`EnumColumn`] has at
    /// least one variant, no variant name twice, and no (model, field) is listed twice.
    pub fn check(&self) -> Result<(), DbError> {
        let mut columns = BTreeSet::new();
        for c in &self.enums {
            if !columns.insert((c.model, c.field)) {
                return Err(DbError::Invalid("enum column listed twice"));
            }
            if c.variants.is_empty() {
                return Err(DbError::Invalid("enum column without variants"));
            }
            let mut names = BTreeSet::new();
            if !c.variants.iter().all(|v| names.insert(v.as_str())) {
                return Err(DbError::Invalid("enum variant listed twice"));
            }
        }
        Ok(())
    }

    /// Declaration order of the enum column `(model, field)`, if the plan lists it.
    pub fn enum_variants(&self, model: ModelId, field: FieldId) -> Option<&[String]> {
        self.enums
            .iter()
            .find(|c| c.model == model && c.field == field)
            .map(|c| c.variants.as_slice())
    }
}

/// What the last successful [`Connection::migrate`] stored (D79).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedSchema {
    /// [`MigrationPlan::to`] of that migration.
    pub hash: SchemaHash,
    /// [`MigrationPlan::schema`] of that migration, unchanged.
    pub schema: String,
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
    /// Applies the plan atomically, see [`MigrationPlan`].
    fn migrate<'a>(&'a mut self, plan: &'a MigrationPlan) -> BoxFuture<'a, Result<(), DbError>>;
    /// Reads committed state.
    fn query<'a>(&'a mut self, q: &'a Query) -> BoxFuture<'a, Result<Rows, DbError>>;
    /// Hash and canonical JSON stored by the last successful [`Connection::migrate`]; `None`
    /// if the database was never migrated. A refused plan leaves it unchanged. The planner
    /// computes the steps of the next plan from it (D79).
    fn applied_schema<'a>(&'a mut self) -> BoxFuture<'a, Result<Option<AppliedSchema>, DbError>>;
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
    /// returns the current last position, `ServerSeq(0)` for an empty log. An op id that is
    /// already in the log or appears twice in `ops` gives [`DbError::Conflict`], and no op of
    /// the call is appended.
    fn append_ops<'a>(&'a mut self, ops: &'a [NewOp]) -> BoxFuture<'a, Result<ServerSeq, DbError>>;
    /// Returns at most `limit` ops with a position greater than `after`, in position order.
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
