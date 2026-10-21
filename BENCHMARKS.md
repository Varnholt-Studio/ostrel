# Benchmarks

Status: draft, started with work package T207 (Refs #207). No benchmark values are published
yet. This file collects the results required by SPEC (AC-27, AC-28, AC-51, AC-61, AC-65) and
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
the audit file. Such a result states:

| Field | Value |
| --- | --- |
| Elevated calls in the measured path | not measured yet |
| Database adapter | not measured yet |
| Disk of the audit file (type, file system, write cache) | not measured yet |
| Hold time of the write lock, p50 and p99, with elevated calls | not measured yet |
| Hold time of the write lock, p50 and p99, without elevated calls | not measured yet |

Values measured on the memory adapter do not count here: it keeps the audit in memory and hides
the sync. A p99 hold time above 20 ms in the KPI A setup is a blocker (ARCHITECTURE 16, risk 12).
A group sync that covers several entries with one sync is not part of v0.3 and may come later by
a DECISION. Details: [`docs/sync.md`](docs/sync.md), section 2.

### 2.2 Audit file and operator actions

Unlinking the audit file between the server's path check and its append writes the entry into the
unlinked file, where it is lost (ARCHITECTURE 5.7, ARCHITECTURE 16 risk 13 (d)). This changes no
benchmark value; it is listed so that the audit guarantee is not read as stronger than it is.
Details: [`docs/security.md`](docs/security.md), section 2.4.
