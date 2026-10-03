# Notes example: expected behaviour

This file states, by hand, what `notes.ostl` must do once the v0.2 compiler builds it.
It is written from SYNTAX 4.1 to 4.8 and SPEC AC-09, AC-10, AC-11, AC-30 and AC-31,
never from compiler output. Tests that use this example cite the case ids below
(`N-..`, `R-..`, `K-..`, `G-..`). A case changes only together with the source or the spec,
never to match an implementation.

## 1. Program facts

| Id | Fact | Source |
|---|---|---|
| G-01 | The program is an app (`app Notes`) without `auth`, so `me`, `signed` and `author` do not occur and no sign in view is shown | SYNTAX 4.1 |
| G-02 | The home view is `Main` (no `home` line) | SYNTAX 4.1 |
| G-03 | `Note` has the declared fields `title: Text`, `body: Text` (default empty text), `pinned: Bool` (default `false`) and the implicit fields `id`, `made`, `changed`, `pending`, `rejected`; it has no `author` field | SYNTAX 4.2 |
| G-04 | `deleteNote` touches no view, no client state and no `server` field, so it is placed as shared code; it is not a `server fn` and needs no `call` rule | SYNTAX 4.6 |
| G-05 | `ostrel build examples/v0_2/notes` exits 0 and produces a `server` target and a `client` target | AC-09 |
| G-06 | The source contains no handwritten HTTP, serialization, ORM or SQL code. A case insensitive search of `notes.ostl` finds none of: `http`, `fetch`, `request`, `response`, `route`, `socket`, `json`, `serialize`, `encode`, `decode`, `extern`, `sql`, `select `, `insert into`, `delete from`, `update `, `create table`, `where id`. The UI label "Delete" and the name `deleteNote` are not SQL and are allowed | AC-09 |

## 2. Behaviour through the generated client (AC-30)

Each case starts from an empty database unless it says otherwise. "Visible" means rendered
in the client's DOM. A note is shown as one `for` item; its children appear in this order:
the title (`look: strong`), the body text, the textbox "Text of TITLE" with the button
"Save", then either the button "Unpin" (pinned note) or the textbox "Rename TITLE" with the
button "Rename", the button "Pin" and the button "Delete" (unpinned note). TITLE is the
current title of that note. Every item root carries `data-pending` and `data-rejected`
(SYNTAX 4.7).

| Id | Steps | Expected |
|---|---|---|
| N-01 | Open the app | The textbox "New note" and the button "Add" are visible; no note is listed |
| N-02 | Type `Groceries` into "New note", press "Add" | One note with title `Groceries`, empty body, unpinned. The textbox "New note" is empty again |
| N-03 | Add `beta`, then `Alpha`, then `Gamma` | Order of titles from top: `Alpha`, `Gamma`, `beta` (sort by title in code point order, upper case letters before lower case, SPEC 12.5) |
| N-04 | Type `  Milk  ` into "New note", press "Add" | The note's title is `Milk` (the entry passes the trimmed text) |
| N-05 | Type only spaces into "New note", press "Add" | Nothing is created; no error notice (the entry does not fire on blank input) |
| N-06 | Note `Groceries` exists; type `eggs, milk` into "Text of Groceries", press "Save" | The body of `Groceries` shows `eggs, milk` |
| N-07 | Note `Groceries` exists; type `Shopping` into "Rename Groceries", press "Rename" | The note's title is `Shopping`; the textboxes are now named "Text of Shopping" and "Rename Shopping"; the list is re-sorted by the new title |
| N-08 | Note `Groceries` exists; press "Pin" | The note shows "Unpin" and no longer shows "Rename Groceries", "Pin" or "Delete"; "Text of Groceries" and "Save" stay |
| N-09 | Note `Groceries` is pinned; press "Unpin" | The note shows "Rename Groceries", "Pin" and "Delete" again |
| N-10 | Notes `Alpha` and `Beta` exist; press "Delete" on `Alpha` | Only `Beta` is listed |
| N-11 | Context A adds `Alpha` and `Beta`, edits the body of `Beta` to `x`, pins `Beta`, deletes `Alpha`. Context B (a second browser context) then loads or reloads the app | Context B lists exactly one note: title `Beta`, body `x`, pinned (shows "Unpin") |
| N-12 | Like N-11, then the server process is stopped and started again with the same `OSTREL_DB`; context B reloads | Same result as N-11 (AC-10) |

