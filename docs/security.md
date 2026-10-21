# Security: operating limits of the server

Status: draft, started with work package T197 (Refs #197), section 2 added with T207 (Refs
#207). Inputs: ARCHITECTURE 5.4, 5.7, 8 and 16 (risks 10 and 13), decisions D102, D117, D118,
D134, D153, D157, D159, D160, D161 and D162, SPEC 12.14. Further sections follow with their work
packages.

## 1. One server process per app database

The security guarantees of the server hold only when exactly one server process, on one host,
serves an app database (D153, D160). The mechanism and the operator rules are in
[`docs/sync.md`](sync.md), section 1.

### 1.1 What breaks with a second process

* **Revocation.** When a device key is revoked, the server closes every open session of that key
  at once (D102). It can only close the sessions it holds itself. Sessions of the revoked key in
  a second process stay open and keep reading and writing until their token expires (at most 24
  hours).
* **Broadcast order and snapshots.** Ops of a second process reach clients outside the broadcast
  order of the first, and snapshots can mix states (`docs/sync.md` 1.1). Rules are still
  checked per request in each process, but a revocation in one process does not reach the
  sessions of the other.

### 1.2 Enforcement

The server takes an exclusive start lock before it opens the app database; a second process on
the same database refuses to start and names the process that holds the lock. The lock is
released by the operating system when the holder ends, so a crash leaves no stale lock.

Limits of the start lock, all documented for v0.3:

* It sees only processes on the same host that share the file system of the lock file. Several
  hosts, or containers without a common file system, on one PostgreSQL database are out of scope
  and not detected (D160). In such a setup the processes also end each other's database
  backends at start and keep restarting each other (`docs/sync.md` 1.4).
* A lock file on a network file system, a lock file deleted while the server runs and a hard
  link to the SQLite file under another name all defeat it.
* The lock is advisory. A process that does not take it (another tool writing to the database)
  is not stopped; only the server takes it.

Operators must not run a second server process on an app database, also not for a short time
during a deployment: stop the old process before starting the new one.

## 2. Audit file of elevated server functions

Every execution of an elevated server function (`as system`) leaves one audit entry: function,
caller, SHA-256 of the canonical encoding of the arguments (never the arguments themselves) and
time (ARCHITECTURE 5.7, D117, D118). The server writes the entry after the `call` rule admitted
the caller and before the body runs, outside the transaction of the body, so an entry without
effect is possible and an effect without entry is not. If the entry cannot be written and
synced, the body does not run and the client gets `Unavailable`; a retry is safe (D134).

### 2.1 Where the file lives

* **SQLite:** `--audit-log`, default `audit.log` next to the database file.
* **PostgreSQL:** no default. A server whose program has an elevated function refuses to start
  without `--audit-log` on a persistent file system.
* **memory:** the audit stays in memory (tests only).

One server process writes the file, because one process serves the app database (section 1,
D153).

### 2.2 What the server guarantees

* Append only: the server never truncates or rewrites the file. Each entry is one line.
* Each entry is synced to disk before the body of its call runs. When the server creates the file
  it also syncs the directory.
* At start, if the file does not end with a line end (an entry torn by a crash), the server
  appends one and syncs before it accepts a call. The torn entry stays a broken line; the next
  entry is readable.
* Before each append the server compares the open file with the path (device, inode, link
  count) and opens the path again if they differ. A failure to open it is an audit failure: the
  call gets `Unavailable` and its body does not run.
* If an append or a sync reports an error, the record may still have been stored. A reader of the
  file must expect an entry for a call that answered `Unavailable`.

The sync of each entry happens while the elevated call holds the write lock of the process
(D161), so every elevated call adds one sync to the hold time of every other writer
(ARCHITECTURE 16, risk 13 (c)). A group sync that covers several entries with one sync is not
part of v0.3; it may come later by a DECISION, and keeps the order of ARCHITECTURE 5.7 (no body
before its entry is on disk). Details in [`docs/sync.md`](sync.md), section 2.

### 2.3 Operator rules

* Rotate only by renaming the file. The server notices the rename at the next append and
  continues in a new file under the configured path.
* Copy and truncate (for example `copytruncate` of logrotate) is not supported: entries written
  between the copy and the truncation are lost.
* Keep a rotated file until the server has written into the new file. An entry whose append
  started just before the rename still goes into the rotated file.
* Do not delete the live audit file, and do not delete a rotated file before the rule above
  allows it (2.4).
* Keep the audit file on a local, persistent file system; it inherits the sync guarantees of
  that file system.

### 2.4 Limit: unlink between check and append

The comparison of open file and path (2.2) runs before the append, not atomically with it. If the
path is unlinked after the comparison and before the append, the entry is written into the
unlinked file (ARCHITECTURE 16, risk 13 (d)). The server reports success, the body runs, and the
entry disappears with the last open handle of that file: the server opens the path again at the
next append, because the link count of the open file is then zero.

This needs an operator action on the live file (deleting it, or deleting a just rotated file
during an append). The server cannot close the gap with the operating system interfaces it uses;
it is a documented limit of v0.3. Following the rules in 2.3 avoids it. A rename instead of a
delete keeps the entry, because the renamed file still has a name.
