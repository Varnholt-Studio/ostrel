# Notes example (v0.2)

A small notes app written in the Ostrel programming language: create, list, update and
delete notes. Source: `notes.ostl`. Expected behaviour, checks and rule cases:
`EXPECTED.md`.

It is the shared target of the v0.2 work: the parser and checker accept it, `ostrel build`
turns it into a server and a browser client (AC-09), the browser test drives it in headless
Chromium (AC-30), and its data survives a server restart on SQLite (AC-10).

## Scope

Included:

* create a note with a title; the list is sorted by title
* edit the body, rename an unpinned note, pin and unpin
* delete an unpinned note; a pinned note cannot be renamed or deleted (server rule)
* title length 1 to 200, body length 0 to 2000 (Unicode scalar values)

Not included: sign in and per user notes (identity arrives in v0.3), search, multi line
bodies, clearing a body from the UI.

## Rules for this directory

* The source contains no HTTP, serialization, ORM or SQL code (AC-09).
* `EXPECTED.md` is written by hand from the spec and reviewed by a second person. It is never
  copied from compiler output (ARCHITECTURE 15.4).

## Status

The v0.1 compiler does not accept this file (`data`, `view` and queries are not in the v0.1
subset). A build check is added once the v0.2 parser and checker land.
