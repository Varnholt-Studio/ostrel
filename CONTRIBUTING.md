# Contributing

## Workflow

1. Every change starts from a task with a clear owner and acceptance criteria.
2. Branch from `dev`: `<team>/<name>/<task>-<topic>`, for example `t3/vera/12-parser`.
3. Run the quality gate locally before pushing: `bash ci/gate.sh`.
4. A reviewer from your team checks the change. Nobody approves their own work.
5. The integration team merges approved work into `dev` after the gate passes on the merged result.
6. Releases are cut from `dev` after sign-off by Quality Assurance and the security review team,
   then fast-forwarded to `main` and tagged.

## Tests

The gate (`ci/checks/50_tests.sh`) runs every suite offline. Code in a language without tests
fails the gate. Tests are never deleted, weakened or skipped without a decision of the QA lead;
a skipped test needs an entry in `ci/quarantine.txt`.

- **Rust:** the toolchain is pinned in `rust-toolchain.toml`. Unit tests live in a
  `#[cfg(test)] mod tests` next to the code, integration tests in `crates/<crate>/tests/`.
  Run them with `cargo test --workspace --locked`. A test that is evidence for an acceptance
  criterion is named `ac_NN_<topic>`, for example `ac_04_deep_nesting`.
- **JavaScript:** tests are files named `*.test.js` or `*.test.mjs` next to the code, written
  with the built in `node:test` and `node:assert/strict` modules. No npm packages: the gate runs
  `node --test` on these files. Type declarations (`.d.ts`) need no tests.
- **Golden files:** the expected output sits next to its case with the same base name
  (`.stdout`, `.stderr`, `.expected_err`, `.tokens`, `.ast`, `.ir`). Expected files are written
  by hand and reviewed by a second person; they are never copied from compiler output. Updating
  goldens needs the explicit bless switch, the diff is part of the review, and `*.new` files are
  never committed.
- **Python** (tooling only): `test_*.py` files using `unittest`.

## Commit messages

- Subject in the imperative mood, at most 72 characters: `Add input validation to parser`
- Blank line, then a short explanation of why, if it is not obvious.
- Reference the task: `Refs #12`

## Rules enforced by the repository

- `main`, `dev`, `gh-pages` and `packaging` are protected.
- History on protected branches is never rewritten. Branches and tags are never deleted.
- Released versions are immutable. A fix to a release ships as a new version.
- Commits must carry the author's own identity.
