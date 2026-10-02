# Chat acceptance contract (workload W1)

This file is the black box contract between the chat acceptance suite
`bench/apps/chat/acceptance` and every chat implementation it runs against:

* `examples/chat/` and `examples/chat-nostd/` (Ostrel programming language, SPEC AC-65),
* REF-A under `bench/reference/ts-express-prisma-yjs/` (SPEC AC-28),
* later reference stacks of MEASUREMENT 4.1.

Sources, in order of precedence: SPEC AC-55, AC-57, AC-61, AC-62, AC-63, AC-65 and AC-28;
MEASUREMENT 1.6 and 4.2; SYNTAX 5 (chat scope and source); ARCHITECTURE 5.5, 5.9, 6.2 and the
note on `len` under 3.2. Where this file and one of them disagree, the source wins and this
file is a bug.

Rules for the suite and for every implementation:

1. The suite addresses the UI only through accessible roles with exact accessible names
   (Playwright `getByRole(role, { name, exact: true })`) and through exact visible text
   (`getByText(text, { exact: true })`), as listed in section 3. No `data-testid`, no CSS
   selectors, no element ids, no DOM structure (MEASUREMENT 4.2, G16).
2. Anything the suite needs from the UI and that is not listed in section 3 is a bug in this
   file, not a reason to add a selector.
3. Crafted requests that bypass the client go through one probe driver per stack (section 6).
   The driver is the only place that knows wire formats, ids and database tables.
4. Release builds, real PASETO sign in with `auth paseto open`, one server, PostgreSQL
   (MEASUREMENT 1.6).
5. Every case below is binary: it passes on a stack or it does not. A stack that cannot meet
   a case fails it; the case is not changed for that stack.

## 1. Scope

The chat scope is the one disclosed with every count (SYNTAX 5, B2-13) and is identical for
all stacks:

* sign in with an Ed25519 user key and a unique handle, open registration;
* rooms: create, join, leave; all rooms and their member lists are visible to every signed in
  user;
* live messages in one total order per room; the newest 200 messages per room are shown;
* offline send with later sync, offline reload of the app, persistence across reload;
* a visible sync state.

Out of scope, and therefore never tested as a feature: typing indicators, unread counts,
editing or deleting messages, private rooms, moderation, device links, key export and import,
invite registration. Editing and deleting messages are tested only as forbidden (probe P1).

## 2. Users, handles and display names

| Item | Contract |
|---|---|
| Handle | 3 to 20 characters of `[a-z0-9_]`, unique after case folding (AC-63 a). The suite generates handles of the form `u<run><n>` in lower case. |
| Display name | Set only through the probe driver as an honest request of the user (`setOwnName`, section 6). Never addressed in the UI. |
| Shown identity | Every place that shows the author of a message shows the author's handle as exact visible text. A display name may be shown in addition, never instead (AC-63 g). |

## 3. Roles and visible text

### 3.1 Signed out view

| Purpose | Role | Accessible name or text |
|---|---|---|
| Handle for a new user | textbox | `Handle` |
| Register a new user with that handle | button | `Register` |
| Sign in with the key already in the browser | button | `Sign in` |

A browser without a stored user key shows `Handle` and `Register`. A browser whose key store
holds a user key shows `Sign in` (AC-63 b). A rejected handle (duplicate after case folding,
or not matching the pattern) leaves the user signed out and the view still shows `Register`.

### 3.2 Signed in view

| Purpose | Role | Accessible name or text |
|---|---|---|
| Sign out (keeps the user key, wipes the local data store) | button | `Sign out` |
| One entry per room, all rooms visible to the user | button | the room name, exactly |
| Name of a new room | textbox | `New room` |
| Create that room | button | `Create` |
| Join the current room (shown only when not a member) | button | `Join` |
| Leave the current room (shown only when a member) | button | `Leave` |
| Message text (shown only when a member of the current room) | textbox | `Message` |
| Send that text | button | `Send` |
| Sync state | visible text | exactly one of `live`, `syncing`, `offline` |
| Each message of the current room | visible text | the author's handle and the message text, each as its own exact text |

Behaviour behind these names:

* Clicking a room button makes that room the current room. Creating a room makes the creator
  its only member and makes it the current room (SYNTAX 5.1 `make if members == {me}`).
