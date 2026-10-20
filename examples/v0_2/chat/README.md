# Chat membership example (v0.2)

The room membership part of the chat, written in the Ostrel programming language: create a
room, join it and leave it. Source: `membership.ostl`. Expected behaviour, checks and rule
cases: `EXPECTED.md`.

It is the shared target for AC-55 (set operations are checked per element): the parser and
checker accept it, the server enforces the membership rule for every set operation, and the
client shows "Join" or "Leave" per room.

## Scope

Included:

* create a room with a name of 1 to 40 characters; the creator is its only member
* join and leave a room as oneself
* all rooms and their member lists are visible to every signed in user

Not included: messages (see `examples/chat/`), adding or removing other users, renaming or
deleting rooms (denied by default), private rooms.

## Rules for this directory

* The source contains no HTTP, serialization, ORM or SQL code (AC-09).
* `EXPECTED.md` is written by hand from the spec and reviewed by a second person. It is never
  copied from compiler output (ARCHITECTURE 15.4).

## Status

The v0.1 compiler does not accept this file (`app`, `data`, `view` and sets are not in the
v0.1 subset). A build check is added once the v0.2 parser and checker land.
