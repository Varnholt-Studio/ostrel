# Default theme (`std/theme/`)

Status: draft for v0.3 (work package T3-3). Owner: T3.

The default theme styles the std elements of SYNTAX 4.8 and the sync state hooks of SYNTAX 4.7.
It ships with every client build as part of the app shell (ARCHITECTURE 5.10) and is excluded
from the line count as a standard theme (MEASUREMENT 1.2), subject to the D30 guard.

## Files

| File | Content |
|---|---|
| `tokens.json` | Source of truth: every token with kind, light value, dark value and a short description |
| `theme.css` | The stylesheet: token declarations on `:root`, dark values, base, layout and hook rules |
| `theme.test.mjs` | Contract tests (`node --test std/theme/theme.test.mjs`, also run by the gate) |

## Tokens

A token name is lowerCamel ASCII. In a `style` block, `$name` reads the token (SYNTAX 4.8).
Each token is also a CSS custom property: `--ostrel-` followed by the name in kebab case, with
a dash before every upper case letter and digit.

| `$token` | Custom property |
|---|---|
| `$accentSoft` | `--ostrel-accent-soft` |
| `$muted` | `--ostrel-muted` |
| `$space2` | `--ostrel-space-2` |

Token kinds: `color` (hex only, so contrast is testable), `length`, `font`, `number`, `shadow`.
A token value is a single inert CSS value: no `;`, braces, quotes, backslash, `@`, `url(` or
comment. The compiler may splice it into CSS without escaping.

Contrast (tested in both schemes, WCAG AA 4.5:1): `text` and `muted` and `danger` on `bg`,
`text` on `surface` and on `accentSoft`, `accentText` on `accent`.

Dark values apply under `@media (prefers-color-scheme: dark)`.

## Hooks

The theme never styles app classes. Selectors use only elements, `data-*` and `aria-*`
attributes, and classes with the reserved `o-` prefix.

| Hook | Set by | Style |
|---|---|---|
| `.o-row`, `.o-col`, `.o-split` | elements `row`, `col`, `split` | flex row, flex column, side plus main grid |
| `.o-scroll-end` | `scroll: end` | scroll container |
| `.o-entry` | element `entry` | field plus accent submit button |
| `.o-notice` | std error notice | danger border and text |
| `select`, `option` | element `pick` | control look of `input`, accent border on hover, focus ring, dimmed when disabled |
| `[data-look="title"]`, `"strong"`, `"muted"` | `look:` | larger and bold, bold, muted color |
| `[aria-current="true"]` | `selected:` | soft accent background |
| `[data-pending="true"]` | root of every `for` item | reduced opacity |
| `[data-rejected="true"]` | root of every `for` item | danger color, line through |

## Assumptions

These need confirmation by the T3 lead and the owners named before the view core and the JS
backend depend on them.

1. ASSUMPTION: `$name` compiles to `var(--ostrel-<kebab>)`, not to the literal value, so a theme
   override (MEASUREMENT 1.4, counted) can change a token at run time. Owner: T1 (CSS mode
   lexer), T3 (`ostrel_codegen_js`).
2. ASSUMPTION: an unknown `$name` is a compile error. The checker reads the names from
   `tokens.json`.
3. ASSUMPTION: the view core (T3-1) renders `row`, `col`, `split`, `entry` and `scroll: end`
   with the `o-` classes above, `look:` as `data-look`, `selected:` as `aria-current="true"`,
   and pending and rejected as `data-pending="true"` and `data-rejected="true"`.
4. ASSUMPTION: `.o-` is reserved; the checker rejects an app style tag starting with `o-`.
5. ASSUMPTION: the view core renders `pick` as a native `select` with one `option` per entry of
   `options:` (both tags are on the allowlist in `runtime/js/view/safe/attrs.js`). The theme
   keeps the native arrow and popup, so no icon or `appearance` override is needed.

## Out of scope for this draft

Theme selection or switching by apps, icons, motion tokens, and a theme override syntax.