Text rendering: a title such as `<b>x</b>` or `{x}` typed into "New note" is shown as these
literal characters, never as markup (D23, AC-47).

## 3. Field checks

`check` runs on the client before the local commit and again on the server (SYNTAX 4.2).
Lengths are Unicode scalar values; both range ends are inclusive.

| Id | Input | Expected |
|---|---|---|
| K-01 | Title of 1 character | Accepted |
| K-02 | Title of 200 characters | Accepted |
| K-03 | "New note": the textbox accepts at most 200 characters (`maxlength` from the `check` of `title`, because the action makes a `Note`) | A 201st character cannot be typed |
| K-04 | "Rename TITLE" submitted with 201 characters | Rejected locally: title unchanged, the text stays in the textbox, the std error notice shows |
| K-05 | Body of 2000 characters via "Save" | Accepted |
| K-06 | Body of 2001 characters via "Save" | Rejected locally: body unchanged, the text stays, the error notice shows |
| K-07 | Via "Rename TITLE" or a crafted op: title of 200 emoji (each one scalar value, 4 bytes in UTF 8) | Accepted (length counts scalar values, not bytes) |
| K-08 | Via "Rename TITLE" or a crafted op: title made of the pair `e`, U+0301 (combining acute accent) 100 times, 200 scalar values | Accepted; with one more U+0301 (201 scalar values) rejected |

Known limit: the body cannot be cleared through the UI, because the entry never fires on blank
input. A crafted op that sets the body to empty text is accepted (`0..2000`).

## 4. Rule cases (server side, AC-11)

Rules are evaluated on the server for every op, also for ops that bypass the generated client.
Default deny (D8) does not apply to any verb here, because each verb has a rule. Expected
outcome kinds: `Denied` for a rule that is false, `Invalid` for a failed `check` or a malformed
op. Every denied or invalid case leaves the database unchanged.

| Id | Crafted op (state before) | Expected |
|---|---|---|
| R-01 | Read all notes (three notes stored) | All three are returned (`see if true`) |
| R-02 | Make `Note { title: "a" }` | Accepted; `body` is empty text, `pinned` is `false` |
| R-03 | Make `Note { title: "a", pinned: true }` | Accepted; the note is pinned |
| R-04 | Make `Note { title: "" }` | `Invalid` (check `title.len in 1..200`) |
| R-05 | Make with a title of 201 characters, or a body of 2001 characters | `Invalid` |
| R-06 | Make carrying an `id` that exists or is tombstoned, in `Note` or any other model | Rejected; the existing row is unchanged, a tombstoned row is not resurrected (AC-31) |
| R-07 | Make carrying an unknown field (for example `owner`) or a field of the wrong type (`pinned: "yes"`) | Rejected |
| R-08 | Edit `title` of an unpinned note | Accepted |
| R-09 | Edit `title` of a pinned note | `Denied` (`edit title if not pinned`; field rules narrow the record rule) |
| R-10 | Edit `body` of a pinned note | Accepted (record rule `edit if true`, no field rule for `body`) |
| R-11 | Edit `pinned` to `false` on a pinned note, then edit `title` | Both accepted, in this order |
| R-12 | Edit `id`, `made` or `changed` of any note | Rejected; the stored values stay |
| R-13 | Drop an unpinned note | Accepted; the note is no longer returned by reads |
| R-14 | Drop a pinned note | `Denied`; the note stays |
| R-15 | Drop or edit an id that does not exist | Rejected; nothing changes |
| R-16 | Call `deleteNote` as a server function call | Not available: it is not a `server fn`, so there is no server entry point for it; the client's `drop` reaches the server as a drop op and R-13, R-14 apply |

## 5. Assumptions

* ASSUMPTION A1: v0.2 has no PASETO identity (SPEC 3 puts it in v0.3), so the example has no
  `auth` and no rule mentions `me`. Rule cases that need a principal (wrong principal, forged
  owner field) need their own fixture.
* ASSUMPTION A2: the std elements `col`, `text`, `button`, `entry` and the attribute `look:` are
  available in v0.2 (SYNTAX 4.8 lists them for v0.3).
* ASSUMPTION A3: a delete from the UI goes through a function call, because an `on` action is an
  assignment, a `make` or a call (SYNTAX 4.8), not a `drop`.
* ASSUMPTION A4: when a single op changes `pinned` and `title` together, the `edit title` rule is
  evaluated against the stored `pinned`. The generated client never sends such an op; R-11 is
  the supported order.