* Exactly one of `Join` and `Leave` is visible while a room is current.
* `Send` with valid text creates one message and clears the `Message` textbox. `Send` with
  text that fails a limit of section 4 creates no message on any client or on the server and
  leaves the text unchanged in the textbox (AC-65, B2-10). The same holds for `Create` and
  the `New room` textbox.
* Messages of the current room are shown oldest at the top. The suite reads the order by the
  vertical position of each message text on screen (bounding box top), not by document order,
  because an implementation may render the list reversed with CSS.
* A message that is waiting in the outbox is visible with its text (AC-62). How a pending or
  rejected message is marked is not part of this contract.

## 4. Limits

Lengths are counted in Unicode scalar values on every stack, client and server
(ARCHITECTURE 3.2 note on `len`, SPEC AC-29, B-F14). Ranges are inclusive.

| Field | Accepted | Source |
|---|---|---|
| Message text | 1 to 2000 scalar values | SYNTAX 5.1 `check text.len in 1..2000` |
| Room name | 1 to 40 scalar values | SYNTAX 5.1 `check name.len in 1..40` |
| Handle | 3 to 20 of `[a-z0-9_]` | AC-63 a |

Boundary vectors the suite sends through the UI (`n x c` means the character `c` repeated `n`
times; no vector has leading or trailing white space):

| Id | Field | Value | Expected |
|---|---|---|---|
| L1 | message | `2000 x a` | accepted |
| L2 | message | `2001 x a` | rejected, text stays in `Message` |
| L3 | message | `2000 x U+1F600` (4000 UTF-16 units, 8000 UTF-8 bytes) | accepted |
| L4 | message | `2001 x U+1F600` | rejected, text stays in `Message` |
| L5 | message | `1000 x (e, U+0301)` (2000 scalar values, 1000 visible characters) | accepted |
| L6 | message | `1000 x (e, U+0301)` followed by `a` | rejected, text stays in `Message` |
| L7 | message | empty textbox | no message is created |
| L8 | room name | `40 x U+00E9` | accepted |
| L9 | room name | `41 x U+00E9` | rejected, text stays in `New room` |
| L10 | room name | `40 x U+1F600` | accepted |

Not tested, because the sources do not fix it: text consisting only of white space, and
leading or trailing white space (SYNTAX 5.1 sends the raw text, SYNTAX 5.2 trims it). The
suite never sends such text.

Rate limits and quotas follow ARCHITECTURE 5.9 and AC-61. The suite runs one flood case
(AC-65): one user sends more new messages than the burst allows; the excess is rejected and
never shown on a second client, and a second user keeps sending and receiving during the
flood.

## 5. Ids and order

Ids are mechanism. No id is visible in the UI or read by the suite through the UI. Room,
message and user ids are handled only by the probe driver (section 6).

The suite checks these observable order properties. Every message text in an order case
carries a unique nonce so that it is found by exact text.

| Id | Case | Expected |
|---|---|---|
| O1 | Two or more clients in one room, all synced | The vertical order of the room's messages is identical on every client and stays identical after a reload. |
| O2 | One client sends several messages in a row | They are shown in the order they were sent, on every client. |
| O3 | Two clients with clocks fixed to the same millisecond each send one message | Both messages are shown, in the same order on every client (MEASUREMENT 1.6). |
| O4 | A client with its clock 1 hour ahead sends a message; afterwards a client with a correct clock sends one, at least 3 s later | Either the skewed message is rejected (never shown on another client, absent from the database), or it is clamped and shown before the later honest message on every client (AC-57 d, MEASUREMENT 1.6). |
| O5 | A client sends a message, then sets its clock 1 hour back and sends another | Either the second message is rejected as in O4, or it is shown after the sender's first message on every client (AC-57 e, B2-9). |
| O6 | A client sends two messages offline, then reconnects | Both arrive exactly once, in the order of O2 (AC-62, AC-59). |

Clocks are set per browser context with the Playwright clock API. The suite never moves the
server clock.

## 6. Probe driver

Each stack provides a driver with the operations below. The suite calls them with handles,
room names and message texts; the driver maps these to the stack's ids and wire format. Every
probe P1 to P7 is sent as a crafted request that bypasses the generated client. Expected for
every probe: the request is rejected (or has no effect), and the database snapshot taken by
`snapshot` before and after the probe is identical. Single exception, from AC-55: in P2 sent
as several operations, the attacker's own join may be accepted when the attacker was not a
member; every removal has no effect.

