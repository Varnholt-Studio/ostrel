# Spike: CRDT and render budget for KPI A

Work package TIGER-1 (ARCHITECTURE 15.3), checks the client side budget of
ARCHITECTURE 5.8 before the real runtime exists. Refs #33.

This is a spike. Nothing in `runtime/js` or any crate depends on it, and it will be deleted
once the runtime and the QA harness in `bench/` measure the same steps.

## What it models

One receiving client with 10 000 issues (dataset rules of MEASUREMENT 2.3, seed 42), a board
with one virtualised column per status (live query `status == s` sorted by `rank`) and a
detail panel. Three remote editors produce background edits; editors A and B produce the
conflict probes of MEASUREMENT 2.4 in the 40 / 40 / 20 mix (scalar field, text, reorder).

Every probe message goes through the four steps of the 5.8 budget, timed separately:

| Step | Module | Budget p99 |
|---|---|---|
| Decode and validate the `Ops` message | `src/decode.mjs` | 1 ms |
| Merge into the local replica | `src/store.mjs` (`apply`), `src/orset.mjs`, `src/seq.mjs` | 2 ms |
| Live query invalidation (indexed, incremental) | `src/store.mjs` (`LiveQuery.update`) | 10 ms |
| View diff and DOM patch | `src/view.mjs` | 20 ms |
| Wait for the next `requestAnimationFrame` (MEASUREMENT 2.5 t1) | `src/harness.mjs` | headroom |

Each probe is checked against an oracle computed independently of the receiving replica:
the HLC rule for registers, and for text a separate tree model (`src/rga.mjs`, no code
shared with `src/seq.mjs`) built from the snapshot text and every text op the row received,
with the two editors' ops replayed in the opposite delivery order. A mismatch fails the run,
as in MEASUREMENT R0.5. Two tests sabotage the merge on purpose and assert that the oracle
notices.

Ops from the server carry their log position `ss` (ServerSeq). The store drops ops at or
below its ServerSeq high water mark (D62) and applies a batch as a whole: a text op that
names an unknown element refuses the batch before anything changes.

## Run

    node measure.mjs                       # Node, stub DOM: steps 1 to 3 are meaningful
    node measure.mjs --browser             # headless Chromium, real DOM and frames
    node measure.mjs --browser --pause 20  # idle page between probes
    node measure-tags.mjs                  # Set ops with large tag lists (risk 7)
    node --test test/*.test.mjs

Options: `--probes N`, `--warmup N`, `--count N`, `--seed N`, `--out report.json`,
`--chrome PATH` (default: `$CHROME` or a Chromium found on the machine). The browser mode
drives Chromium over the DevTools protocol with Node's built in WebSocket and serves the
page from a loopback server; it needs no npm packages and no network.

Numbers from this script are development numbers on a shared machine. Per MEASUREMENT
R0.7 they are not published here; the team discusses them internally and the official
values come from the `bench/` harness on GitHub Actions.

## Findings

1. **Index search must use the indexed key.** When one batch moves several rows of the
   same live query, the rows already hold their new sort keys while the array still has the
   old order. A binary search over live values then misses rows (found by the property test
   in `test/store.test.mjs`). `LiveQuery` keeps the key each member was indexed under; the
   runtime index (T5 with T3) needs the same rule.
2. **Probe design decides what the oracle can see.** With the obvious workload the op that
   arrives last also carries the higher HLC, and text probes never insert at the same
   origin. A merge that lets the last arrival win, or ignores the tie order of concurrent
   inserts, then passes every check. The harness randomises stamp order, delivery order and
   same origin inserts. Proposal for MEASUREMENT 2.4 (QA): require all three.
3. **The frame wait dominates and depends on arrival phase.** Engine work for a probe is a
   small fraction of one frame. The wait until the next `requestAnimationFrame` is close to
   a full frame interval when messages arrive back to back, and close to zero on an idle
   page in headless Chromium. KPI runs should report the frame wait as its own value and
   not treat it as engine cost or as noise.
4. **The t1 clock point stops before paint.** A `requestAnimationFrame` callback runs
   before style, layout and paint of that frame, so t1 of MEASUREMENT 2.5 does not include
   rendering of the patched nodes. Proposal (QA): take t1 in a task posted from that
   callback (after the frame is produced), or report the CDP paint distance for every probe
   instead of 5 percent.
5. **Timer resolution.** Without cross origin isolation Chromium rounds `performance.now()`
   to 100 microseconds, which hides every step below that. The spike server sends
   `Cross-Origin-Opener-Policy` and `Cross-Origin-Embedder-Policy`; the `bench/` server
   should do the same.
6. **First touch of a long text.** The spike keeps a snapshot description as one string and
   expands it into elements on the first remote op, which stands in for one run of the run
   length encoding. That expansion is the largest single merge cost seen. The sequence core
   (5.2) should split runs instead of expanding whole documents.

7. **Rank keys must not end in '0'.** No key lies directly below such a key (nothing is
   between `a` and `a0`), so `rankBetween` cannot place a row in front of it. Generated keys
   never end in '0'; decode now refuses remote ranks that do, and `rankBetween` refuses
   such bounds. The runtime `Rank` needs the same rule on both sides.
8. **Dedupe belongs to the log position, not the sender's seq.** Dropping ops whose
   replica seq is not above the last one seen loses ops that reach the log out of seq order
   and lets a redelivered add revive a removed element. The ServerSeq mark of D62 fixes
   both; `test/store.test.mjs` covers the reconnect overlap case of ARCHITECTURE 5.4.
9. **Batches need a reference check before merge.** Applying op by op and throwing at the
   first unknown text origin left half a batch applied. The check costs one set lookup per
   text op and makes a batch all or nothing.
10. **Tag lists are cheap in the merge.** `measure-tags.mjs` puts T live tags on one
    element. Add, re-add and split remove stay at a few hundredths of a millisecond per
    batch up to 64 tags and well under the merge budget at 256 tags (four remove ops). The
    larger cost is reading a big set in code point order, which sorts on every call; the
    runtime should keep the sorted order instead of sorting per render.

## Not covered (follow up)

* Rust side (`ostrel_crdt`), server merge and the real wire format of
  `ostrel_sync::protocol`. The op shape here is an assumption of the spike.
* Fugue: the sequence CRDT here is RGA with tombstones, enough for the cost class, not the
  algorithm of 5.2.
* IndexedDB writes of received ops, WebSocket framing, three browser clients, re-scoping and
  `Held` diffing (risk 3 of ARCHITECTURE 16).
* CPU throttling and the 4G variant of MEASUREMENT 3.
