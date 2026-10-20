# Built in types

Status: frozen with the type table of ARCHITECTURE 5.1 (v1.11). Owner: architect. Refs #111.
This document is derived from that table and adds nothing to it: if the two ever disagree, the
table in ARCHITECTURE 5.1 wins and this file is a bug. A change to a type goes through an
architect DECISION first and is then copied here.

It answers one question for every built in type: how a value of that type looks on the server,
on the wire and in the browser, so that the type checker (T2), the generated JS and its guards
(T3) and the database adapters (T6) agree without reading each other's code.

## 1. Where a value lives

A value takes one of four forms. Each type below lists all of them.

| Form | Where | Defined in |
|---|---|---|
| Wire | JSON in sync messages, server function calls and canonical encodings | ARCHITECTURE 5.1, 5.3 |
| Server | `ostrel_core::value::Value` in Rust, the persisted and wire value; the VM keeps its own reference counted values and converts at the `Host` boundary | ARCHITECTURE 5.3 |
| Database | the column or side table an adapter writes; SQLite is shown, other adapters follow their own mapping document | `docs/db-mapping.md` 2.2 and 3 |
| Client | the in memory JS value of the generated client and `runtime/js` | `runtime/js/api.d.ts` section 2 |

## 2. Type table

The column "AC-29" marks the types the v0.2 type checker must support at least (SPEC AC-29).
The other types are part of the frozen table and keep the same representation when they arrive.

| Type | AC-29 | Wire (JSON) | Server (`Value`) | Database (SQLite) | Client (JS) | Default merge |
|---|---|---|---|---|---|---|
| `Bool` | yes | `true` or `false` | `Bool(bool)` | `INTEGER`, 0 or 1 | `boolean` | LWW register |
| `Int` | yes | number, safe integer | `Int(SafeInt)` | `INTEGER` | `number` (`Int`) | LWW register |
| `Float` | no | number | `Float(FiniteFloat)` | `REAL` | `number` (`Float`) | LWW register |
| `Text` | yes | string | `Text(String)` | `TEXT`, UTF-8, not normalised | `string` (`Text`) | LWW register; `merge text` selects the sequence CRDT |
| `Time` | yes | number, ms since the Unix epoch | `Time(SafeInt)` | `INTEGER` | `number` (`Time`) | LWW register |
| `Bytes` | no | base64url string without padding | `Bytes(Vec<u8>)` | `BLOB` (as a `Set` element or `Map` key: `TEXT`, unpadded base64url, see 2.1) | `Uint8Array` | LWW register |
| enum | yes | string, the variant name | `Enum(String)` | `INTEGER`, ordinal in declaration order (as a `Set` element or `Map` key: `TEXT`, the variant name, see 2.1) | `string` (`EnumValue`) | LWW register |
| reference `T` | yes | id string, 32 lowercase hex digits | `Ref(RowId)` | `BLOB`, 16 bytes big endian | `string` (`Ref`, a `RowId`) | LWW register |
| `T?` | yes | value of `T`, or `null` | as `T`, or `Null` | as `T`, nullable, `NULL` for `none` | as `T`, or `null` | as `T` |
| `Set[T]` | yes | array | `Set(Vec<Value>)` | side table, one row per live add tag, so one element can have several rows (see 2.1) | readonly array | observed remove set, add wins |
| `Map[K, V]` | no | array of `[k, v]` pairs | `Map(Vec<(Value, Value)>)` | side table, one row per key including removed keys (see 2.1) | readonly array of `[k, v]` | per key LWW register with tombstone |
| `List[T]` | no | array | `List(Vec<Value>)` | `TEXT`, canonical JSON of the whole value | readonly array | LWW of the whole value |
| `Rank` | no | string, fractional index key | `Rank(String)` | `TEXT` | `string` (`Rank`) | LWW of the key |
| `Int serial [per f]` | no | number | `Int(SafeInt)`, `Null` until synced | `INTEGER`, nullable | `number`, `null` until synced | none, server assigned |

`Doc` and a counter type do not exist. Collaborative text is `Text merge text`.

### 2.1 Collections in the database

A `Set[T]` and a `Map[K, V]` field each get a side table (ARCHITECTURE 6.2, `docs/db-mapping.md` 3).

* `Set[T]`: observed remove set with add tags (D49). A row is one live add tag:
  `(row_id, elem, tag_replica, tag_seq)` with primary key `(row_id, elem, tag_replica)`. There is
  at most one live tag per element and replica, but replicas that added the same element
  concurrently each keep their own row, so an element can have several rows. The element is in
  the set while it has at least one row. There are no tombstones.
* `Map[K, V]`: one row per key, `(row_id, key, value, hlc, removed)` with primary key
  `(row_id, key)`. A removed key stays as a tombstone with `removed = 1`.
* Element and key form (D79): the `elem` and `key` columns hold the wire value, so that
  `ORDER BY` equals `compare_key` (D61). An enum element or key is stored as its variant name and
  a `Bytes` element or key as unpadded base64url, both as `TEXT` with code point collation. A
  reference stays 16 bytes big endian; all other types use their column form from the table above.
