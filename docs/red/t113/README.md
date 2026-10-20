# Red review of the set CRDT against the membership hijack (T146)

Subject: T113, commit `038e377` (`crates/ostrel_crdt/`, `tests/crdt-vectors/set/`,
`tests/crdt-vectors/format.mjs`), with the JS counterpart
`runtime/js/crdt/set/add_wins_set.mjs` at the same commit.
Measured against: SPEC AC-55, ARCHITECTURE G12 and 6.2, D96, D106.
Reviewer of this report: kira.

## Result

No S1 or S2 finding. The Rust add wins set binds every remove to the entry of the element it
names (`crates/ostrel_crdt/src/set.rs:80-96`), so a remove can never delete a tag of another
element, whatever tags it carries and however many ops it is split into. The JS set behaves
the same (`add_wins_set.mjs:172-181`). Two lower findings concern the shared vectors, not
the merge itself.

## Attack cases run

All cases are in `red_t146_hijack.rs` (Rust) and `js_hijack_probe.mjs` (JS) in this
directory. Every case passes at `038e377`, which is the expected, safe outcome.

| Case | Attack | Outcome |
|---|---|---|
| H1 | one op: remove `me` naming the tags of every member | only `me` goes |
| H2 | several ops: remove `me` once per victim, naming only that victim's tags | no change at all |
| H3 | 200 victim tags sent as 64 tag batches against `me` | all 200 victim tags stay |
| H4 | make tags (D106): four start elements share the make OpId, remove one naming it | the other three stay with their tag |
| H5 | stale, future and `u32::MAX` seq for a live tag | no change |
| H6 | seq 0 and `u32::MAX` adds, replica `u64::MAX` | replays ignored, no overflow |
| H7 | remove naming a foreign tag arrives before that tag's add | add survives |
| H8 | empty text element, duplicate tag in one remove | only the named tag goes |
| H9 | `Text` with the hex of a reference used as element | same element as the reference (see N1) |

Reproduce: copy `red_t146_hijack.rs` to `crates/ostrel_crdt/tests/` on a checkout of
`038e377` and run `cargo test -p ostrel_crdt --test red_t146_hijack` (9 passed). JS:
`node docs/red/t113/js_hijack_probe.mjs <checkout of 038e377>`.

## Findings

### F1 (S3): the make tags of D106 have no shared vector, and the format cannot express one

D106 gives every start element of a `Set` field the OpId of the `make` as its add tag, so
one tag belongs to several elements, and names T113 as owner of a vector for it. At
`038e377` there is none, and the vector format cannot hold one: an op adds exactly one
element and `format.mjs:72` refuses a second op with the same id ("is used twice").

Steps: `node docs/red/t113/validator_probe.mjs <checkout of 038e377>`.
Expected: a vector with two adds tagged by one make OpId can be validated and run on both
sides. Actual: `REJECTED: ops[1].id 000000000000000700000001 is used twice`.

Impact: the shared tag is exactly the case D96 exists for (a remove of one start member
naming the make tag must not drop the other start members). Rust and JS both get it right
today (H4), but nothing shared pins it, so the SQLite side table (T114) and later adapters
are not checked against it. Proposal: a set op `{"make": [e, ...]}` in the vector format
with one id, plus a vector where a remove of one start element names the make tag and
the others stay. This changes `format.mjs`, the README, both runners and the JS loader;
proposed as a follow up task for T5, since D106 came after the T113 approval.

### F2 (S4): the validator accepts a remove tag that names a remove op

`format.mjs:134` skips any tag whose op is not an add. Since `038e377` this also hides a
tag that names the id of a remove op, which is an authoring error in a vector (dora's
review note (a) on T113, still open).

Steps: second probe in `validator_probe.mjs`. Expected: refused. Actual: accepted.

### N1 (note, no finding): string class elements compare by wire string

`compare_key` treats `Text`, `Enum`, `Rank`, `Ref` and `Bytes` as one class compared by
their wire string (`ostrel_core/src/f1b/value.rs:166-208`, D61), so `Text` holding the hex
of a user reference is the same element as that reference (H9). This is intended and
unreachable in a typed `Set[Ref User]`, because the server validates each op against the
schema before it reaches a CRDT or a driver (ARCHITECTURE 5.5, 6.2 F6). The hijack defence
of AC-55 for elements therefore rests on that validation and on the per op rule check
(T116, T124); the CRDT layer cannot and does not enforce G12. Both belong in the AC-55
acceptance tests of T124, not in T113.

## Not covered

The server side rule check of G12 (T116), the SQLite side table DELETE (T114, reviewed in
T147) and the conformance case of D105 (T70) are outside this review.
