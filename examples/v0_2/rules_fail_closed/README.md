# Fail closed rule examples (AC-33)

Five small apps, one for each part of acceptance criterion AC-33 "Rules fail closed". Each app
is a must deny case: it shows one way a permission rule could fail open and states, by hand, what
the compiler or the server has to do instead. They are the input for the v0.2 checker goldens
and for the security suite under `tests/acceptance/v0_2/`, which turns them into automated tests
named `ac_33_<topic>`.

Rules for this directory:

* Every `.expected` file is written by hand from the language documents (SPEC 5 and 12,
  SYNTAX 4.4 to 4.6, ARCHITECTURE 5.5, 5.7 and 9), never from compiler or server output. If an
  implementation disagrees, the implementation is wrong until a reviewed change to this
  directory says otherwise.
* Every `.ostl` file has exactly one `.expected` file with the same stem, and vice versa.
* Each must deny step comes with an allowed control step on the same rule, so a server that
  denies everything does not pass.
* Principals are development identities of a debug build (AC-12). Ops, reject reasons and
  `Call`/`Return` are those of the sync protocol (ARCHITECTURE 5.4).
* No file here is run by the gate yet; the compiler cannot read v0.2 programs at this point.

## Format of an expected file

Plain text, one `key: value` per line, indented lines continue the key above.

| Key | Meaning |
|---|---|
| `case`, `ac` | stem of the case and the part of AC-33 it covers |
| `kind` | `compile` (only `ostrel check`), `ops` (crafted sync ops), `client` (generated client in the browser job), `query` (rows a principal receives) |
| `compile` | expected result of `ostrel check` for cases that must compile |
| `given` | state created before the steps, by the named principal through allowed writes |
| `steps`, `probes` | numbered actions, each with its `expect`; `db` states the database after the step |
| `final db` | database after the last step |
| `must not` | the fail open results the case exists to catch |
| `ASSUMPTION` | a reading that the documents do not state yet; to be confirmed or corrected |

## Cases

| File | AC-33 | Fail open result it catches |
|---|---|---|
| `a_optional_ordering` | (a) | `priority >= 3` with `priority: Int?` compiles, or only warns |
| `b_rule_runtime_error` | (b) | a failed division inside `not (...)` is read as `false`, so the rule allows |
| `c_set_directions` | (c) | a removal is allowed because the rule expression is true, though the rule mentions only additions |
| `d_call_denied_fallback` | (d) | a fallback swallows more than `Offline`: `discount(c) ?? 0` shows `0` after `Denied` or `OutcomeUnknown`, though `??` turns only `Offline` into its fallback (ARCHITECTURE 3.2 G11, 5.7) |
| `e_limit_after_see` | (e) | `limit 3` and `last 3` are cut before the `see` rule filters, so a reader gets fewer rows |

## Open points

* (a) and (c): the diagnostic codes and texts for the type error and the set direction warning
  are assigned by the v0.2 checker task. The expected files pin line, column, severity and the
  count of diagnostics only.
* (a): the column is the `>=` operator, following E0300 in `tests/errors/README.md`. This is an
  ASSUMPTION until the checker catalog states the position.
* (d): see the ASSUMPTION in `d_call_denied_fallback.expected` on how an unhandled call error ends
  a client function.
