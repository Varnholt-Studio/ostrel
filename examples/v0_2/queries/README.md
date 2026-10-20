# Query examples for `last n` (v0.2)

Hand written examples for SPEC AC-56: `Type sort f last n` returns the last n rows in sort
order among the rows the principal may read, and in `where` and `sort` a local name in scope
that equals a field name of the queried type is a compile error with a rename hint
(SYNTAX 4.5, G14, B2-14, B2-18). They also cover AC-33 (e) for `last n` (SPEC 5.0).

Files:

* `feed.ostl`: a small app with four live lists over one model. Its `see` rule hides some rows,
  so a wrong order of filtering and limiting gives a visibly different list.
* `naming/`: one small app per name resolution case. `s..` files must fail to compile, `c..`
  files are controls that must compile.
* `EXPECTED.md`: the expected behaviour and diagnostics, with case ids (`Q-..`, `L-..`, `S-..`,
  `C-..`) that tests cite.

## Rules for this directory

* `EXPECTED.md` is written by hand from the spec and reviewed by a second person. It is never
  copied from compiler output (ARCHITECTURE 15.4).
* Each file in `naming/` contains exactly one fault (`s..`) or none (`c..`).
* Expectations pin only what the spec decides. Open points are listed in `EXPECTED.md` 5 and
  are not part of any case.

## Status

The v0.1 compiler does not accept these files (`app`, `data`, `view` and queries are not in the
v0.1 subset). Tests are added once the v0.2 parser and checker land.
