# tools/fuzz

Seeded, std only fuzzer for the Ostrel compiler front end (SPEC AC-36, architect
decision #46: no `cargo-fuzz`). Owner: QA.

It generates inputs from a seed, runs the `ostrel` CLI on each input in a fresh process
with a time and a memory limit, and keeps every input that does not end with exit code
0 or 1.

## What is checked

| Requirement (AC-36, AC-04) | How |
|---|---|
| Lexer, parser and checker | target `check`: `ostrel check <input>` |
| Formatter | target `fmt`: `ostrel fmt <input>` |
| No panic, no signal | exit code 101 (`panic`), any other exit code except 0 and 1, and any signal are findings |
| No input over 1 s | per input wall clock limit 1 s for `check` and `fmt` (5 s for `run`, SPEC 12.4); a killed run is a `timeout` finding |
| No memory above 512 MB | `ulimit -v 524288` (KiB) before the target starts; an allocation above the limit fails, the process aborts, and the signal is a finding |
| At least 30 minutes each | `run.sh` runs every campaign of `seeds.txt` for 30 minutes by default |
| Command and seeds in the repo | `run.sh`, `seeds.txt` |
| Crashing inputs become corpus cases | see "Findings" below |

## Inputs

Every input is a pure function of `(seed, iteration, corpus)`; nothing depends on the
time, the order of runs or the machine. Half of the inputs (when a corpus is given) are
byte mutations of corpus files; the rest are generated programs:

* Grammar aware generation after SYNTAX v1.1 section 7: script programs with `fn main()`
  (v0.1 subset) and full v0.3 programs (`app`, `data` with rules and checks, `enum`,
  `var`, `view` with elements, attributes, style tags and handlers, `style` with CSS,
  `extern js`), string literals with interpolation and every escape of G1.
* Twists on about a third of the programs: deep parenthesis and view nesting, flat
  operator chains around the AST height limit 2 048 (D54), tabs in indentation, raw bidi
  controls (D53), nested interpolation past depth 32, invalid escapes, unterminated
  strings, invalid UTF-8 and NUL bytes, CRLF, indentation jumps, keyword soup, long lines.
* Byte mutation: bit flips, random bytes, deletions, duplications, splices of a second
  corpus file, line swaps and insertion of lexer relevant tokens.

## Commands

Build (std only, no network):

```sh
export CARGO_TARGET_DIR="$PWD/target"
cargo build --locked -p ostrel_cli
cargo build --manifest-path tools/fuzz/Cargo.toml
```

Full AC-36 run (every line of `seeds.txt`, 30 minutes each, release builds):

```sh
bash tools/fuzz/run.sh          # or: bash tools/fuzz/run.sh 5m for a short local run
```

One campaign:

```sh
target/debug/ostrel-fuzz run --bin target/debug/ostrel --target check --seed 1 \
  --duration 30m --corpus tests/hostile --corpus examples
```

Other subcommands: `gen` writes generated inputs to a directory, `repro` rebuilds one
input from seed and iteration, `help` prints all flags. Exit codes: 0 clean, 1 findings,
2 usage or I/O error.

The report of `run` ends with `RESULT: CLEAN` or `RESULT: FINDINGS` and lists the count
per outcome, the slowest clean input and every finding file.

## Findings

Findings are written to `tools/fuzz/findings/` (ignored by git), at most `--keep` (20)
per outcome label and campaign, as `TARGET-OUTCOME-sSEED-iITERATION.ostl` plus a `.txt`
with the command, outcome, time, stderr head and the `repro` command. `repro` with the
same corpus directories rebuilds the input byte for byte.

Every finding becomes a regression case: copy the `.ostl` file into `tests/hostile/`
under a descriptive name with its expectation (exit 0 or 1) in the same change as the
fix, and refer to the seed and iteration in the commit message.

## Assumptions

* The front end is fuzzed through the CLI, one process per input, because only a
  process can be killed after 1 s and limited to 512 MB without unsafe code.
* The memory limit bounds the address space, which is never smaller than the resident
  memory, so the check is at least as strict as AC-36 asks.
* Until the root `Cargo.toml` lists `tools/fuzz` as a workspace member, this crate is its
  own workspace (`[workspace]` in `Cargo.toml`), and the gate does not run its tests;
  run them with `cargo test --manifest-path tools/fuzz/Cargo.toml`. The crate has no
  dependencies, so its lock file carries no information and is not committed.
