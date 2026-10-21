# Security: operating limits of the server

Status: draft, started with work package T197 (Refs #197). Inputs: ARCHITECTURE 5.4, 8 and 16
(risk 10), decisions D102, D153 and D160, SPEC 12.14. Further sections follow with their work
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
