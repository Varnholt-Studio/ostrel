# Chat membership example: expected behaviour

This file states, by hand, what `membership.ostl` must do once the v0.2 compiler builds it.
It is written from SYNTAX 4.1 to 4.4 and 4.8, SPEC AC-33 (c) and AC-55, ARCHITECTURE G12 and
5.6 and decisions D96 and D106, never from compiler output. Tests that use this example cite
the case ids below (`G-..`, `N-..`, `K-..`, `M-..`, `S-..`, `R-..`, `W-..`). A case changes
only together with the source or the spec, never to match an implementation.

## 1. Program facts

| Id | Fact | Source |
|---|---|---|
| G-01 | The program is an app with `auth paseto open`, so `me`, `signed`, the std `User` type and the implicit `author` field exist | SYNTAX 4.1 |
| G-02 | The home view is `Main` (no `home` line); it is shown only to a signed principal | SYNTAX 4.1 |
| G-03 | `Room` has the declared fields `name: Text` and `members: Set[User]` (default `{me}`) and the implicit fields `id`, `made`, `changed`, `author`, `pending`, `rejected` | SYNTAX 4.2 |
| G-04 | `members` merges as an add wins observed remove set | SYNTAX 4.3 |
| G-05 | `Room` has rules for `see`, `make` and the field `members` only. It has no record `edit` rule, no `edit name` rule and no `drop` rule | SYNTAX 4.4 |
| G-06 | The `edit members` rule mentions both directions (`added` and `removed`), so `ostrel check` emits no warning for it (compare W-01) | SYNTAX 4.4, AC-55 |
| G-07 | The source contains no handwritten HTTP, serialization, ORM or SQL code. A case insensitive search of `membership.ostl` finds none of: `http`, `fetch`, `request`, `response`, `route`, `socket`, `json`, `serialize`, `encode`, `decode`, `extern`, `sql`, `select `, `insert into`, `delete from`, `update `, `create table`, `where id` | AC-09 |

## 2. Fixture

