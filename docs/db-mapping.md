# SQLite schema mapping

Status: draft for review, work package T6-3 (ARCHITECTURE 15.3). Refs #23. Aligned with the F1b
contract of `crates/ostrel_db/src/api.rs` (T6-1b, Refs #69): error names, `Set` tags (D49), change
sets, `NULL` order, cursor and schema hash.
Scope: how `ostrel_db_sqlite` stores models, collections, the op log and sequences, and how it plans
queries, including `last n`. Inputs: ARCHITECTURE 5.1 (type table), 5.3 (ids), 6.1 and 6.2 (DB
interface), 9 (pushdown subset); SYNTAX 4.2 to 4.5; SPEC AC-10, AC-31, AC-33 (e), AC-37, AC-56.

This document describes behaviour the SQLite adapter must have. It does not change the DB
interface. Where the interface leaves something open, the gap is written down as an `ASSUMPTION`
(section 10) and listed as an open question (section 11), so the next reader does not have to guess.

Minimum SQLite version: 3.45 (the build machine ships 3.45.1). Features used: `STRICT` tables
(3.37), `RETURNING` (3.35), `ALTER TABLE ... DROP COLUMN` (3.35), `NULLS FIRST` (3.30), row value
comparisons (3.15), partial indexes. The adapter checks `sqlite_version()` on `connect` and fails
with a clear error below 3.45.

## 1. Principles

1. **Readable by other tools.** Materialised rows stay plain tables with the names from the source
   program (ARCHITECTURE 6.2). A developer who opens the file in any SQLite client recognises their
   models. A view per model (section 2.6) shows ids as hex and times as numbers.
2. **Identifiers from the schema, values only as parameters.** Table and column names come from
   the compiled schema and are always double quoted. Every value is a bound parameter. No value is
   ever formatted into SQL text (AC-37). This includes sequence keys, ids and enum ordinals.
3. **One byte order everywhere.** Ids and HLC stamps are stored as big endian blobs, so SQLite's
   byte comparison gives the same order as the Rust and JS comparisons.
4. **Type affinity is not trusted.** Every table is `STRICT`, so SQLite rejects a value of the
   wrong storage class instead of silently converting it.
5. **The adapter stores, it does not merge.** Merging is done by `ostrel_crdt` and `ostrel_sync`
   before a `Write` reaches the driver. The driver persists merged state and checks versions.

## 2. Models

### 2.1 Names

| Source item | SQLite object | Example |
|---|---|---|
| `data Room` | table `"Room"` | `"Room"` |
| scalar field `name` | column `"name"` | `"Room"."name"` |
| `Set` or `Map` field `members` | side table `"Room__members"` | section 3 |
| `Text merge text` field `body` | columns `"body"` and `"body__crdt"` | section 3.4 |
| implicit fields | columns `"id"`, `"made"`, `"changed"`, `"author"` | section 2.3 |
| row version | column `"ostrel_version"` | section 2.4 |
| adapter tables | prefix `ostrel_` | section 4 |

Collisions that the planner (`ostrel_db::plan`) must reject with a clear error before any SQL runs:

* SQLite identifiers are case insensitive. Ostrel names are case sensitive, so two fields `url` and
  `Url` in one model, or two models `Room` and `ROOM`, map to the same SQLite name. The formatter
  lint makes this unlikely (SYNTAX 3), but the plan check is what guarantees it.
* A side table name `<Model>__<field>` must not equal another model name. Model names are
  `UpperCamel` and contain no `__`, so this only matters if the lint is bypassed.
* Model names starting with `ostrel_` (any case) are reserved for adapter tables.

### 2.2 Column types (ARCHITECTURE 5.1)

| Ostrel type | Column type | Stored value | Order in `sort` |
|---|---|---|---|
| `Bool` | `INTEGER` with `CHECK (x IN (0, 1))` | 0 or 1 | false before true |
| `Int` | `INTEGER` | 64 bit integer, range already checked to plus and minus (2^53 minus 1) (G9) | numeric |
| `Float` | `REAL` | NaN and infinities rejected before the driver | numeric |
| `Text` | `TEXT` | UTF-8, not normalised | `BINARY` collation (section 6.4) |
| `Time` | `INTEGER` | ms since epoch | numeric |
| `Bytes` | `BLOB` | raw bytes (base64url is only the wire form) | bytewise |
| enum | `INTEGER` | ordinal in declaration order, from 0 | declaration order |
| reference `T` | `BLOB` with `CHECK (length(x) = 16)` | `RowId`, 16 bytes big endian | by id |
| `T?` | as `T`, nullable | `NULL` for `none` | `NULL` first ascending, last descending (6.4) |
| `List[T]` | `TEXT` | canonical JSON of the whole value (ARCHITECTURE 5.3) | not sortable |
| `Rank` | `TEXT` | fractional index key | bytewise |
| `Int serial [per f]` | `INTEGER`, nullable | `NULL` until the server assigns it | numeric |
| `Set[T]`, `Map[K, V]` | no column; side table | section 3 | not sortable |

Non optional fields are `NOT NULL`. A field default (`= backlog`, `= {me}`) is applied by the
runtime when it builds the `make`; it is not a SQL `DEFAULT`, because defaults such as `me` and
`now` depend on the request. The only SQL defaults are the backfill values of section 5.

Enums are stored as ordinals so that rule comparisons such as `(team.roles[me] ?? guest) >= member`
compile to an indexable integer comparison. The wire format keeps the variant name (ARCHITECTURE
5.1); the adapter converts using the schema. Table `ostrel_enums` (section 4) lists every ordinal
with its name for readers of the file.

### 2.3 Implicit fields

| Field | Column | Type | Written by |
|---|---|---|---|
| `id` | `"id"` | `BLOB PRIMARY KEY`, 16 bytes | runtime, at `make` |
| `made` | `"made"` | `BLOB NOT NULL`, 16 byte HLC | server accepted stamp of the `make` op |
| `changed` | `"changed"` | `BLOB NOT NULL`, 16 byte HLC | server accepted stamp of the latest op on the row |
| `author` | `"author"` | `BLOB NOT NULL`, 16 bytes; only in apps with `auth` | server, from the session (G8) |

`pending` and `rejected` exist only on the client and have no column.

Byte layout, shared by `RowId` and `Hlc` (ARCHITECTURE 5.3): `wall_ms` as 6 bytes, then `counter`
as 2 bytes, then `replica` as 8 bytes, all big endian. Byte comparison of two blobs then equals the
order `(wall_ms, counter, replica)`, which is the HLC total order and the time order of ids.

`made` and `changed` are `Time` values in the language. A comparison with a time `t` in ms is
translated to a comparison with a 16 byte bound, so the column stays the single source:

| Source | SQL |
|---|---|
| `made < t` | `"made" < bound(t)` |
| `made >= t` | `"made" >= bound(t)` |
| `made <= t` | `"made" < bound(t + 1)` |
| `made > t` | `"made" >= bound(t + 1)` |
| `made == t` | `"made" >= bound(t) AND "made" < bound(t + 1)` |

where `bound(t)` is the blob of `(wall_ms = t, counter = 0, replica = 0)`, the smallest stamp in
millisecond `t`. The example `changed < now - days.days` (SYNTAX 8) becomes `"changed" < ?`.

### 2.4 Version and tombstones

Every model table has `"ostrel_version" INTEGER NOT NULL`, 1 after insert, increased by 1 on every
update (ARCHITECTURE 6.1, `expect_version`).

Deleted rows leave the model table and its side tables. The id stays reserved forever in the
registry `ostrel_rows` (section 4), which is also how a `make` with an id that exists or existed in
any model is refused (D20, AC-31).

### 2.5 Writes

All writes run inside a transaction started with `BEGIN IMMEDIATE` (section 7).

**`Write::Insert`**

1. `INSERT INTO ostrel_rows (id, model, deleted) VALUES (?, ?, 0)`. A primary key violation means
   the id exists or existed in some model: the driver returns `DbError::Conflict` before step 2.
   Nothing else is touched, so a tombstoned row is never resurrected.
2. `INSERT INTO "Model" ("id", "made", "changed", "author", <fields>, "ostrel_version") VALUES (?, ..., 1)`.
3. One `INSERT` per element or key into each side table (section 3).

**`Write::Update`**

1. `UPDATE "Model" SET <fields> = ?, "ostrel_version" = "ostrel_version" + 1 WHERE "id" = ? AND "ostrel_version" = ?`.
2. If no row changed, the driver distinguishes with one lookup in `ostrel_rows`: row unknown or
   deleted gives `DbError::NotFound`, otherwise the version did not match and the driver returns
   `DbError::VersionMismatch`, which `ostrel_sync` turns into a retry (ARCHITECTURE 5.6).
3. Side table entries carried by the write are upserted (section 3.3). The version bump in step 1
   also applies when only side table entries change, so `expect_version` protects collections too.

**`Write::Delete`**

1. `DELETE FROM "Model" WHERE "id" = ? AND "ostrel_version" = ?`, same error handling as update.
   Side table rows go with it through `ON DELETE CASCADE`.
2. `UPDATE ostrel_rows SET deleted = 1 WHERE id = ?`.

References are not foreign keys. A referenced row may be deleted later (a dropped room keeps its
messages until they are dropped), and the server already checks every reference value when it is
written (ARCHITECTURE 5.5, step 5). Section 6.3 says how queries treat a reference to a deleted row.

### 2.6 Read view for people

For each model the migration creates a view `"Model__view"` with ids and references as lowercase
hex (`lower(hex("id"))`), `made` and `changed` as ms numbers, enums as names and side table fields
as JSON arrays. The adapter never reads the view; it exists so that a person inspecting the file
does not need to decode blobs by hand.

## 3. Collections

### 3.1 `Set[T]`

Observed remove set with add tags (D49, ARCHITECTURE 6.2). A row of the side table is one live
tag; there are no tombstones.

```sql
CREATE TABLE "Room__members" (
  row_id      BLOB    NOT NULL REFERENCES "Room" ("id") ON DELETE CASCADE,
  elem        BLOB    NOT NULL,      -- column type of T, here a reference
  tag_replica BLOB    NOT NULL CHECK (length(tag_replica) = 8),
  tag_seq     INTEGER NOT NULL,      -- OpId.seq of the add
  PRIMARY KEY (row_id, elem, tag_replica)
) STRICT, WITHOUT ROWID;
CREATE INDEX "Room__members__by_elem" ON "Room__members" (elem, row_id);
```

The primary key answers "is `me` in this room" (`see` and `edit` rules) and keeps at most one live
tag per (element, replica). The index answers "which rooms contain `me`" for list queries filtered
by membership. An element is in the set while it has at least one row.

### 3.2 `Map[K, V]`

```sql
CREATE TABLE "Team__roles" (
  row_id  BLOB    NOT NULL REFERENCES "Team" ("id") ON DELETE CASCADE,
  key     BLOB    NOT NULL,          -- column type of K
  value   INTEGER,                   -- column type of V; NULL when removed
  hlc     BLOB    NOT NULL,
  removed INTEGER NOT NULL CHECK (removed IN (0, 1)),
  PRIMARY KEY (row_id, key)
) STRICT, WITHOUT ROWID;
CREATE INDEX "Team__roles__by_key" ON "Team__roles" (key, row_id) WHERE removed = 0;
```

ARCHITECTURE 6.2 names the shape `(row_id, key, value, hlc, removed)` for maps. Removed keys stay
as tombstones with `removed = 1`, so a late write with an older stamp is merged correctly. Garbage
collection of old tombstones is out of scope for v0.3.

`WITHOUT ROWID` is used because side table rows are small and always looked up by their key.

### 3.3 How a `Write` reaches a side table

`Write::Insert` and `Write::Update` carry, per collection field, a `CollectionChange`
(`crates/ostrel_db/src/api/write.rs`), already merged by `ostrel_crdt`:

* `CollectionChange::Set`: a list of `SetChange`, applied in order. `Add { elem, tag }` makes `tag`
  the live tag of `(elem, tag.replica)` unless the stored tag of that replica has a `seq` of at
  least `tag.seq`; `Remove { elem, tag }` deletes the tag only if replica and `seq` both match,
  otherwise nothing happens. An insert carries no `Remove`.

  ```sql
  INSERT INTO "Room__members" (row_id, elem, tag_replica, tag_seq) VALUES (?, ?, ?, ?)
  ON CONFLICT (row_id, elem, tag_replica) DO UPDATE SET tag_seq = excluded.tag_seq
  WHERE excluded.tag_seq > tag_seq;
  DELETE FROM "Room__members" WHERE row_id = ? AND elem = ? AND tag_replica = ? AND tag_seq = ?;
  ```

* `CollectionChange::Map`: a list of `MapEntry { key, value, hlc }`, at most one per key; `value`
  `None` is a removed key. Each entry replaces the stored one:

  ```sql
  INSERT INTO "Team__roles" (row_id, key, value, hlc, removed) VALUES (?, ?, ?, ?, ?)
  ON CONFLICT (row_id, key) DO UPDATE SET value = excluded.value, hlc = excluded.hlc,
                                          removed = excluded.removed;
  ```

The driver does not compare map stamps; the merge already decided. The tag rules above are the
only decisions of the driver, and they make a replayed change harmless. Every write batch is
checked with `check_writes` first (field written twice, `Set` or `Map` value in a column, `Remove`
in an insert, map key twice), so all adapters refuse the same input with `DbError::Invalid`.

Queries return the state per collection field as `CollectionState`: live tags ordered by element
(`compare_key`, D61), then tag; map entries including tombstones ordered by key.

### 3.4 `Text merge text`

Two columns: `"body" TEXT NOT NULL` holds the plain text for `where` and `sort`, and
`"body__crdt" BLOB NOT NULL` holds the encoded sequence CRDT state (ARCHITECTURE 5.2). Both are
written in the same `Update`; the driver never derives one from the other. A conformance case
checks that they cannot be written separately.

### 3.5 `List[T]`

Whole value LWW in v0.3 (G6): one `TEXT` column with the canonical JSON encoding. Lists are not
queryable in `where` in v0.3 (they are outside the pushdown subset of ARCHITECTURE 9).

## 4. Adapter tables

```sql
-- Every id ever inserted, in any model. Enforces D20 across models.
CREATE TABLE ostrel_rows (
  id      BLOB    PRIMARY KEY CHECK (length(id) = 16),
  model   TEXT    NOT NULL,
  deleted INTEGER NOT NULL CHECK (deleted IN (0, 1))
) STRICT, WITHOUT ROWID;

-- Server op log (ARCHITECTURE 6.2: append_ops, ops_since).
CREATE TABLE ostrel_ops (
  seq     INTEGER PRIMARY KEY AUTOINCREMENT,   -- ServerSeq
  replica BLOB    NOT NULL CHECK (length(replica) = 8),
  op_seq  INTEGER NOT NULL,                    -- OpId.seq
  hlc     BLOB    NOT NULL CHECK (length(hlc) = 16),
  model   TEXT    NOT NULL,
  row_id  BLOB    NOT NULL CHECK (length(row_id) = 16),
  body    TEXT    NOT NULL,                    -- canonical JSON of the op
  UNIQUE (replica, op_seq)
) STRICT;

-- Server assigned numbers for serial fields (next_in_sequence).
CREATE TABLE ostrel_sequences (
  key  TEXT    PRIMARY KEY,
  last INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- Applied schema, one row.
CREATE TABLE ostrel_schema (
  id         INTEGER PRIMARY KEY CHECK (id = 1),
  hash       TEXT    NOT NULL,
  schema     TEXT    NOT NULL,                 -- canonical JSON of the model schema
  applied_at INTEGER NOT NULL
) STRICT;

-- Enum ordinals with their names, for people reading the file.
CREATE TABLE ostrel_enums (
  enum    TEXT    NOT NULL,
  ordinal INTEGER NOT NULL,
  name    TEXT    NOT NULL,
  PRIMARY KEY (enum, ordinal)
) STRICT, WITHOUT ROWID;
```

**Op log.** `AUTOINCREMENT` guarantees that a committed `seq` is never reused. Because SQLite has a
single writer and every writing transaction starts with `BEGIN IMMEDIATE`, commits happen in `seq`
order, so `ops_since(after, limit)` (`WHERE seq > ? ORDER BY seq LIMIT ?`) never skips an op that
commits later with a smaller number. This property does not hold for PostgreSQL sequences and
needs its own design there. `UNIQUE (replica, op_seq)` is a backstop for idempotent replay; the
server already acknowledges a repeated `OpId` without writing it (ARCHITECTURE 5.5, step 2).
`append_ops` returns the `seq` of the last op it inserted. `ServerSeq` is a `u64`; SQLite stores
it as a signed 64 bit integer, which leaves 2^63 minus 1 values. Filtering by read scope is done
by `ostrel_sync`, not by the driver.

**Sequences.** `next_in_sequence(key)` runs

```sql
INSERT INTO ostrel_sequences (key, last) VALUES (?, 1)
ON CONFLICT (key) DO UPDATE SET last = last + 1
RETURNING last;
```

The key is built by `ostrel_sync` from the schema and is passed as a parameter: `Issue.number` for
`serial`, and `Issue.number/<team id as hex>` for `serial per team`. The number is taken in the
same transaction as the update that stores it, so a rolled back write frees its number; numbers
are unique, gaps are allowed. SQLite has no native sequences, so `Capabilities.sequences` is
`false` and this table is the emulation (ASSUMPTION A3).

## 5. Migrations

`Connection::migrate` runs the whole plan in one transaction (SQLite DDL is transactional). It
first compares `MigrationPlan.from` with `ostrel_schema.hash` (`None` when the table has no row) and
refuses a plan made for a different starting point with `DbError::SchemaMismatch` (A4). On success it stores the new schema and hash and
refreshes `ostrel_enums` and the read views.

| Plan step | SQLite | Destructive flag needed |
|---|---|---|
| create model | `CREATE TABLE`, side tables, indexes, view | no |
| add optional field | `ALTER TABLE ADD COLUMN ... NULL` | no |
| add non optional field | `ALTER TABLE ADD COLUMN ... NOT NULL DEFAULT <constant>`; the plan must name a constant backfill value, otherwise it is refused | no |
| add `Set` or `Map` field | create side table and index | no |
| add or drop index | `CREATE INDEX` or `DROP INDEX` | no |
| append enum variant | none (new ordinal at the end) | no |
| insert or reorder enum variants | `UPDATE` with a `CASE` over old ordinals, per column using the enum | yes |
| drop field | drop its indexes, then `ALTER TABLE DROP COLUMN` or `DROP TABLE` for a side table | yes |
| drop model | `DROP TABLE` for table, side tables and view; ids stay in `ostrel_rows` with `deleted = 1` | yes |
| change type or merge strategy | refused in v0.3 (SYNTAX 11, item 4) | not available |

The constant backfill value is a literal from the plan bound as a parameter of a one time `UPDATE`
when SQLite does not accept it as a column default; it is never formatted into SQL text.

## 6. Queries

### 6.1 Shape

A `Query` (ARCHITECTURE 6.1) becomes one statement:

```sql
SELECT <columns> FROM "Model" AS t
WHERE (<see rule>) AND (<where>) AND (<cursor>)
ORDER BY <sort keys>, t."id" <dir>
LIMIT ?
```

Side table fields of the returned rows are read in a second statement per side table with
`WHERE row_id IN (...)` over the returned ids, at most `Capabilities.max_in_list` ids per
statement.

The `see` rule is part of `WHERE`, so `LIMIT` counts only rows the principal may read (AC-33 (e),
AC-56). There is no filtering after the query.

### 6.2 Translating the pushdown subset (ARCHITECTURE 9)

`t` is the queried row; `:x` stands for a bound parameter.

| Source | SQL |
|---|---|
| `a and b`, `a or b`, `not a` | `AND`, `OR`, `NOT` |
| `f == v`, `f != v` | `t."f" IS :v`, `t."f" IS NOT :v` (never `=`, see 6.3) |
| `f < v` and other ordered comparisons | `t."f" < :v` |
| `me` | the principal's user id as a parameter |
| `signed` | constant `1` or `0` as a parameter |
| `ref.f` (one hop) | `(SELECT r."f" FROM "Ref" AS r WHERE r."id" = t."ref")` |
| `a.b.f` (two hops) | the same subquery nested once more |
| `x in setField` | `EXISTS (SELECT 1 FROM "M__set" AS s WHERE s.row_id = <row> AND s.elem IS :x)` |
| `x in mapField` | the same on the map side table with `key` |
| `mapField[k]` | `(SELECT m.value FROM "M__map" AS m WHERE m.row_id = <row> AND m.key IS :k AND m.removed = 0)` |
| `e ?? c` | `COALESCE(e, :c)` |
| enum variant `member` | its ordinal as a parameter |

Here `<row>` is `t."id"` for a field of the queried model, or the reference column for a hop
(`t."room"` in `me in room.members`).

Examples from the chat and the issue tracker (SYNTAX 5.1, 8):

```sql
-- Message: see if me in room.members
EXISTS (SELECT 1 FROM "Room__members" AS s
        WHERE s.row_id = t."room" AND s.elem IS :me)

-- Issue: see if me in team.roles
EXISTS (SELECT 1 FROM "Team__roles" AS m
        WHERE m.row_id = t."team" AND m.key IS :me AND m.removed = 0)

-- Issue: (team.roles[me] ?? guest) >= member
COALESCE((SELECT m.value FROM "Team__roles" AS m
          WHERE m.row_id = t."team" AND m.key IS :me AND m.removed = 0), :guest) >= :member
```

### 6.3 `NULL` handling

SQL comparisons with `NULL` yield `NULL`, and `NOT NULL` is still `NULL`. Ostrel has two valued
logic, so the translation keeps `NULL` out of every boolean:

* Equality is always `IS` and `IS NOT`. `none == none` is true, `none == x` is false (SYNTAX 4.4),
  and a missing map key compared with `==` is false.
* Ordered comparisons with an optional operand are compile errors (AC-33 (a)); `??` turns an
  optional into a value first.
* The driver contract still defines them, because a missing map key (`mapField[k]` without `??`)
  is `NULL` too: an ordered comparison with `NULL` is false, and `not` of it is true
  (`crates/ostrel_db/src/api/query.rs`, filter semantics). Where an operand can be `NULL`, the
  translation wraps the comparison as `COALESCE(<cmp>, 0)` so that `NOT` sees a boolean.
* The remaining source of `NULL` is a hop through a non optional reference whose target row was
  deleted. The translation adds `EXISTS (SELECT 1 FROM "Ref" WHERE "id" = t."ref")` as a top level
  conjunct for every such hop, so the whole rule is false and the row is not readable. This fails
  closed: a rule like `a or room.x == 1` is denied when `room` is gone even if `a` is true. See
  ASSUMPTION A5 and question Q2.

### 6.4 Order

* Every `ORDER BY` ends with `t."id"` in the same direction as the sort field (ASSUMPTION A6). So
  `sort f desc` is exactly the reverse of `sort f`, and the order is total (SYNTAX 4.5).
* A query without `sort` orders by `t."id"` ascending, which is creation time order.
* `Text` and `Rank` use the `BINARY` collation, which on UTF-8 is Unicode code point order. Other
  replicas must sort the same way: PostgreSQL with `COLLATE "C"`, and the JS runtime by code point,
  not by the default UTF-16 code unit comparison of `<` (they differ for characters above U+FFFF).
  This is a cross team requirement, see question Q3.
* `NULL`, and a field that was never written, sorts before every value in ascending order and
  after every value in descending order, so descending stays the exact reverse (driver contract,
  `crates/ostrel_db/src/api/query.rs`, column order). SQLite does this by default. PostgreSQL
  defaults to the opposite (`NULLS LAST` ascending, `NULLS FIRST` descending), so its adapter
  writes `NULLS FIRST` for ascending and `NULLS LAST` for descending keys explicitly. `sort` on an
  optional field is a compile error in v0.3 (Q4, ARCHITECTURE 19); the rule still matters for
  `ostrel_sync` queries and fields added later by a migration.

### 6.5 `limit n` and `last n`

`limit n` is `ORDER BY <keys> LIMIT n`.

`last n` (G14, AC-56) is planned without anything new in the driver (ARCHITECTURE 6.1): the planner
reverses every sort direction including the id tie break, sets `limit n`, and the runtime reverses
the returned rows. For the chat:

```text
source:    Message where room == r sort made last 200
query IR:  model Message, filter room == r and see, order (made desc, id desc), limit 200
SQL:       ... WHERE <see> AND t."room" IS :r ORDER BY t."made" DESC, t."id" DESC LIMIT 200
runtime:   reverse the 200 rows, so they are in ascending order of (made, id)
```

Because `see` is in `WHERE`, these are the last 200 rows the principal may read, as AC-56 requires.

### 6.6 Paging with `after`

`Query.after` (`Cursor`) holds the sort values and id of the last row of the previous page. The
contract defines it as a position: the query returns only rows strictly after `(values, id)` in
query order, and the row itself need not exist any more. With every sort key non optional and one
direction (ASSUMPTION A6), the cursor is a row value comparison:

```sql
AND (t."made", t."id") > (:made, :id)     -- ascending
AND (t."made", t."id") < (:made, :id)     -- descending, also used for last n pages
```

With mixed directions or a `NULL` in the cursor the adapter expands the comparison key by key
(`a > :a OR (a IS :a AND (b < :b OR ...))`) under the `NULL` order of section 6.4.

### 6.7 Indexes

The planner knows every query of the program at compile time and creates:

* the primary key on `"id"`;
* one index per reference column (`"Message" ("room")`), used by hops and rule subqueries;
* one index per query shape: equality filter columns first, then the sort column, then `"id"`.
  The chat gets `"Message" ("room", "made", "id")`, the board gets
  `"Issue" ("team", "status", "rank", "id")`. Both `limit n` and `last n` use the same index,
  `last n` scans it backwards.

Index names are `"<Model>__by_<col>_<col>"`, again checked for collisions by the planner.

## 7. Connection and transactions

On `connect` the adapter sets:

| Pragma | Value | Why |
|---|---|---|
| `foreign_keys` | `ON` | side table cascade (section 3) |
| `journal_mode` | `WAL` | readers do not block the writer |
| `synchronous` | `FULL` | an `Ack` promises durability; `NORMAL` in WAL mode can lose the last commits on power loss |
| `busy_timeout` | `5000` | waits instead of failing when another connection holds the write lock |
| `trusted_schema` | `OFF` | a database file from elsewhere cannot run functions through views or triggers |

`Connection::begin` issues `BEGIN IMMEDIATE`, which takes the write lock at the start. A deferred
transaction that reads first and writes later can fail with `SQLITE_BUSY` at the first write, which
would surface as a random error in the middle of a batch. Read only `query` calls outside a
transaction use the WAL snapshot and do not take the lock.

`rusqlite` is blocking and the server runs on a `current_thread` runtime (ARCHITECTURE 4.1). The
adapter therefore owns its connection on one dedicated thread and serves requests through a
channel; the `BoxFuture` returned to the caller waits for the reply. This keeps the transaction on
the thread that began it.

`Capabilities` for SQLite: `transactions: true`, `sequences: false` (emulated, section 4),
`subqueries: true`, `json_fields: true`, `max_in_list: 1000` (the page size of `Held` and `Evict`,
well below SQLite's parameter limit).

## 8. Injection safety (AC-37)

* Identifiers come only from the compiled schema and are double quoted, with any `"` in a name
  doubled. Ostrel names cannot contain `"`, so the doubling is a second line of defence.
* Every value, including enum ordinals, `limit` values, sequence keys and backfill constants, is a
  bound parameter.
* The conformance suite round trips the metacharacter values of AC-37 through every column type
  of section 2.2 and every side table of section 3.

## 9. Conformance cases this mapping adds

These are proposals for `tests/db-conformance/` (owner T6-2); each is adapter independent.

1. Insert of an id that exists in another model fails; insert of a deleted id fails; neither changes
   any row (AC-31).
2. Update and delete with a stale `expect_version` fail and change nothing.
3. Update that only changes a side table entry still increases the version.
4. `last n` with a `see` filter returns the last n readable rows in ascending order, also when
   unreadable rows are interleaved (AC-56, AC-33 (e)).
5. Sort ties on the sort field are broken by id in the sort direction; `sort f desc limit n` equals
   the reverse of `sort f last n`.
6. `Text` sort order of strings containing characters above U+FFFF and in U+E000 to U+FFFF is code
   point order.
7. `x == none`, `x != none` and a missing map key behave as in SYNTAX 4.4 inside `not`.
8. A row whose non optional reference points to a deleted row is not readable under a rule that
   hops through it.
9. `next_in_sequence` per key starts at 1, is unique, and a rolled back number may be reused.
10. `ops_since` returns ops in `seq` order with no gaps among committed ops, and a repeated
    `(replica, op_seq)` is refused.
11. A `made` comparison at a millisecond boundary matches section 2.3 for `<`, `<=`, `>`, `>=`, `==`.

## 10. Assumptions

* **A1.** Resolved by T6-1b: `Write::Insert` and `Write::Update` carry merged changes per
  collection field (`CollectionChange`, section 3.3), not the whole collection. Review of the
  `Set` tag shape by T5 pending.
* **A2.** Resolved by T6-1b: existing or deleted id gives `DbError::Conflict`, stale version
  `DbError::VersionMismatch`, missing row `DbError::NotFound`.
* **A3.** `Capabilities.sequences = false` means the driver emulates sequences inside the
  transaction; `next_in_sequence` must still work.
* **A4.** Resolved by T6-1b: `MigrationPlan` carries `from` (hash of the schema it was planned
  from, `None` for a new database), `to` and the canonical JSON of the target schema. How the
  planner reads the stored schema is open (no contract method yet).
* **A5.** A hop through a non optional reference to a deleted row makes the whole rule false
  (fail closed, S-9), even where short circuit evaluation in the VM would not reach the hop.
* **A6.** The id tie break follows the direction of the sort field, and one query has one sort
  field (SYNTAX 7: `sort name [desc]`).
* **A7.** `Hlc` and `RowId` order is lexicographic over `(wall_ms, counter, replica)`, matching
  the byte layout in 2.3.

## 11. Open questions

* **Q1 (T5, architect).** Closed by D49: observed remove set with add tags, side table of
  section 3.1.
* **Q2 (architect).** Is a hop to a deleted row a runtime error in rules (then A5 matches S-9), or
  does it yield `none`? The VM evaluation of write rules and this SQL translation must agree.
* **Q3 (T3, T5).** The JS runtime must compare `Text` and `Rank` by code point (6.4). Owner of the
  shared sort vectors?
* **Q4 (architect).** `sort` on an optional field: allow with an expanded cursor predicate, or make
  it a compile error in v0.3? Proposal: compile error in v0.3, no example needs it.
* **Q5 (T5).** The `replicas` table of ARCHITECTURE 5.3 is not part of the DB interface. Does it
  live in the database through an extra trait method, or in a model of the std library?
