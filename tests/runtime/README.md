# Runtime error goldens (AC-52)

Each case is a script program that compiles without diagnostics and fails at run time:

* `NAME.ostl`: a v0.1 program (SYNTAX 4.11) with `fn main()` and exactly one runtime fault.
* `NAME.expected`: the exact stdout, that is every line printed before the error.
* `NAME.expected_err`: the exact stderr, one line.
* `NAME.args` (optional): one CLI argument per line, passed before the file path.

Expected output was written by hand from the language documents (SPEC 5.0 AC-52, 12.3, 12.4,
ARCHITECTURE 3.4), before the compiler could produce it. It is never blessed from compiler
output. If the compiler disagrees with a golden, the compiler is wrong until a reviewed change
to this directory says otherwise. `examples/v0_1/` holds only programs that exit 0 (D41).

## Contract checked by the golden runner (`ci/checks/55_golden.sh`, suite `ac_52_runtime_goldens`)

1. `ostrel run [ARGS] tests/runtime/NAME.ostl` exits with code 1 (never 0, 2, 101 or a signal).
2. stdout equals `NAME.expected` and stderr equals `NAME.expected_err`, byte for byte.
3. Line format: `PATH:LINE:COLUMN: runtime error[KIND]: MESSAGE`, `PATH` exactly as given on
   the command line, `LINE` and `COLUMN` 1 based and counted in Unicode scalar values (SPEC 12.1).
4. Every kind `IntOverflow`, `DivisionByZero`, `CallDepth`, `StepLimit` and `TextLimit` has at
   least one case. `HeapLimit` is covered by the hostile corpus, not here (SPEC 5.0 AC-52).
5. No case depends on the exact `StepLimit` boundary (SPEC 12.4): one case is far beyond the
   default limit, one uses a small `--max-steps` value.

## Cases

| Kind | Case | Position | What it pins |
|---|---|---|---|
| IntOverflow | `int_overflow_add` | 7:9 | `INT_MAX - 1 + 1` is fine, `INT_MAX + 1` is not |
| IntOverflow | `int_overflow_sub` | 6:9 | `INT_MIN + 1 - 1` is fine, `INT_MIN - 1` is not |
| IntOverflow | `int_overflow_mul_in_callee` | 5:3 | product far beyond 64 bits, reported in the callee |
| DivisionByZero | `division_by_zero` | 4:3 | `/` with a negative dividend, reported in the callee |
| DivisionByZero | `remainder_by_zero` | 4:3 | `%`, including `0 % 0` |
| CallDepth | `call_depth_boundary` | 7:3 | 10 000 frames run, frame 10 001 fails (SPEC 12.4) |
| CallDepth | `call_depth_unbounded` | 5:3 | recursion without a base case ends cleanly |
| StepLimit | `step_limit_default` | 11:9 | far beyond the default of 100 000 000 steps |
| StepLimit | `step_limit_small` | 11:9 | `--max-steps 1000` on a program that passes by default |
| TextLimit | `text_limit` | 12:9 | 16 777 216 bytes fit, one byte more fails (D44) |

## Messages

| Kind | Message |
|---|---|
| IntOverflow | `` `Int` result is outside the range of -9007199254740991 to 9007199254740991 `` |
| DivisionByZero | `division or remainder by zero` |
| CallDepth | `call would exceed the limit of 10000 frames` |
| StepLimit | `` program exceeded its step limit; raise it with `--max-steps` `` |
| TextLimit | `` `Text` value would be longer than 16777216 bytes `` |
| HeapLimit | `VM heap would grow beyond 268435456 bytes` (no golden, see rule 4) |

## Assumptions (open question in `#spec`, Refs #16)

The language documents fix the kind names and the line format, but not the message texts and
not every position. Until the spec owner answers, these cases rely on:

* ASSUMPTION 1: one fixed message per kind, the texts above. `StepLimit` names no number, so the
  text does not change with `--max-steps`.
* ASSUMPTION 2: `IntOverflow`, `DivisionByZero` and `TextLimit` are reported at the start of the
  expression whose evaluation fails: for a binary operator the start of its left operand, for an
  interpolated string its opening quote. `CallDepth` is reported at the start of the call
  expression (SPEC 12.4).
* ASSUMPTION 3: `StepLimit` is reported at the start of the call expression in `fn main` that is
  running when the limit is reached (at the failing instruction only when it is in `main`
  itself). SPEC 12.4 forbids goldens that depend on the exact boundary, and the position of the
  failing instruction inside a recursive function does depend on it, so a byte exact stderr
  golden needs a position that does not.
