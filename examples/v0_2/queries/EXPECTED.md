# Query examples: expected behaviour

This file states, by hand, what `feed.ostl` and the files in `naming/` must do once the v0.2
compiler builds them. It is written from SYNTAX 4.4, 4.5 and 4.8 and SPEC AC-03, AC-33 (e) and
AC-56 (with the reading of SPEC 5.0), never from compiler output. Tests cite the case ids below.
A case changes only together with the source or the spec, never to match an implementation.

## 1. Fixture

Unless a case says otherwise, the database holds exactly these `Post` rows, written by crafted
make ops before the client opens the app:

| text | pos | hidden |
|---|---|---|
| `a` | 1 | `false` |
| `b` | 2 | `true` |
| `c` | 3 | `false` |
| `d` | 4 | `false` |
| `e` | 5 | `true` |
| `f` | 6 | `false` |

`see if not hidden` makes `a`, `c`, `d` and `f` readable. `b` and `e` are never sent to a client
and never shown. All `pos` values differ, so the `id` tie break plays no part in sections 2 and 3.

A list is read as the texts of its `for` items from top to bottom, under its heading
(`look: title`). "Must deny" marks the cases that a runtime which filters by `see` after the
limit fails (AC-33 (e)).

## 2. Lists after opening the app

| Id | List (query) | Expected | Wrong result it rules out |
|---|---|---|---|
| Q-01 | "Last three" (`Post sort pos last 3`) | `c`, `d`, `f` | `d`, `f` (filter after limit, must deny); `f`, `d`, `c` (descending order) |
| Q-02 | "Last two descending" (`Post sort pos desc last 2`) | `c`, `a` | `a`, `c` (wrong direction); `a` alone (filter after limit, must deny) |
| Q-03 | "First two" (`Post sort pos limit 2`) | `a`, `c` | `a` alone (filter after limit, must deny, AC-33 (e)); also the control for Q-02, which is this list reversed |
| Q-04 | "Last two up to five" (`Post where pos <= 5 sort pos last 2`) | `c`, `d` | `d` alone (filter after limit, must deny) |

Q-02 follows from SYNTAX 4.5: the readable rows in sort order are `f`, `d`, `c`, `a`, and the last
two rows in that order are `c`, `a`, still in sort order.

## 3. Live lists (SYNTAX 4.5: a query in a view is a live subscription limited by `see`)

Each case starts from the fixture with the app open in context A. The change is a crafted op
from a second principal, then context A is observed without a reload.

| Id | Change | Expected in "Last three" | Expected in "Last two up to five" |
|---|---|---|---|
| L-01 | Make `Post { text: "g", pos: 7 }` | `d`, `f`, `g` | unchanged: `c`, `d` |
| L-02 | Make `Post { text: "h", pos: 7, hidden: true }` | unchanged: `c`, `d`, `f` (must deny: `f` alone) | unchanged: `c`, `d` |
| L-03 | Edit `f`: `hidden` to `true` | `a`, `c`, `d` | unchanged: `c`, `d` |
| L-04 | Edit `e`: `hidden` to `false` | `d`, `e`, `f` | `d`, `e` |
| L-05 | Edit `d`: `hidden` to `true` | `a`, `c`, `f` | `a`, `c` |

In L-03 and L-05 the row that left the scope is evicted (SYNTAX 4.4) and the next readable row
moves into the window; the list never shows fewer rows than the window while enough readable
rows exist.

## 4. Name resolution in `where` and `sort` (`naming/`)

In `where` and `sort` a bare name is a field of the queried type; a local in scope with the same
name as such a field is a compile error with the hint "rename the local" (SYNTAX 4.5, B2-18).

Expected for every `s..` case: `ostrel check naming/FILE.ostl` exits 1 and prints exactly one
diagnostic (AC-03, one fault gives one diagnostic) in the format
`PATH:LINE:COLUMN: error[CODE]: MESSAGE`, whose `MESSAGE` contains `rename the local`. The code
is in the names range E0200 to E0299 (SPEC 12.6). The code number, the exact message and the
position are not pinned here: the code is assigned by the checker task together with its first
golden (`tests/errors/README.md`), and the position is open (section 5, O-3).

Expected for every `c..` case: `ostrel check naming/FILE.ostl` exits 0 and prints no error.

| Id | File | Local in scope | Queried type and field | Expected |
|---|---|---|---|---|
| S-01 | `s01_param_in_where.ostl` | parameter `pos` of `dropAt` | `Post.pos`, used in `where` | compile error, rename hint |
| S-02 | `s02_let_in_where.ostl` | `let pos` | `Post.pos`, used in `where` | compile error, rename hint |
| S-03 | `s03_loop_var_in_where.ostl` | outer loop variable `pos` (a `Slot`) | `Post.pos`, used in `where` | compile error, rename hint |
| S-04 | `s04_loop_var_in_sort.ostl` | outer loop variable `pos` (a `Slot`) | `Post.pos`, used in `sort` | compile error, rename hint |
| S-05 | `s05_implicit_field.ostl` | parameter `made` | implicit field `Post.made` (SYNTAX 4.2), used in `where` | compile error, rename hint |
| S-06 | `s06_unused_local.ostl` | parameter `pos`, not used in the query | `Post.pos`; the `where` names only `text` | compile error, rename hint (ASSUMPTION A1) |
| C-01 | `c01_local_of_other_type.ostl` | parameter `pos` | `Slot` has no field `pos`; `where at == pos` compares `Slot.at` with the parameter | accepted |
| C-02 | `c02_no_where_no_sort.ostl` | outer loop variable `pos` | `Post`, query without `where` and `sort` | accepted (the rule covers only `where` and `sort`) |

S-01 to S-04 are the shapes a reader would misread silently: `pos == pos` would otherwise be
always true and `drop` would remove every row. S-03 and S-04 show that a loop variable of an
enclosing `for` counts as a local in scope.

## 5. Assumptions and open points

* ASSUMPTION A1 (S-06): the rule applies to every local in scope whose name equals a field of
  the queried type, whether or not the `where` or `sort` mentions it. This is the literal text
  of AC-56 and SYNTAX 4.5. If the spec owner decides that only a name used in the query counts,
  S-06 becomes a control case (accepted) and nothing else changes.
* ASSUMPTION A2: the std elements `col` and `text` and the attribute `look:` are available in
  v0.2, as in the notes example (`examples/v0_2/notes/EXPECTED.md` A2).
* O-1 (not a case): the order of rows with equal sort values under `desc` together with `last n`.
  SYNTAX 4.5 says only that every sort ends with `id`; `docs/db-mapping.md` 6.5 reverses the id
  tie break with the direction. No case here depends on it, because all `pos` values differ.
* O-2 (not a case): `last 0`, a negative `n`, and `last n` without `sort`. None is decided.
* O-3 (not a case): the position of the S diagnostics. For S-01 to S-05 the natural position is
  the bare name in `where` or `sort`; S-06 has no such name in the query.
* O-4 (not a case): whether the loop variable of a `for` is in scope inside its own query, as in
  `for pos in Post where pos == 1`. The example `for room in Room where room == room` in SYNTAX 4.5
  suggests it is, but `Room` has no field `room` in any document, so the example decides nothing.
