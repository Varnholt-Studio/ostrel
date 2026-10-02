# CRDT vectors

Shared golden vectors for the CRDTs of ARCHITECTURE 5.1 and 6.2. The Rust implementation
(`crates/ostrel_crdt`) and the JavaScript runtime (`runtime/js/crdt/`) run the same files and
must produce the same canonical output for every vector (AC-41).

Expectations are written by hand and reviewed by a second person. They are never copied from
the output of an implementation.

## Layout

| Path | Content |
|---|---|
| `lww/` | Last writer wins register, the default merge of every scalar field |
| `set/` | `Set[T]`: add wins observed remove set with add tags (D49) |
| `map/` | `Map[K, V]`: last writer wins per key, with tombstones (G6) |
| `canon/cases.json` | Canonical JSON encoding and the hex form of ids (ARCHITECTURE 5.3) |
| `format.mjs` | Loader and validator for the op vectors |
| `oracle.mjs` | Minimal reference models for `lww` and `set` until their runtime modules land |
| `*.test.mjs` | JavaScript runners, part of the gate (`ci/checks/50_tests.sh`) |

Planned, not yet present: `rank/` (fractional index) and `text/` (collaborative plain text, D11).

## Op vectors

One case per file, named `NN_topic.json`. Every field is required and no other field is allowed.

```json
{
  "strategy": "map",
  "description": "What the case shows, in one sentence.",
  "ops": [
    { "id": "<OpId>", "hlc": "<Hlc>", "op": { "put": ["ana", "admin"] } }
  ],
  "deliveries": [[0], [0, 0]],
  "expect": { "value": [["ana", "admin"]], "state": [["ana", { "hlc": "<Hlc>", "value": "admin" }]] }
}
```

* `ops`: the ops of all replicas. `id` is the `OpId` (24 lowercase hex digits: replica, then
  seq) and `hlc` the `Hlc` (32 lowercase hex digits: wall time, counter, replica), both as on the
  wire. The replica in `id` and `hlc` is the same; ids and hlcs are unique; the seqs of each
  replica are listed as 1, 2, 3 and so on.
* `deliveries`: each entry is one replica's delivery order, as indexes into `ops`. Every op
  appears at least once; an index that appears again is a replay of that op.
* `expect`: what every replica holds after its delivery, compared by canonical encoding.
  `value` is what a program reads, `state` is the full replica state including tombstones and
  tags. Both must match for every delivery.

A runner creates a fresh replica per delivery, applies the ops in that order and compares
`encode(state())` and `encode(value())` with the canonical encoding of `expect`.

### Op bodies, value and state per strategy

| Strategy | Op body | `value` | `state` |
|---|---|---|---|
| `lww` | `{"set": v}` | `v` of the highest `Hlc` | `{"hlc": h, "value": v}` |
| `set` | `{"add": e}` or `{"remove": e, "tags": [OpId, ...]}` | live elements | `[[e, [tag, ...]], ...]` for live elements, tags sorted |
| `map` | `{"put": [k, v]}` or `{"remove": k}` | `[[k, v], ...]` for live keys | `[[k, {"hlc": h, "value": v}], ...]` or `{"hlc": h, "removed": true}` for a tombstone |

Elements and keys are ordered by their canonical encoding, compared by Unicode code point
(which equals UTF-8 byte order, D50). For text this puts U+FF01 before U+1F600, unlike the
default JavaScript sort.

### Rules for `set` vectors

The server log delivers a remove only after the adds it names, and there are no tag tombstones
(ARCHITECTURE 6.2). The validator therefore refuses a delivery in which a named add comes after
the remove, including a replay of that add. A remove names between 1 and 64 tags (ARCHITECTURE
5.9); a tag the replica never received is ignored.

## Canonical encoding cases

`canon/cases.json` holds a list of `{name, kind, input, canonical}` (or `error` instead of
`canonical`):

| `kind` | `input` | Notes |
|---|---|---|
| `value` | a JSON value | A number written without fraction or exponent is an `Int`, otherwise a `Float` |
| `utf16` | UTF-16 code units as numbers | Covers unpaired surrogates, which JSON text cannot carry safely; `"error": "Invalid"` |
| `replica_id`, `server_seq` | decimal string | Fixed width hex of 16 digits |
| `op_id` | `{replica, seq}` as decimal strings | 24 digits |
| `hlc`, `row_id` | `{wall_ms, counter, replica}` as decimal strings | 32 digits |

Decimal strings keep 64 bit values exact in every JSON parser.

## Adding a model

A model is a module with `createReplica()`, which returns an object with `apply(op)`, `value()`
and `state()`. Add it to `MODELS` in `vectors.test.mjs`. When the runtime module of a strategy
lands, it runs next to the oracle, so both are checked against the same expectations.

## Open points

* The op body shapes above follow the wire forms in ARCHITECTURE 5.1 and 6.2. The protocol stub
  (T25, `ostrel_sync::protocol`) is the authority; if it differs, the vectors follow it.
* Applying a `set` add again after a remove that named its tag would bring the element back.
  Vectors exclude that order (see above); deduplicating replayed ops belongs to the sync layer.
