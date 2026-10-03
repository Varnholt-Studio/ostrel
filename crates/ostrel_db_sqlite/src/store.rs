//! Tables of this driver and the SQL that reads and writes them.
//!
//! The layout is schema free, like the rows of the contract: no `MigrationStep` exists yet, so
//! the driver cannot know models, fields or their types when it creates tables. Rows, column
//! values and collection entries live in adapter tables keyed by model and field id. The per
//! model tables of docs/db-mapping.md replace this layout once migration steps are defined.
//!
//! Every value is a bound parameter; the only SQL text is the constant text in this file
//! (AC-37).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use ostrel_db::api::{
    AppliedSchema, CollectionChange, CollectionState, DbError, EnumColumn, FieldId, MapEntry,
    MigrationPlan, ModelId, NewOp, RowId, SchemaHash, ServerSeq, SetChange, SetTag, StoredOp,
    Value, Write,
};
use rusqlite::{Connection, OptionalExtension, ToSql, params};

use crate::codec::{self, Corrupt};
use crate::eval::{self, Lookup, Stored};

/// Value of `PRAGMA user_version` for this layout. A file with another non zero value was
/// written by something else and is refused.
pub const LAYOUT_VERSION: i64 = 1;

const CREATE: &str = "
CREATE TABLE IF NOT EXISTS ostrel_rows (
  id      BLOB    PRIMARY KEY CHECK (length(id) = 16),
  model   INTEGER NOT NULL,
  version INTEGER NOT NULL,
  deleted INTEGER NOT NULL CHECK (deleted IN (0, 1))
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS ostrel_rows__live ON ostrel_rows (model, id) WHERE deleted = 0;
CREATE TABLE IF NOT EXISTS ostrel_fields (
  row_id BLOB    NOT NULL,
  field  INTEGER NOT NULL,
  value  BLOB    NOT NULL,
  PRIMARY KEY (row_id, field)
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ostrel_collections (
  row_id BLOB    NOT NULL,
  field  INTEGER NOT NULL,
  kind   INTEGER NOT NULL CHECK (kind IN (0, 1)),
  PRIMARY KEY (row_id, field)
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ostrel_set_tags (
  row_id      BLOB    NOT NULL,
  field       INTEGER NOT NULL,
  k           BLOB    NOT NULL,
  elem        BLOB    NOT NULL,
  tag_replica BLOB    NOT NULL CHECK (length(tag_replica) = 8),
  tag_seq     INTEGER NOT NULL,
  PRIMARY KEY (row_id, field, k, tag_replica)
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ostrel_map_entries (
  row_id BLOB    NOT NULL,
  field  INTEGER NOT NULL,
  k      BLOB    NOT NULL,
  key    BLOB    NOT NULL,
  value  BLOB,
  hlc    BLOB    NOT NULL CHECK (length(hlc) = 16),
  PRIMARY KEY (row_id, field, k)
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ostrel_ops (
  seq     INTEGER PRIMARY KEY AUTOINCREMENT,
  replica BLOB    NOT NULL CHECK (length(replica) = 8),
  op_seq  INTEGER NOT NULL,
  hlc     BLOB    NOT NULL CHECK (length(hlc) = 16),
  model   INTEGER NOT NULL,
  row_id  BLOB    NOT NULL CHECK (length(row_id) = 16),
  body    BLOB    NOT NULL,
  UNIQUE (replica, op_seq)
) STRICT;
CREATE TABLE IF NOT EXISTS ostrel_sequences (
  key  TEXT    PRIMARY KEY,
  last INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ostrel_schema (
  id         INTEGER PRIMARY KEY CHECK (id = 1),
  hash       BLOB    NOT NULL CHECK (length(hash) = 32),
  schema     TEXT    NOT NULL,
  applied_at INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS ostrel_enums (
  model   INTEGER NOT NULL,
  field   INTEGER NOT NULL,
  ordinal INTEGER NOT NULL,
  name    TEXT    NOT NULL,
  PRIMARY KEY (model, field, ordinal)
) STRICT, WITHOUT ROWID;
";

/// Any failure of SQLite. The text is for logs.
pub fn backend(e: rusqlite::Error) -> DbError {
    DbError::Backend(e.to_string())
}

fn corrupt(_: Corrupt) -> DbError {
    DbError::Backend("stored data is damaged".into())
}

fn to_i64(n: u64) -> Result<i64, DbError> {
    i64::try_from(n).map_err(|_| DbError::Backend("number beyond the storage range".into()))
}

fn to_u64(n: i64) -> Result<u64, DbError> {
    u64::try_from(n).map_err(|_| corrupt(Corrupt))
}

fn to_u32(n: i64) -> Result<u32, DbError> {
    u32::try_from(n).map_err(|_| corrupt(Corrupt))
}

/// Creates the tables of an empty file, or checks the layout of a file written before.
/// Runs inside a write transaction of the caller.
pub fn prepare(conn: &Connection) -> Result<(), DbError> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(backend)?;
    match version {
        0 => {
            conn.execute_batch(CREATE).map_err(backend)?;
            conn.execute_batch(&format!("PRAGMA user_version = {LAYOUT_VERSION}"))
                .map_err(backend)
        }
        LAYOUT_VERSION => Ok(()),
        _ => Err(DbError::Unsupported("database file with another layout")),
    }
}

// ---------------------------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------------------------

pub fn applied_schema(conn: &Connection) -> Result<Option<AppliedSchema>, DbError> {
    let row: Option<(Vec<u8>, String)> = conn
        .query_row(
            "SELECT hash, schema FROM ostrel_schema WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(backend)?;
    row.map(|(hash, schema)| {
        let hash: [u8; 32] = hash.try_into().map_err(|_| corrupt(Corrupt))?;
        Ok(AppliedSchema {
            hash: SchemaHash(hash),
            schema,
        })
    })
    .transpose()
}

/// Enum columns of the applied schema (D87), ordered by model and field.
pub fn load_enums(conn: &Connection) -> Result<Vec<EnumColumn>, DbError> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT model, field, ordinal, name FROM ostrel_enums ORDER BY model, field, ordinal",
        )
        .map_err(backend)?;
    let mut rows = stmt.query([]).map_err(backend)?;
    let mut out: Vec<EnumColumn> = Vec::new();
    while let Some(r) = rows.next().map_err(backend)? {
        let (model, field, ordinal, name): (i64, i64, i64, String) = (
            r.get(0).map_err(backend)?,
            r.get(1).map_err(backend)?,
            r.get(2).map_err(backend)?,
            r.get(3).map_err(backend)?,
        );
        let (model, field) = (to_u32(model)?, to_u32(field)?);
        let column = match out.last_mut() {
            Some(c) if c.model == model && c.field == field => c,
            _ => {
                out.push(EnumColumn {
                    model,
                    field,
                    variants: Vec::new(),
                });
                out.last_mut().ok_or(corrupt(Corrupt))?
            }
        };
        // Ordinals are stored from 0 without gaps; anything else is a damaged file.
        if usize::try_from(ordinal).ok() != Some(column.variants.len()) {
            return Err(corrupt(Corrupt));
        }
        column.variants.push(name);
    }
    Ok(out)
}

/// Stores the result of a migration: hash, schema text and enum columns, replacing the old
/// ones. Runs inside a write transaction of the caller.
pub fn store_schema(conn: &Connection, plan: &MigrationPlan, now_ms: i64) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO ostrel_schema (id, hash, schema, applied_at) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT (id) DO UPDATE SET hash = excluded.hash, schema = excluded.schema,
                                        applied_at = excluded.applied_at",
        params![&plan.to.0[..], plan.schema, now_ms],
    )
    .map_err(backend)?;
    conn.execute("DELETE FROM ostrel_enums", [])
        .map_err(backend)?;
    let mut stmt = conn
        .prepare_cached(
            "INSERT INTO ostrel_enums (model, field, ordinal, name) VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(backend)?;
    for c in &plan.enums {
        for (i, name) in c.variants.iter().enumerate() {
            let ordinal = i64::try_from(i).map_err(|_| DbError::Invalid("too many variants"))?;
            stmt.execute(params![c.model, c.field, ordinal, name])
                .map_err(backend)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Reading rows
// ---------------------------------------------------------------------------------------------

/// Which live rows to load.
#[derive(Clone, Copy)]
enum Which {
    Model(ModelId),
    Row(RowId),
}

impl Which {
    /// Condition on `r`, the `ostrel_rows` alias, with `?1` the bound parameter.
    fn clause(self) -> &'static str {
        match self {
            Which::Model(_) => "r.model = ?1 AND r.deleted = 0",
            Which::Row(_) => "r.id = ?1 AND r.deleted = 0",
        }
    }

    fn param(self) -> Box<dyn ToSql> {
        match self {
            Which::Model(m) => Box::new(m),
            Which::Row(id) => Box::new(codec::row_id_bytes(id).to_vec()),
        }
    }
}

fn get<T: rusqlite::types::FromSql>(r: &rusqlite::Row<'_>, i: usize) -> Result<T, DbError> {
    r.get(i).map_err(backend)
}

/// Runs `sql` with the one parameter of `which` and hands every result row to `each`.
fn each_row(
    conn: &Connection,
    sql: &str,
    which: Which,
    mut each: impl FnMut(&rusqlite::Row<'_>) -> Result<(), DbError>,
) -> Result<(), DbError> {
    let mut stmt = conn.prepare_cached(sql).map_err(backend)?;
    let p = which.param();
    let mut rows = stmt.query([p.as_ref()]).map_err(backend)?;
    while let Some(r) = rows.next().map_err(backend)? {
        each(r)?;
    }
    Ok(())
}

fn row_of(rows: &mut BTreeMap<RowId, Stored>, id: RowId) -> Result<&mut Stored, DbError> {
    rows.get_mut(&id).ok_or(corrupt(Corrupt))
}

/// Live rows with their fields and collections, in id order.
fn load(conn: &Connection, which: Which) -> Result<BTreeMap<RowId, Stored>, DbError> {
    let cond = which.clause();
    let mut out: BTreeMap<RowId, Stored> = BTreeMap::new();
    let id_at = |r: &rusqlite::Row<'_>| -> Result<RowId, DbError> {
        codec::row_id_from(&get::<Vec<u8>>(r, 0)?).map_err(corrupt)
    };
    each_row(
        conn,
        &format!(
            "SELECT r.id, r.model, r.version FROM ostrel_rows AS r WHERE {cond} ORDER BY r.id"
        ),
        which,
        |r| {
            out.insert(
                id_at(r)?,
                Stored {
                    model: to_u32(get(r, 1)?)?,
                    version: to_u64(get(r, 2)?)?,
                    fields: BTreeMap::new(),
                    collections: BTreeMap::new(),
                },
            );
            Ok(())
        },
    )?;
    if out.is_empty() {
        return Ok(out);
    }
    each_row(
        conn,
        &format!(
            "SELECT f.row_id, f.field, f.value FROM ostrel_fields AS f
             JOIN ostrel_rows AS r ON r.id = f.row_id WHERE {cond}"
        ),
        which,
        |r| {
            let value = codec::decode_value(&get::<Vec<u8>>(r, 2)?).map_err(corrupt)?;
            row_of(&mut out, id_at(r)?)?
                .fields
                .insert(to_u32(get(r, 1)?)?, value);
            Ok(())
        },
    )?;
    each_row(
        conn,
        &format!(
            "SELECT c.row_id, c.field, c.kind FROM ostrel_collections AS c
             JOIN ostrel_rows AS r ON r.id = c.row_id WHERE {cond}"
        ),
        which,
        |r| {
            let state = match get::<i64>(r, 2)? {
                0 => CollectionState::Set(Vec::new()),
                _ => CollectionState::Map(Vec::new()),
            };
            row_of(&mut out, id_at(r)?)?
                .collections
                .insert(to_u32(get(r, 1)?)?, state);
            Ok(())
        },
    )?;
    // Tags in element order (compare_key through the order key), then by tag.
    each_row(
        conn,
        &format!(
            "SELECT s.row_id, s.field, s.elem, s.tag_replica, s.tag_seq FROM ostrel_set_tags AS s
             JOIN ostrel_rows AS r ON r.id = s.row_id WHERE {cond}
             ORDER BY s.row_id, s.field, s.k, s.tag_replica"
        ),
        which,
        |r| {
            let elem = codec::decode_value(&get::<Vec<u8>>(r, 2)?).map_err(corrupt)?;
            let tag = codec::op_id_from(&get::<Vec<u8>>(r, 3)?, get(r, 4)?).map_err(corrupt)?;
            let field = to_u32(get(r, 1)?)?;
            match row_of(&mut out, id_at(r)?)?.collections.get_mut(&field) {
                Some(CollectionState::Set(tags)) => tags.push(SetTag { elem, tag }),
                _ => return Err(corrupt(Corrupt)),
            }
            Ok(())
        },
    )?;
    each_row(
        conn,
        &format!(
            "SELECT m.row_id, m.field, m.key, m.value, m.hlc FROM ostrel_map_entries AS m
             JOIN ostrel_rows AS r ON r.id = m.row_id WHERE {cond}
             ORDER BY m.row_id, m.field, m.k"
        ),
        which,
        |r| {
            let key = codec::decode_value(&get::<Vec<u8>>(r, 2)?).map_err(corrupt)?;
            let value = get::<Option<Vec<u8>>>(r, 3)?
                .map(|b| codec::decode_value(&b))
                .transpose()
                .map_err(corrupt)?;
            let hlc = codec::hlc_from(&get::<Vec<u8>>(r, 4)?).map_err(corrupt)?;
            let field = to_u32(get(r, 1)?)?;
            match row_of(&mut out, id_at(r)?)?.collections.get_mut(&field) {
                Some(CollectionState::Map(entries)) => entries.push(MapEntry { key, value, hlc }),
                _ => return Err(corrupt(Corrupt)),
            }
            Ok(())
        },
    )?;
    Ok(out)
}

/// Rows read through hops, each loaded at most once per query.
struct Rows<'c> {
    conn: &'c Connection,
    cache: RefCell<HashMap<RowId, Option<Rc<Stored>>>>,
}

impl Lookup for Rows<'_> {
    fn live(&self, id: RowId) -> Result<Option<Rc<Stored>>, DbError> {
        if let Some(hit) = self.cache.borrow().get(&id) {
            return Ok(hit.clone());
        }
        let row = load(self.conn, Which::Row(id))?.remove(&id).map(Rc::new);
        self.cache.borrow_mut().insert(id, row.clone());
        Ok(row)
    }
}

/// Runs a query on what `conn` sees. [`ostrel_db::api::Query::check`] runs first.
pub fn query(
    conn: &Connection,
    enums: &[EnumColumn],
    q: &ostrel_db::api::Query,
) -> Result<ostrel_db::api::Rows, DbError> {
    q.check(enums)?;
    let rows: Vec<(RowId, Rc<Stored>)> = load(conn, Which::Model(q.model))?
        .into_iter()
        .map(|(id, r)| (id, Rc::new(r)))
        .collect();
    let look = Rows {
        conn,
        cache: RefCell::new(
            rows.iter()
                .map(|(id, r)| (*id, Some(Rc::clone(r))))
                .collect(),
        ),
    };
    eval::run(enums, &look, q, rows)
}

// ---------------------------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------------------------

/// The stored form of a value, or `Invalid` when it is nested too deeply to store.
fn stored(v: &Value) -> Result<Vec<u8>, DbError> {
    codec::encode_value(v).ok_or(DbError::Invalid("value nested too deeply"))
}

fn key(v: &Value) -> Result<Vec<u8>, DbError> {
    codec::order_key(v).ok_or(DbError::Invalid("value nested too deeply"))
}

/// Refuses values of enum columns, and of `Map` fields whose values are enums, that are not
/// declared variants (contract rules in `ostrel_db::api`). `Set` elements and `Map` keys are
/// not checked (D87).
pub fn check_enums(enums: &[EnumColumn], writes: &[Write]) -> Result<(), DbError> {
    let declared = |model: ModelId, f: FieldId, v: &Value| match eval::variants(enums, model, f) {
        Some(vs) if *v != Value::Null => eval::ordinal(vs, v).is_some(),
        _ => true,
    };
    for w in writes {
        let (Write::Insert {
            model,
            fields,
            collections,
            ..
        }
        | Write::Update {
            model,
            fields,
            collections,
            ..
        }) = w
        else {
            continue;
        };
        let bad_field = fields.iter().any(|(f, v)| !declared(*model, *f, v));
        let bad_value = collections.iter().any(|(f, c)| match c {
            CollectionChange::Map(entries) => entries
                .iter()
                .filter_map(|e| e.value.as_ref())
                .any(|v| !declared(*model, *f, v)),
            CollectionChange::Set(_) => false,
        });
        if bad_field || bad_value {
            return Err(DbError::Invalid("enum value is not a declared variant"));
        }
    }
    Ok(())
}

/// Refuses values this driver cannot store (nesting beyond [`codec::MAX_DEPTH`]) before the
/// first write, so such a batch is refused like any other invalid batch.
pub fn check_storable(writes: &[Write]) -> Result<(), DbError> {
    for w in writes {
        let (Write::Insert {
            fields,
            collections,
            ..
        }
        | Write::Update {
            fields,
            collections,
            ..
        }) = w
        else {
            continue;
        };
        for (_, v) in fields {
            stored(v)?;
        }
        for (_, c) in collections {
            match c {
                CollectionChange::Set(changes) => {
                    for ch in changes {
                        let (SetChange::Add { elem, .. } | SetChange::Remove { elem, .. }) = ch;
                        stored(elem)?;
                        key(elem)?;
                    }
                }
                CollectionChange::Map(entries) => {
                    for e in entries {
                        stored(&e.key)?;
                        key(&e.key)?;
                        if let Some(v) = &e.value {
                            stored(v)?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Applies one write. The caller runs it inside a savepoint and has run `check_writes`,
/// [`check_enums`] and [`check_storable`] on the batch.
pub fn apply_write(conn: &Connection, w: &Write) -> Result<(), DbError> {
    match w {
        Write::Insert {
            model,
            row,
            fields,
            collections,
        } => {
            let id = codec::row_id_bytes(*row);
            // The id is taken while a live row or a tombstone holds it, in any model.
            let added = conn
                .prepare_cached(
                    "INSERT INTO ostrel_rows (id, model, version, deleted) VALUES (?1, ?2, 1, 0)
                     ON CONFLICT (id) DO NOTHING",
                )
                .and_then(|mut s| s.execute(params![&id[..], model]))
                .map_err(backend)?;
            if added == 0 {
                return Err(DbError::Conflict);
            }
            write_fields(conn, &id, fields)?;
            for (f, c) in collections {
                apply_collection(conn, &id, *f, c)?;
            }
        }
        Write::Update {
            model,
            row,
            expect_version,
            fields,
            collections,
        } => {
            let id = codec::row_id_bytes(*row);
            bump_version(conn, &id, *model, *expect_version, false)?;
            write_fields(conn, &id, fields)?;
            for (f, c) in collections {
                apply_collection(conn, &id, *f, c)?;
            }
        }
        Write::Delete {
            model,
            row,
            expect_version,
        } => {
            let id = codec::row_id_bytes(*row);
            bump_version(conn, &id, *model, *expect_version, true)?;
            for sql in [
                "DELETE FROM ostrel_fields WHERE row_id = ?1",
                "DELETE FROM ostrel_collections WHERE row_id = ?1",
                "DELETE FROM ostrel_set_tags WHERE row_id = ?1",
                "DELETE FROM ostrel_map_entries WHERE row_id = ?1",
            ] {
                conn.prepare_cached(sql)
                    .and_then(|mut s| s.execute([&id[..]]))
                    .map_err(backend)?;
            }
        }
    }
    Ok(())
}

/// Checks that `id` is a live row of `model` at `expect_version` and increases its version by
/// one; `delete` also turns it into a tombstone.
fn bump_version(
    conn: &Connection,
    id: &[u8],
    model: ModelId,
    expect_version: u64,
    delete: bool,
) -> Result<(), DbError> {
    let found: Option<(i64, i64, i64)> = conn
        .prepare_cached("SELECT model, version, deleted FROM ostrel_rows WHERE id = ?1")
        .and_then(|mut s| {
            s.query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()
        })
        .map_err(backend)?;
    let version = match found {
        Some((m, v, 0)) if m == i64::from(model) => to_u64(v)?,
        _ => return Err(DbError::NotFound),
    };
    if version != expect_version {
        return Err(DbError::VersionMismatch);
    }
    let next = version
        .checked_add(1)
        .ok_or(DbError::Backend("row version exhausted".into()))?;
    let next = to_i64(next)?;
    conn.prepare_cached("UPDATE ostrel_rows SET version = ?2, deleted = ?3 WHERE id = ?1")
        .and_then(|mut s| s.execute(params![id, next, i64::from(delete)]))
        .map_err(backend)?;
    Ok(())
}

fn write_fields(conn: &Connection, id: &[u8], fields: &[(FieldId, Value)]) -> Result<(), DbError> {
    let mut stmt = conn
        .prepare_cached(
            "INSERT INTO ostrel_fields (row_id, field, value) VALUES (?1, ?2, ?3)
             ON CONFLICT (row_id, field) DO UPDATE SET value = excluded.value",
        )
        .map_err(backend)?;
    for (f, v) in fields {
        stmt.execute(params![id, f, stored(v)?]).map_err(backend)?;
    }
    Ok(())
}

/// Applies the changes of one collection field. A field stored as the other collection kind
/// starts over as the new kind; the compiler never emits that.
fn apply_collection(
    conn: &Connection,
    id: &[u8],
    field: FieldId,
    change: &CollectionChange,
) -> Result<(), DbError> {
    let kind: i64 = match change {
        CollectionChange::Set(_) => 0,
        CollectionChange::Map(_) => 1,
    };
    let old: Option<i64> = conn
        .prepare_cached("SELECT kind FROM ostrel_collections WHERE row_id = ?1 AND field = ?2")
        .and_then(|mut s| s.query_row(params![id, field], |r| r.get(0)).optional())
        .map_err(backend)?;
    if old != Some(kind) {
        for sql in [
            "DELETE FROM ostrel_set_tags WHERE row_id = ?1 AND field = ?2",
            "DELETE FROM ostrel_map_entries WHERE row_id = ?1 AND field = ?2",
        ] {
            conn.prepare_cached(sql)
                .and_then(|mut s| s.execute(params![id, field]))
                .map_err(backend)?;
        }
        conn.prepare_cached(
            "INSERT INTO ostrel_collections (row_id, field, kind) VALUES (?1, ?2, ?3)
             ON CONFLICT (row_id, field) DO UPDATE SET kind = excluded.kind",
        )
        .and_then(|mut s| s.execute(params![id, field, kind]))
        .map_err(backend)?;
    }
    match change {
        CollectionChange::Set(changes) => {
            for c in changes {
                apply_set_change(conn, id, field, c)?;
            }
        }
        CollectionChange::Map(entries) => {
            let mut stmt = conn
                .prepare_cached(
                    "INSERT INTO ostrel_map_entries (row_id, field, k, key, value, hlc)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT (row_id, field, k) DO UPDATE SET key = excluded.key,
                       value = excluded.value, hlc = excluded.hlc",
                )
                .map_err(backend)?;
            for e in entries {
                let value = e.value.as_ref().map(stored).transpose()?;
                stmt.execute(params![
                    id,
                    field,
                    key(&e.key)?,
                    stored(&e.key)?,
                    value,
                    &codec::hlc_bytes(e.hlc)[..]
                ])
                .map_err(backend)?;
            }
        }
    }
    Ok(())
}

/// One tag change of a `Set` field: at most one live tag per (element, replica) (D49).
fn apply_set_change(
    conn: &Connection,
    id: &[u8],
    field: FieldId,
    change: &SetChange,
) -> Result<(), DbError> {
    let (SetChange::Add { elem, tag } | SetChange::Remove { elem, tag }) = change;
    let k = key(elem)?;
    let replica = codec::replica_bytes(tag.replica);
    match change {
        SetChange::Add { .. } => {
            let elem = stored(elem)?;
            // A replay of this add or of an older add of the same replica changes nothing; a
            // newer one replaces the tag and keeps the stored element.
            conn.prepare_cached(
                "INSERT INTO ostrel_set_tags (row_id, field, k, elem, tag_replica, tag_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (row_id, field, k, tag_replica) DO UPDATE SET tag_seq = excluded.tag_seq
                 WHERE excluded.tag_seq > ostrel_set_tags.tag_seq",
            )
            .and_then(|mut s| s.execute(params![id, field, k, elem, &replica[..], tag.seq]))
            .map_err(backend)?;
        }
        SetChange::Remove { .. } => {
            // Only the full tuple (row, field, element, replica, seq) names a live tag; a tag of
            // another element is ignored like an unknown one (D96).
            conn.prepare_cached(
                "DELETE FROM ostrel_set_tags
                 WHERE row_id = ?1 AND field = ?2 AND k = ?3 AND tag_replica = ?4 AND tag_seq = ?5",
            )
            .and_then(|mut s| s.execute(params![id, field, k, &replica[..], tag.seq]))
            .map_err(backend)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Op log and sequences
// ---------------------------------------------------------------------------------------------

fn last_seq(conn: &Connection) -> Result<ServerSeq, DbError> {
    let last: i64 = conn
        .query_row("SELECT coalesce(max(seq), 0) FROM ostrel_ops", [], |r| {
            r.get(0)
        })
        .map_err(backend)?;
    Ok(ServerSeq(to_u64(last)?))
}

/// Appends the ops in order. Every id is checked before the first insert, so a refused call
/// appends nothing.
pub fn append_ops(conn: &Connection, ops: &[NewOp]) -> Result<ServerSeq, DbError> {
    let mut seen = BTreeSet::new();
    let mut exists = conn
        .prepare_cached("SELECT 1 FROM ostrel_ops WHERE replica = ?1 AND op_seq = ?2")
        .map_err(backend)?;
    for op in ops {
        let replica = codec::replica_bytes(op.id.replica);
        let in_log = exists
            .exists(params![&replica[..], op.id.seq])
            .map_err(backend)?;
        if in_log || !seen.insert(op.id) {
            return Err(DbError::Conflict);
        }
    }
    let mut insert = conn
        .prepare_cached(
            "INSERT INTO ostrel_ops (replica, op_seq, hlc, model, row_id, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(backend)?;
    for op in ops {
        insert
            .execute(params![
                &codec::replica_bytes(op.id.replica)[..],
                op.id.seq,
                &codec::hlc_bytes(op.hlc)[..],
                op.model,
                &codec::row_id_bytes(op.row)[..],
                &op.body[..]
            ])
            .map_err(backend)?;
    }
    last_seq(conn)
}

/// At most `limit` ops after the position `after`, in position order.
pub fn ops_since(
    conn: &Connection,
    after: ServerSeq,
    limit: u32,
) -> Result<Vec<StoredOp>, DbError> {
    // Positions are stored as signed 64 bit integers; nothing lies after the largest one.
    let Ok(after) = i64::try_from(after.0) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn
        .prepare_cached(
            "SELECT seq, replica, op_seq, hlc, model, row_id, body FROM ostrel_ops
             WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )
        .map_err(backend)?;
    let mut rows = stmt.query(params![after, limit]).map_err(backend)?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(backend)? {
        out.push(StoredOp {
            seq: ServerSeq(to_u64(get(r, 0)?)?),
            op: NewOp {
                id: codec::op_id_from(&get::<Vec<u8>>(r, 1)?, get(r, 2)?).map_err(corrupt)?,
                hlc: codec::hlc_from(&get::<Vec<u8>>(r, 3)?).map_err(corrupt)?,
                model: to_u32(get(r, 4)?)?,
                row: codec::row_id_from(&get::<Vec<u8>>(r, 5)?).map_err(corrupt)?,
                body: get(r, 6)?,
            },
        });
    }
    Ok(out)
}

/// Next number of the sequence `key`, starting at 1.
pub fn next_in_sequence(conn: &Connection, key: &str) -> Result<u64, DbError> {
    let last: Option<i64> = conn
        .prepare_cached("SELECT last FROM ostrel_sequences WHERE key = ?1")
        .and_then(|mut s| s.query_row([key], |r| r.get(0)).optional())
        .map_err(backend)?;
    // SQLite would turn the overflow into a REAL, which the STRICT table refuses with an
    // unclear error; say what happened instead.
    let next = match last {
        None => 1,
        Some(n) => n
            .checked_add(1)
            .ok_or(DbError::Backend("sequence exhausted".into()))?,
    };
    conn.prepare_cached(
        "INSERT INTO ostrel_sequences (key, last) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET last = excluded.last",
    )
    .and_then(|mut s| s.execute(params![key, next]))
    .map_err(backend)?;
    to_u64(next)
}