| Operation | Meaning |
|---|---|
| `snapshot()` | Canonical, ordered dump of users, rooms, memberships and messages, without volatile columns. |
| `setOwnName(user, name)` | Honest change of the user's own display name. |
| `members(room)` | Handles of the room's members, read through the stack's normal read path. |

| Probe | Crafted request | Source |
|---|---|---|
| P1 | Edit, then delete a message of another user | MEASUREMENT 1.6, SYNTAX 5 |
| P2 | Room hijack: add `me` and remove every other member, once as one operation and once as several operations | AC-55, MEASUREMENT 1.6 |
| P3 | Create a message whose id equals an existing message id | D20, MEASUREMENT 1.6 |
| P4 | Write with the replica id (REF-A: Yjs client id) of another user | D22, AC-57 b, MEASUREMENT 1.6 |
| P5 | Create a message with `author` set to another user | AC-63 i |
| P6 | Change the own handle, and change another user's display name | AC-63 g |
| P7 | Write a message into a room the user is not a member of | SYNTAX 5.1 `make if me in room.members` |

UI cases that complete the probes:

| Id | Case | Expected |
|---|---|---|
| U1 | User A writes a message offline, signs out, user B signs in in the same browser, the network returns | No message with that text exists on the server or on any client (MEASUREMENT 1.6, RED-A 7). |
| U2 | A member is removed from a room by leaving on another device while the first device is offline; the first device reconnects | The room's messages disappear from that device's view and local store (AC-44, AC-58). |
| U3 | Message texts from the XSS corpus are sent | They are shown as the exact text, no script runs and no new element appears (MEASUREMENT 1.6, D23). |
| U4 | Sign out, then `Sign in` in the same browser | Same handle and the same room memberships as before (AC-63 b). |
| U5 | Two users with the same display name write in one room | Their messages show two different handles (AC-63 g). |

MEASUREMENT 1.6 also lists "call of a `server fn` with foreign data" and "`make` with a
deleted row id". The chat declares no server function and deletes no rows (SYNTAX 5), so
neither applies to W1; they are covered by the issue tracker workload and the hostile corpus.

## 7. Current state of the implementations

Checked against REF-A server `721db52` and REF-A client core `ddd04a7`. The Ostrel chat
examples do not exist yet; their column states what SYNTAX 5 implies.

| Item | Contract | Ostrel chat (SYNTAX 5) | REF-A today | Action |
|---|---|---|---|---|
| Message length unit | scalar values | `len` counts scalar values | UTF-16 units (`server/src/messages.ts:35`, `client/src/message.ts:36`) | REF-A server and client count scalar values; L3 and L4 fail today |
| Message length range | 1 to 2000 | `1..2000` | 1 to 2000 UTF-16 units after a non white space check | covered by the line above |
| Room name | 1 to 40 scalar values | `1..40` | 1 to 64 UTF-16 units, trimmed (`server/src/app.ts:110`) | REF-A uses 1 to 40 scalar values |
| Message id | not visible | runtime assigned | lower case UUID, client chosen | none |
| Order | O1 to O6 | `sort made`, server stamped and re-stamped (ARCHITECTURE 5.5) | `(ts, id)` by code point, out of window rejected (server README) | none for the contract; O4 and O5 take either branch |
| Newest 200 shown | yes | `last 200` | UI not built yet | REF-A UI shows at least the newest 200 |
| Sync state text | `live`, `syncing`, `offline` | `text sync.state` | UI not built yet | REF-A UI renders these words |
| Invalid text stays in input | yes | client `check` before commit | UI not built yet | REF-A UI validates before sending |
| Sign in names | section 3.1 | std sign in view | UI not built yet | std view and REF-A UI use these names |

## 8. Assumptions and open points

* ASSUMPTION: the std sign in view of `auth paseto open` uses the names of section 3.1, and
  `text sync.state` renders the enum variant names `live`, `syncing` and `offline`. Both are
  asked in `#spec`; until answered the suite uses them.
* ASSUMPTION: the std `entry` keeps its text when the `check` of the created record fails, as
  AC-65 requires for the suite; SYNTAX 5.2 shows the same with an explicit draft variable.
* Open: white space only message text (section 4) is not decided by the sources.
