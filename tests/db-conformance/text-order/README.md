# Text order, length and range vectors

Shared vectors for the `Text` rules that must hold on every target (SPEC AC-29, B-F14;
ARCHITECTURE 6.2, D50; SYNTAX 4.2; `docs/types.md` section 3):

* `Text` compares by Unicode code point, never by UTF-16 unit, locale or normalised form. On UTF-8
  this equals byte order, which is what SQLite `BINARY` and PostgreSQL `COLLATE "C"` use.
* `len` counts Unicode scalar values, never UTF-8 bytes, UTF-16 units or graphemes. Nothing is
  normalised.
* A range `a..b` includes both ends.

The database adapters run `order` and `compare` through the conformance harness (T70), the VM (T2)
and the JS runtime (T3) run all four sections. `vectors.test.mjs` checks the format and every
expectation against independent oracles (code point order, UTF-8 byte order, scalar count), and
checks that the file keeps the cases that make a UTF-16, byte, grapheme, exclusive bound or
normalising implementation fail.

## Format (version 1)

`vectors.json` is one JSON object: `format` (1), `ac` (`"AC-29"`), `doc`, and the sections below.
The file is ASCII; every other character is written as a JSON escape, characters above U+FFFF as
a surrogate pair escape. Every entry has a unique `name` (`[a-z0-9_]+`) and an optional `doc`.

Text values use the format of `tests/db-conformance/cases/README.md`: `{"text": "..."}` or
`{"text_repeat": ["<unit>", count]}` for the unit repeated `count` times. No value contains an
unpaired surrogate.

* `order`: `{name, sorted}`, a list of at least two texts in strictly ascending order. Inserting
  them in any order and sorting ascending must give exactly this list; descending gives its
  reverse.
* `compare`: `{name, a, b, expect}`, `expect` is `-1`, `0` or `1` for `a` less than, equal to or
  greater than `b`. Swapping `a` and `b` negates it.
* `len`: `{name, text, expect}`, `expect` is the number of Unicode scalar values.
* `range`: `{name, text, lo, hi, expect}` checks `text.len in lo..hi` with `lo` and `hi` as JSON
  numbers; `{name, int, lo, hi, expect}` checks `int in lo..hi` with all three as decimal strings
  within plus or minus (2^53 minus 1). `expect` is the boolean result. `lo` is never greater than
  `hi`, because SYNTAX 4.2 does not define empty ranges.

## Not covered yet

`Rank` order (it shares the `Text` rule, its own vectors are in `tests/crdt-vectors/rank/`),
equality filters on `Text` (D55, need `Query.filter`), and the order of `Set` elements and `Map`
keys (`tests/crdt-vectors/set/` and `map/`).
