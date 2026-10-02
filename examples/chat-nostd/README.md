# Chat example (strict D30 reading)

The same offline capable realtime chat as `../chat/`, written in the Ostrel
programming language, with every standard library UI convenience that so far
has only one user replaced by app code. Source: `chat.ostl`.

## Replaced by app code

| Std item in `../chat/` | Replacement here |
|---|---|
| `split` | `.app` grid rule on a `row` |
| `entry` | draft variables, `createRoom` and `send` functions, `input` plus `button` rows |
| `selected:` | `~active` class plus rule |
| `scroll: end` | `.log` rule with `column-reverse` plus `sort made desc limit 200` (instead of `last 200`) |
| `look:` | `.strong`, `.title`, `.muted` rules |
| theme pending and rejected styling | `.pending` and `.rejected` rules |

Still used as std: base elements (`row`, `col`, `text`, `button`, `input`),
`bind:`, `hint:`, std auth and `User`, `Time.clock`, theme tokens.

A failed `check` inside `make` aborts `createRoom` or `send`, so the draft text
stays in the input.

## Scope

Identical to `../chat/` (see its README).

## Status

* The compiler cannot parse this file yet. A parse check is added once the
  parser lands.
* Line count: 88 non blank lines by hand. This is not an official number. The
  official count comes only from `tools/loc` after `ostrel fmt` on a tagged
  commit, together with the D30 ruling per std item.
