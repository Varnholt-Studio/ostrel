# Benchmarks

Status: draft. Owner: QA (ARCHITECTURE 5.9). Sections 2.1 and 2.2 were written with work
package T207 (Refs #207) as a template for QA. No benchmark values are published yet. This file collects the results required by SPEC (AC-27, AC-28, AC-51, AC-61, AC-65) and
the limits that a reader must know to compare them. Only results of official runs as defined in
`MEASUREMENT.md` appear here; values from a development machine never do.

## 1. Rules

* Every value names the commit, the hardware, the operating system and the load of the measuring
  machine (SPEC S-13).
* Latency reports state engine time and frame wait separately (SPEC S-13).
* Losses against a reference stack are published as openly as wins.
* Every protection that our server enforces and that changes a number is listed next to the
  result, together with whether the reference stack enforces it too (ARCHITECTURE 5.9).

## 2. Disclosed limits

This section is incomplete. It lists the limits documented so far; QA completes it with every
limit of ARCHITECTURE 5.9 before results are published.

### 2.1 Audit sync of elevated server functions under the write lock

Every call of an elevated server function (`as system`) appends one entry to the audit file and
syncs it to disk before its body runs, while the call holds the write lock of the server process
(ARCHITECTURE 5.7, ARCHITECTURE 16 risk 13 (c), D161). As a consequence:

* each elevated call adds one sync of the audit file to the hold time of the write lock, and every
  other writer of the process waits for it;
* elevated calls are serial, so their rate is bounded by the sync latency of the disk that holds
  the audit file;
* calls of functions that are not elevated, and op batches, pay no audit sync.

A result of an app whose measured path calls elevated functions therefore depends on the disk of
the audit file.

There is no value yet: the file store of the audit (ARCHITECTURE 5.7) is not on the development
branch, which has only the in memory store used by tests (`crates/ostrel_server/src/audit.rs`,
see [`docs/sync.md`](docs/sync.md), section 2.5), so no audit sync can be measured. A result
whose measured path calls elevated functions states these fields:

* the number of elevated calls in the measured path;
* the database adapter (SQLite or PostgreSQL);
* the disk of the audit file (type, file system, write cache);
* the hold time of the write lock, p50 and p99, with elevated calls;
* the hold time of the write lock, p50 and p99, without elevated calls.

Values measured on the memory adapter do not count here: it keeps the audit in memory and hides
the sync. A p99 hold time above 20 ms in the KPI A setup is a blocker (ARCHITECTURE 5.4,
ARCHITECTURE 16 risk 12); this bound is on the hold time of the write lock, while KPI A itself is
a p99 of at most 50 ms for engine plus render (ARCHITECTURE 1, D2).
A group sync that covers several entries with one sync is not part of v0.3 and may come later by
a DECISION. Details: [`docs/sync.md`](docs/sync.md), section 2.

### 2.2 Audit file and operator actions

Unlinking the audit file between the server's path check and its append writes the entry into the
unlinked file, where it is lost (ARCHITECTURE 5.7, ARCHITECTURE 16 risk 13 (d)). This changes no
benchmark value; it is listed so that the audit guarantee is not read as stronger than it is.
Details: [`docs/security.md`](docs/security.md), section 2.4.

### 2.3 Query scans without an index under the write lock

Both database drivers scan every row of the model and filter in `ostrel_db::eval`; a scanned row
costs 10 VM steps through `scan_limit` (ARCHITECTURE 5.9, 6.1, ARCHITECTURE 16 risk 13 (b), D178).
This bounds the hold time of the write lock for a scanning call at about 240 ms, far above the
20 ms of ARCHITECTURE 5.4. Until equality and reference filters are answered from an index (work
package T224, design in D183), the hold time of KPI A is a blocker, and a result measured before
then states that it was measured without an index. No value is published yet.
