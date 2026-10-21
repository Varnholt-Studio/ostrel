# Sync: operating limits

Status: draft, started with work package T197 (Refs #197). Inputs: ARCHITECTURE 5.4, 5.6, 6.1
and 16 (risk 10), decisions D153 and D160, SPEC 12.14. Further sections on the sync protocol
follow with their work packages.

## 1. One server process per app database

In v0.3 exactly one server process serves an app database, on one host (D153, D160). Several
processes on one database, or one database shared by servers on several hosts, are not
supported.

### 1.1 Why

Three parts of the sync engine live in the memory of the server process and are only correct
when that process is the only writer:

* **Revocation of a device key** closes the open sessions of that key, but only the sessions of
  the process that handled the revocation. A second process would keep its sessions of the
  revoked key open until their token expires (at most 24 hours).
* **Broadcast order.** Each process numbers its commits and sends them to its clients in one
  order. Ops written by a second process would reach clients outside that order.
* **Quiet point of snapshots.** A snapshot is read at a point where no commit of this process
  is in flight. Commits of a second process are not seen by that check, so a snapshot could
  mix states.

The row versions of the database adapter (`expect_version`) protect single rows against lost
updates, but none of the three points above.

### 1.2 Start lock

The server takes a start lock before it opens the app database and keeps it until it exits. A
second server process on the same database does not start; it stops with a message such as:

    another server process already serves this app database: the start lock
    /srv/app/data/app.db.lock is held by process 4711; an app database is served by exactly one
    server process, stop the other process first (docs/sync.md)

The lock is an exclusive advisory file lock (`flock` on Linux). The operating system releases it
when the process ends in any way, also after a crash or `kill -9`, so a restarted server never
finds a stale lock. The lock file contains the process id of the holder and is never removed.

| Database | Lock file |
| --- | --- |
| `sqlite:<path>` | `<path>.lock` next to the database file. Symbolic links of the file and of its directory are followed first, so every spelling of one file uses one lock. |
| `postgres://...` | `ostrel-pg-<sha256 of the database name>.lock` in the lock directory of the server. The key is the database name alone, the same name the adapter puts into `application_name`. |
| `memory:` | none, the database lives inside the process. |

Restarts by a supervisor (systemd, a container runtime) work as usual: the old process has
ended, so its lock is free. After a failed commit the server ends with exit code 75 and the
supervisor starts a new process (ARCHITECTURE 5.4); the new process takes the lock before it
reads the log end.

### 1.3 Operator rules

* Run one server process per app database. To restart, stop the old process first; the new one
  refuses to start while the old one still runs.
* Do not delete a lock file while a server runs. A process started after the deletion would
  create a new lock file and start beside the running one.
* Keep the SQLite database and its lock file on a local file system. Network file systems may
  not implement `flock` faithfully.
* Do not open one SQLite database through a hard link under another name; the second name gets
  its own lock file.
* On PostgreSQL, give every server process of an app the same lock directory on a local file
  system. Two apps on one host whose databases have the same name on different PostgreSQL
  servers refuse each other; give them different lock directories.

### 1.4 Limit: one host

The start lock sees only processes on the same host that share the file system of the lock file.
A PostgreSQL database served by processes on several hosts, or in several containers without a
common file system, is out of scope for v0.3 and is not detected (D160).

Such a setup does not only break revocation, broadcast order and snapshots (1.1). At start, the
PostgreSQL adapter ends every other backend of the app (ARCHITECTURE 6.1). A process on a second
host therefore ends the backends of the first; the first process fails its next commit, ends with
exit code 75 and, restarted by its supervisor, ends the backends of the second. Both hosts keep
restarting each other, and both write in between.

Serving one app database from several processes or hosts needs a later decision that provides a
revocation notice to every process, one global broadcast order, a global quiet point and a start
lock that works across hosts (ARCHITECTURE 16, risk 10).