Three users exist as std `User` rows with the handles `alice`, `bob` and `carol`. "As alice"
means the principal of the client or of the crafted op is alice (A1). Unless a case says
otherwise, one room exists: `general`, made by alice, so `members == {alice}` and the add tag
of alice is the `OpId` of that `make` (D106). "Add tag of X" is the tag of the live add of
element X in `members`. Every crafted `make` carries all declared fields, as the generated
client sends them (ARCHITECTURE 5.4, D106); a crafted `make` that leaves out a field with a
default is undecided (QUESTION #1471) and no case here expects anything of it.

## 3. Behaviour through the generated client

Each room is one `for` item with the room name (`look: strong`) and either the button "Leave"
(the principal is a member) or the button "Join" (not a member). Rooms are sorted by name in
code point order (SPEC 12.5).

| Id | Steps | Expected |
|---|---|---|
| N-01 | As bob, open the app (fixture) | The textbox "New room", the button "Create" and the room `general` with the button "Join" are visible |
| N-02 | As bob, type `random` into "New room", press "Create" | A room `random` is listed after `general`, with the button "Leave"; bob is its only member |
| N-03 | As bob, press "Join" on `general` | `general` shows "Leave"; `members == {alice, bob}` on the server |
| N-04 | After N-03, as bob, press "Leave" on `general` | `general` shows "Join"; `members == {alice}` on the server |
| N-05 | As alice, open the app | `general` shows "Leave" (alice is a member from the `make`) |

## 4. Field checks

| Id | Input | Expected |
|---|---|---|
| K-01 | ASCII room name of 1 character, and of 40 characters | Accepted |
| K-02 | "New room", ASCII input only: the textbox accepts at most 40 characters (`maxlength` from the `check` of `name`) | A 41st ASCII character cannot be typed |
| K-03 | As bob: crafted make `Room { name: n, members: {bob} }` with an ASCII name `n` of 0 or 41 characters | `Invalid`; nothing is stored |

K-01 to K-03 use ASCII input only, where one character is one Unicode scalar value and one
UTF-16 code unit. For other input the unit of `maxlength` is undecided (A5).

## 5. Membership cases (AC-55)

Each case is a crafted op sent to the server, also bypassing the generated client. Outcome
kinds: `Accepted`, `Denied` (the rule is false), `Invalid` (malformed op). Every `Denied` or
`Invalid` op leaves the database unchanged. "Members after" is the stored set after the op or
ops of the case.

| Id | State before | Op(s) | Expected per op | Members after |
|---|---|---|---|---|
| M-01 join | `{alice}` | as bob: add `bob` | `Accepted` (`added == {me}`, `removed` empty) | `{alice, bob}` |
| M-02 leave | `{alice, bob}` | as bob: remove `bob`, naming the add tag of bob | `Accepted` (`removed == {me}`, `added` empty) | `{alice}` |
| M-03 add other | `{alice}` | as alice: add `carol` | `Denied` (`added == {carol}`, not `{me}`; `removed` is empty, so the second clause is false) | `{alice}` |
| M-04 hijack, several ops | `{alice, bob}` | as carol, in this order: add `carol`; remove `alice` naming the add tag of alice; remove `bob` naming the add tag of bob | add `carol`: `Accepted` (a join); each removal: `Denied` | `{alice, bob, carol}` |
| M-05 hijack, one op | `{alice, bob}` | as carol: one op that adds `carol` and removes `alice` and `bob` | `Invalid` (more than one element and both directions); carol is not added either | `{alice, bob}` |
| M-06 hijack, removals only | `{alice, bob}` | as carol (not a member): remove `alice` naming the add tag of alice; remove `bob` naming the add tag of bob | each `Denied` | `{alice, bob}` |
| M-07 remove other as member | `{alice, bob}` | as bob: remove `alice` naming the add tag of alice | `Denied` (`removed == {alice}`) | `{alice, bob}` |
| M-08 foreign tag | `{alice, bob}` | as bob: remove `bob`, naming only the add tag of alice | `Accepted` by the rule (`removed == {me}`), but the tag does not belong to element `bob` and is ignored like an unknown tag; nothing is removed | `{alice, bob}` |
| M-09 join twice | `{alice, bob}`, bob added from replica B1 | as bob from B1: add `bob` | `Accepted`; the set keeps one element `bob`, whose only live tag from B1 is now the `OpId` of this op (one live tag per element and replica, ARCHITECTURE 6.2) | `{alice, bob}` |
| M-10 no principal | `{alice}` | without a signed principal: add any user | `Denied` (ARCHITECTURE 5.5 step 1: without a verified session every op is `Denied`) | `{alice}` |
| M-11 leave from one of two replicas | `{alice, bob}`, bob added once from replica B1 (tag t1) and once from replica B2 (tag t2), two devices of bob | as bob from B1: remove `bob` naming only t1 | `Accepted` by the rule; t2 stays live, so bob stays a member (add wins, ARCHITECTURE 6.2). A remove naming t1 and t2 leaves `{alice}` | `{alice, bob}` |

M-04 and M-05 together are the hijack case of AC-55: whether sent as one or as several ops,
no other member is removed. M-08 is the element binding of remove tags (D96): a remove of `me`
can never take away another member.

## 6. Op shape cases (AC-55)

| Id | Op (state before `{alice, bob}`) | Expected |
|---|---|---|
| S-01 | as carol: one op that adds `carol` and `bob` | `Invalid` (more than one element), even though one element is `me` |
| S-02 | as bob: one op that removes `bob` and `alice` | `Invalid` (more than one element) |
| S-03 | as bob: one op that adds `bob` and removes `bob` | `Invalid` (both directions) |
| S-04 | as bob: one op on `members` with no element | `Invalid` |

## 7. Other rule cases

| Id | Crafted op | Expected |
|---|---|---|
| R-01 | as bob, through the generated client: create the room `x` (N-02) | The client sends a `make` with all fields, `members: {bob}` from the default `{me}`; `Accepted`. A crafted `make` without `members` is not a case here (QUESTION #1471) |
| R-02 | as bob: make `Room { name: "x", members: {bob} }` | `Accepted` |
| R-03 | as bob: make `Room { name: "x", members: {alice, bob} }` | `Denied` (`make if members == {me}`) |
| R-04 | as bob: make `Room { name: "x", members: {} }` | `Denied` |
| R-05 | make `Room { name: "x", members: {bob} }` without a signed principal | `Denied` (ARCHITECTURE 5.5 step 1) |
| R-06 | as alice: edit `name` of `general` | `Denied` (no record `edit` rule and no `edit name` rule, default deny D8) |
| R-07 | as alice: drop `general` | `Denied` (no `drop` rule, D8) |
| R-08 | as bob (not a member): read all rooms | `general` is returned with its member list (`see if signed`) |
| R-09 | without a signed principal: read all rooms | Nothing is returned |

## 8. Compiler warning (AC-55)

| Id | Source change | Expected |
|---|---|---|
| W-01 | Replace line 10 of `membership.ostl` with `  edit members if added == {me}` | `ostrel check` exits 0 and emits one warning at that rule: the direction `removed` is not mentioned and is denied. At run time M-01 is `Accepted` and M-02 is `Denied` |
| W-02 | Replace line 10 with `  edit members if removed == {me}` | One warning for `added`. M-01 is `Denied`, M-02 is `Accepted` |

The warning text and code are not fixed here; the spec names neither (A4).

## 9. Assumptions

* ASSUMPTION A1: v0.2 has no PASETO sign in (SPEC 3 puts it in v0.3). The app declares
  `auth paseto open` because `me`, `signed` and `User` are compile errors without `auth`
  (SYNTAX 4.1). In v0.2 tests the principal comes from the development identity of a debug
  build (SPEC 2, AC-12), and the harness seeds the users of section 2.
* ASSUMPTION A2: the std elements `col`, `row`, `text`, `button`, `entry` and the attribute
  `look:` are available in v0.2 (SYNTAX 4.8 lists them for v0.3), as in
  `examples/v0_2/notes/` (T122, A2).
* ASSUMPTION A3: an op that names several elements or both directions exists only as a crafted
  op; the generated client sends one element per op (G12).
* ASSUMPTION A4: AC-55 requires a warning for an unmentioned direction but fixes neither its
  code nor its text; W-01 and W-02 check only that exactly one warning is emitted at the rule.
* ASSUMPTION A5: whether `maxlength` counts UTF-16 code units or scalar values is open
  (QUESTION esra #1247). K-01 to K-03 use ASCII input only, as `examples/v0_2/notes/` does.
