# Contributing

## Workflow

1. Every change starts from a task with a clear owner and acceptance criteria.
2. Branch from `dev`: `<team>/<name>/<task>-<topic>`, for example `t3/vera/12-parser`.
3. Run the quality gate locally before pushing: `bash ci/gate.sh`.
4. A reviewer from your team checks the change. Nobody approves their own work.
5. The integration team merges approved work into `dev` after the gate passes on the merged result.
6. Releases are cut from `dev` after sign-off by Quality Assurance and the security review team,
   then fast-forwarded to `main` and tagged.

## Commit messages

- Subject in the imperative mood, at most 72 characters: `Add input validation to parser`
- Blank line, then a short explanation of why, if it is not obvious.
- Reference the task: `Refs #12`

## Rules enforced by the repository

- `main`, `dev`, `gh-pages` and `packaging` are protected.
- History on protected branches is never rewritten. Branches and tags are never deleted.
- Released versions are immutable. A fix to a release ships as a new version.
- Commits must carry the author's own identity.