* A `Map` value is stored like a model column, so an enum value stays an ordinal.

### 2.2 Implicit fields

Every row has `id` (a reference to the row itself), `made` and `changed`, `author` (a reference to
`User`, only in apps with `auth`), and on the client only `pending` and `rejected`.

`made` and `changed` are server checked HLC stamps (ARCHITECTURE 5.1, 5.5). On the wire and in
`RowMeta` of the client (`runtime/js/api.d.ts` section 4) they are `Hlc` hex strings of 32
digits; in the database they are 16 byte big endian blobs. A program reads them as `Time`
(SYNTAX 4.6 and 8, `docs/db-mapping.md` 2.3): the value is the stamp's `wall_ms`, which is the
first 12 hex digits of the wire form. A comparison with a `Time` in a query is translated to a
comparison with the smallest stamp of that millisecond (`docs/db-mapping.md` 2.3). `sort made`
and `sort changed` order by the full stamp and then by `id`, not by the `Time` value alone.

## 3. Rules that hold on every target

These rules are the reason the table needs no per target exceptions. Each rule must be checked
by shared vectors that the Rust and the JS implementation both run. For text length (including
emoji and combining marks) and inclusive range bounds this is required by SPEC AC-29 and owned
by T2 (type checker, VM) and T3 (generated JS, guards). On dev at c4d3d8d vectors exist only for
part of the rules:

* `tests/crdt-vectors/canon/cases.json`: the `Int` bounds, floats including `-0`, escapes and
  unpaired surrogates in the canonical encoding.
* `tests/crdt-vectors/set/`, `map/` and `rank/`: element, key and `Rank` order (D61).
* Text order by code point in the database: only `text_orders_by_code_point` in
  `crates/ostrel_db_memory/tests/driver.rs` and `ac_37_text_order_with_nul` in
  `tests/db-conformance/cases/`. The shared file `tests/db-conformance/text-order/` named in
  ARCHITECTURE 6.2 (owner T6) does not exist yet.

Still missing: `IntOverflow` in arithmetic, `len` in scalar values, inclusive ranges, row order of
enum and `Bool`, and optional comparisons.

**Integers.** `Int` and `Time` are limited to plus and minus (2^53 minus 1), that is
`-9007199254740991` to `9007199254740991`, both bounds included. Rust stores them as `i64`, JS as
`number`. `Int` arithmetic is checked: leaving the range raises `IntOverflow` on both targets. An `Int`
literal outside the range is a compile error. A value outside the range that arrives from the
database, the wire or a bridge is a typed error and is never rounded (ARCHITECTURE 7.4, G9).

**Floats.** A `Float` is always finite. NaN and the infinities are rejected at every boundary,
and `-0` is encoded as `0` (ARCHITECTURE 5.3).

**Text length.** `len` counts Unicode scalar values on every target, never UTF-8 bytes and never
UTF-16 units. An emoji such as U+1F600 has length 1; `e` followed by the combining acute accent
U+0301 has length 2, because nothing is normalised. A JS string with an unpaired surrogate is not
a `Text` and is `Invalid` at every boundary. Independently of `len`, the VM refuses to build a
single `Text` above 16 MiB of UTF-8 (`TextLimit`, SPEC 12.4).

**Ranges.** `a..b` includes both ends on every target: `check text.len in 1..2000` accepts lengths
1 and 2000 and refuses 0 and 2001.

**Order.** `Text` and `Rank` compare by Unicode code point, never by UTF-16 unit or locale. An enum
compares by declaration order. `Bool` puts `false` before `true`. Elements of a `Set` and keys of a
`Map` are ordered by wire value (booleans, then numbers, then strings), which is the order of
`compareKey` in JS and `compare_key` in Rust (ARCHITECTURE 6.2, D61).

**Optionals.** `none` is `null` on the wire and in JS, `NULL` in the database. Ordering
comparisons with an optional operand are compile errors; `==` and `!=` are allowed and `none`
equals only `none` (SYNTAX 4.4).

**Identifiers.** Ids, HLC stamps and server log positions are not `Int`. They travel as
lowercase hex strings of fixed width (`RowId` 32 digits) and are stored as big endian bytes, so
string order, byte order and value order agree (ARCHITECTURE 5.3).

## 4. Sources

* ARCHITECTURE 5.1 (type table), 5.3 (ids, `Value`, canonical encoding), 6.2 (order), 7.4 (integers)
* SYNTAX 4.2 (`len`, `..`), 4.3 (merge per field), 4.4 (optionals in rules)
* SPEC AC-29, 12.4 (`TextLimit`)
* ARCHITECTURE 6.2 (D49 `Set` tags, D61 element order, D79 element and key form)
* `runtime/js/api.d.ts` sections 2 and 4 (client values, `RowMeta`)
* `docs/db-mapping.md` 2.2, 2.3 and 3 (SQLite)
