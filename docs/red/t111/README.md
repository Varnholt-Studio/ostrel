# Red review of docs/types.md against AC-29 (T111)

Task: T149. Object under review: `docs/types.md` at f202ff3 (unchanged on dev ebce16c), read
against SPEC AC-29 and B-F14, ARCHITECTURE v1.13 sections 5.1, 5.3, 5.9 and 7.4, SYNTAX 4.2
and 4.11, `runtime/js/api.d.ts` and the open v0.2 tasks. Focus as assigned: text length in
Unicode scalar values, inclusive ranges, the `Int` bound of plus or minus (2^53 minus 1), and
equality of server and client.

Verified and holding (no finding):

* `Int` literal bound in the checker: `9007199254740991` and `-9007199254740991` run,
  `9007199254740992`, `-9007199254740992` and a 23 digit literal give E0010 (debug build
  of `ostrel run` at ebce16c). `-9007199254740991 / -1` and `% -1` stay in range.
* Code point order of `Text`: `compareText` and `compareKey` in
  `runtime/js/crdt/canon/canon.mjs:44` and `:73` match `compare_key` in
  `crates/ostrel_core/src/f1b/value.rs:143`.
* Unpaired surrogates are `Invalid` in the JS encoder (`canon/cases.json` cases
  `utf16_lone_high`, `utf16_lone_low`), and `docs/types.md` section 3 lists honestly that the
  `len` and range vectors do not exist yet.

Result: 1 finding S2, 3 findings S3, 1 finding S4. Reproduction for F1 to F4:
`node docs/red/t111/probes.mjs` (exit 1, 11 of 13 probes show a broken claim).

## F1 (S2): the shared `len` and range vectors of AC-29 have no owner and no place

References: SPEC AC-29 (last sentence), B-F14, `docs/types.md:79` to `:82`, `:93`; tasks T120,
T121, T115, T116.

Steps: look for the task that writes the shared vectors for `len` in Unicode scalar values and
for inclusive range bounds, which AC-29 requires "identically on server and client, proven by
shared vectors incl. emoji and combining marks".

Expected: one task with a lock on one shared vector file (as `tests/crdt-vectors/canon/` for
the canonical encoding) that both the Rust and the JS side run.
Actual: `docs/types.md:81` assigns the rule to the units T2 and T3, not to a task, and names
no file. T120 (checker goldens) lists only the diagnostics of AC-29; T121 (JS backend) lists
`compareText` (D50) but neither `len` nor ranges; T115 and T116 mention neither. No task in
the backlog names `len`, scalar values or inclusive ranges.

Impact: this is the exact defect B-F14 was raised for (S2 there). The obvious JS
implementation is `String.prototype.length`; probe F1c shows 1000 emoji have `.length` 2000
and 1000 scalar values, so a guard and a server check can disagree, and a message shown
optimistically is later `rejected`. Without an owner, AC-29 cannot be met at v0.2.

Proposal (lead, with the architect): a task in T2 or T5 that adds
`tests/crdt-vectors/text-len/cases.json` (scalar values for ASCII, U+00E9, `e` U+0301,
U+1F600, a ZWJ sequence, U+FEFF, a lone surrogate as `Invalid`, the empty string) and
range cases (`1..2000` at 0, 1, 2000, 2001; a reversed range such as `5..1`), run by the VM
and by `runtime/js`; T120 and T121 depend on it.

## F2 (S3): the `Time` range is wider than the JS `Date` range

References: `docs/types.md:34`, `:96`, `:79` ("no per target exceptions");
`runtime/js/api.d.ts:68`; SYNTAX 4.2 (`Time.clock`, `date`).

Steps: `new Date(t).getTime()` for `t` in the documented `Time` range (probe F2).
Expected: every valid `Time` is a usable instant on both targets.
Actual: for any `t` with an absolute value above 8.64e15 (about year 275 760) JS gives
`NaN`, while `Time(SafeInt)` on the server accepts it up to 9007199254740991. `clock` and
`date` (std/core) then differ by target. `made` and `changed` are safe (48 bit `wall_ms`), but
a user written `Time` field is not.

Proposal: limit `Time` to plus or minus 8.64e15 ms (the ECMAScript time value range), as a
typed error at every boundary like the `Int` rule, and say so in 7.4 and `docs/types.md`.

## F3 (S3): the accepted wire forms of an `Int` are not defined

References: `docs/types.md:31` ("number, safe integer"), `:99` to `:101`; ARCHITECTURE 5.3
(canonical encoding defines output only), 7.4.

Steps: `JSON.parse` of `1.0`, `1e3`, `-0`, `9007199254740990.9`, `9007199254740991.4`, then
`Number.isSafeInteger` (probe F3).
Expected: one definition of which JSON number texts are an `Int` or `Time`, the same in the
Rust parser (T5 follow up with `serde_json`) and in JS.
Actual: JS turns all five into safe integers before any guard sees the text; the last two
are not integers at all and are silently rounded, against "never rounded" in
`docs/types.md:101`. `serde_json` reads `1.0` and `1e3` as `f64`, so the Rust side
will decide differently unless someone writes the rule down.

Proposal: the wire accepts an `Int` or `Time` only as a JSON integer without fraction or
exponent and without `-0`, checked on the raw text in Rust; JS receives only canonical server
output. Add the five texts as `Invalid` cases to the shared vectors of F1.

## F4 (S3): the 256 KiB `Text` field limit has no unit

References: ARCHITECTURE 5.9 (`Text` field size 256 KiB, `Invalid`), `docs/types.md:105` to
`:110` (does not mention the limit).

Steps: 262 144 copies of U+4E00 (probe F4).
Expected: one unit for the limit on client and server.
Actual: 262 144 UTF-16 units, 786 432 UTF-8 bytes. A client guard counting units accepts what
a server counting bytes rejects as `Invalid`, the B-F14 pattern again, now for the field limit.

Proposal: state the unit (UTF-8 bytes of the scalar values, before JSON escaping) in 5.9 and
in the text length rule of `docs/types.md`, and add a boundary case to the vectors of F1.

## F5 (S4): the canonical vectors cover the `Int` bounds only as output

References: `docs/types.md:85` ("the `Int` bounds"), `tests/crdt-vectors/canon/cases.json:7`,
`:8`.

The two cases encode the largest and smallest safe integer. No case shows that 2^53 or
minus 2^53 is refused, so the sentence promises more than the file proves. Proposal: either
narrow the sentence or add refusal cases with F1.

## Earlier review notes still open on dev

The two follow ups of the T111 review (ivo) are not applied at ebce16c: `docs/types.md:3`
names ARCHITECTURE 5.1 as v1.11 without a hash (current v1.13), and `:123` says server log
positions are stored as big endian bytes, while SQLite stores `ServerSeq` as `INTEGER`. They
are not counted again here.
