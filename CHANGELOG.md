# Changelog

All notable changes are documented here.

## Unreleased

## v0.1

First milestone: a walking skeleton of the compiler. Source text goes through the full
pipeline and produces a result. This release covers the core language only.

### Added

- Command line interface `ostrel` with two commands: `ostrel check FILE` compiles a program and
  reports diagnostics, `ostrel run [--max-steps N] FILE` compiles a script program and runs its
  `fn main()`. `ostrel --help` and `ostrel --version` print usage and version.
- Exit codes: 0 on success, 1 for any diagnostic or runtime error, 2 for a usage error. Exit code
  70 marks an internal defect of the compiler and exit code 101 a crash (panic) of the compiler.
  Both are always a bug.
- Lexer with indentation based layout, string interpolation and escapes, and strict handling of
  source text: invalid UTF-8 (`E0014`) and bidirectional control characters (`E0011`) are
  rejected with their own codes. A byte order mark at the start of a file is skipped; one inside
  code, like any other unexpected character, is rejected as `E0008`.
- Parser, type checker, intermediate representation and a bytecode virtual machine for the core
  language: `Int`, `Bool` and `Text` values, `let`, functions with and without a result, `if` and
  `else`, `return`, recursion and `print`.
- Checked integer arithmetic: overflow, division by zero and remainder by zero stop the program
  with a runtime error instead of producing a wrong value. Call depth, heap size and text length
  are bounded and report their own runtime error kinds.
- Diagnostics with stable codes (`E0001` and following) and exact `file:line:column` positions.
  Runtime errors use one fixed message per kind. The registered codes are listed in
  `tests/errors/README.md`.
- Golden test suites: 13 example programs in `examples/v0_1/`, 55 compile error cases in
  `tests/errors/` and 10 runtime error cases in `tests/runtime/`. Expected output is written by
  hand from the language definition and compared byte for byte.
- Hostile input suites in `tests/hostile/` that feed malformed and adversarial source to
  `ostrel check` and `ostrel run` under CPU time and memory limits. Any crash, hang or panic fails
  the suite.
- Pinned Rust toolchain (1.95.0 in `rust-toolchain.toml`) and an offline, deterministic CI gate
  (`ci/gate.sh`) that runs after `cargo fetch --locked` on a fresh clone.

### Known limitations

- Only the core language is implemented. There is no browser target, no database access, no
  data synchronization, no permission rules and no TS/JS bridge in this release. These are
  planned for v0.2 and v0.3.
- `Float` is not available yet; using it is a compile error (`E0100`).
- The step limit (`--max-steps`, default 100 000 000) counts executed instructions. It does not
  yet bound the running time of a program in every case.
