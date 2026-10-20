# Red review T148: weighted VM steps (T109, D94)

Reviewed: `crates/ostrel_vm/src/exec.rs` and `bytecode.rs` at dev `ebce16c` (contains T109
`35d703a`), against ARCHITECTURE 3.4 "Step cost" (D94) and the follow up points of the T109
review (ivo, #1200, point 4).

Question: does any instruction still do work that grows, or costs much more than one plain
instruction, without being weighted, so that the step limit does not bound the running time?

Result: no S1 or S2. No instruction does unweighted work that grows with an operand. Three
findings: F1 (S3) fixed cost of allocations is not weighted, F2 (S3) printed volume has no
limit of its own, F3 (S4) a `Concat` refused by `HeapLimit` can be reported as `StepLimit`.

## Method

* Every `Op` in `bytecode.rs:38-110` checked against `work_bytes` (`exec.rs:328-354`) and
  against what its arm in `execute` (`exec.rs:379-484`) really does.
* Probes in `probes/`, run with a release build of `ostrel` by `repro.sh`. Times are CPU
  seconds (user, D93). The build machine was heavily loaded during the runs (load average
  about 13 on 2 cores), so the ratios to `tests/runtime/step_limit_default.ostl` measured
  in the same run are the evidence, not the absolute values.

## Coverage of every instruction

| Op | Work | Weighted | Verdict |
|---|---|---|---|
| `LoadInt`, `LoadBool`, `Copy`, `Neg`, `Not`, `Arith`, `Order`, `Jump`, `Branch` | constant, `Copy` of a `Text` is an `Rc` clone | no need | ok |
| `LoadText` | copies the constant on its first load only, cached in `Vm::texts` (`exec.rs:250-263`); total bounded by the 10 MiB source | no (D94) | ok |
| `Equal` | `str` equality compares lengths first; bytes only for equal length | yes, equal length only | ok |
| `ToText` | at most 20 bytes of digits, but two or three heap allocations | no | F1 |
| `Concat` | part lengths plus references; above `TextLimit` only references (`exec.rs:340-344`) | yes | ok for bytes; F1 for the fixed allocation cost; F3 at `HeapLimit` |
| `Call` | registers of the callee plus a `Vec` of arguments | yes, registers | ok; allocation part of F1 |
| `Print` | text length; `Int` formatting at most 20 bytes | yes | ok for time; F2 for volume |
| `Return` | drops the frame; its registers were charged by the `Call` | via `Call` | ok |
| drop of a `Text` | free of a block whose bytes were charged when it was built | via creation | ok |

`step_cost` uses `div_ceil` and the running total uses `checked_add` (`exec.rs:38-40`,
`368-376`), so no overflow; the `TextLimit` path keeps `TextLimit` ahead of `StepLimit`
(`tests/runtime/text_limit` unchanged).

## F1 (S3): the fixed cost of allocations is not weighted

Commit: dev `ebce16c`. Probes: `probes/f1_interpolate_text.ostl`,
`probes/f1_interpolate_int.ostl`.

Steps: build release, `bash docs/red/t109/repro.sh`.

Expected (D94, `exec.rs:28-34`: "the step limit bounds the running time"): 100 000 000 steps
take about as long for any program as for `step_limit_default` (fib).

Actual (same run):

| Program | CPU s until `StepLimit` | Ratio |
|---|---|---|
| `step_limit_default.ostl` | 1.57 | 1.0 |
| `f1_interpolate_text.ostl` (`"{s}"`, one part `Concat`) | 4.12 | 2.6 |
| `f1_interpolate_int.ostl` (`"{m}"` of an `Int`, `ToText` plus `Concat`) | 5.92 | 3.8 |

Repeated runs gave 1.68/6.05 and 1.79/6.06 for fib and the `Int` probe.

Cause: `"{m}"` lowers to `ToText` (1 step) and a one part `Concat` (2 steps,
`ostrel_ir/src/lower.rs:339-360`). Together they do about six heap allocations
(`to_string`, the `String` and `Rc` of `alloc_text` twice, the `Vec` of `values` in
`exec.rs:435`) plus the frees of the values they replace. `WORK_BYTES` weights bytes only;
an allocation adds no step, but takes several times the time of a plain step.

Impact: the bound holds within a constant factor (about 4), so this is no denial of
service like #1136. But a program made of interpolations reaches the default `StepLimit`
only after about 6 s CPU, above the 5 s run limit (D93, AC-04, SPEC 12.4 as answered in
#220: "StepLimit not reachable within 5 s is a BLOCKER, do not loosen"). No case of the
AC-04 corpus does this today. On the server of v0.2 the same factor applies to every
request.

Proposal (owner T2, decision architect): charge a fixed surcharge per heap allocation
(measured, for example 2 steps for each `alloc_text` and each new frame), or make the one
part `Concat` of a `Text` reuse its operand. Add the `Int` probe as a hostile case with a
CPU margin like `run_long_loop_by_recursion`.

## F2 (S3): printed volume is bounded only by steps

Commit: dev `ebce16c`. Probe: `probes/f2_print_volume.ostl`.

Steps: `ostrel run docs/red/t109/probes/f2_print_volume.ostl | wc -c`.

Expected: a limit on what one run can write, or a documented bound.

Actual: 6 358 565 243 bytes (about 6.4 GB) reach stdout before `StepLimit`, in 1.4 s user
plus 1.7 s system CPU. `Print` is weighted at 64 bytes per step, so the default limit
allows about 1e8 times 64 bytes.

Impact: time is bounded (D94 met), volume is not: a short program fills a disk or a log.
ARCHITECTURE 5.9 lists no output limit. Relevant once `print` reaches a server log.

Proposal: architect decides an output limit per run (new kind or reuse of `HeapLimit`), or
documents the bound in 5.9 and `BENCHMARKS.md`.

## F3 (S4): a `Concat` refused by `HeapLimit` is reported as `StepLimit`

This is point 4 of the T109 review (ivo, #1200), now reproduced.

Commit: dev `ebce16c`. Probe: `probes/f3_heap_limit_edge.ostl`.

Steps: `ostrel run --max-steps N docs/red/t109/probes/f3_heap_limit_edge.ostl` for N from
4 194 986 to 4 326 057.

Expected (ARCHITECTURE 3.4, D94: "an instruction refused by `TextLimit` or `HeapLimit`
reports that kind"): `HeapLimit` at 10:14, as without `--max-steps`.

Actual: `StepLimit` at 17:9 for all 131 072 values in that window; `HeapLimit` from
4 326 058 on. `work_bytes` charges the full length of a `Concat` that `alloc_text` will
refuse for the heap (`exec.rs:340-344` checks only `max_text_bytes`), here 131 074 steps.
The same code path applies to a `Call` whose frame is refused by `HeapLimit` (read, not
probed).

Impact: the reported kind depends on the exact boundary; no golden is affected (SPEC 12.4
forbids boundary goldens).

Proposal: charge only the references when the result plus `TEXT_OVERHEAD` would exceed the
heap that is left, as for `TextLimit`; or narrow the sentence in ARCHITECTURE 3.4 to
`TextLimit`.

## Not findings

* `LoadText` unweighted is safe: each constant is copied once per run and cached.
* `Equal` with texts of different length reads no bytes, so B = 0 is right.
* Overcharging (the same `Rc` compared with itself still costs its length) is harmless.
