# Error goldens (AC-03)

Each case is a pair:

* `NAME.ostl`: a v0.1 program (SYNTAX 4.11) with exactly one fault.
* `NAME.expected_err`: the exact stderr the CLI must print for it, one diagnostic per line.

Expected output was written by hand from the language documents, before the compiler could
produce it. It is never blessed from compiler output. If the compiler disagrees with a golden,
the compiler is wrong until a reviewed change to this directory says otherwise.

## Contract checked by the golden runner

1. `ostrel check tests/errors/NAME.ostl` exits with code 1 (never 0, never 101, never a signal).
2. stderr equals `NAME.expected_err` byte for byte.
3. Line format: `PATH:LINE:COLUMN: error[CODE]: MESSAGE`, where `PATH` is the path exactly as
   given on the command line (the runner passes `tests/errors/NAME.ostl` from the repo root).
4. `LINE` and `COLUMN` are 1 based, lines are separated by LF. `COLUMN` is 1 plus the number of
   Unicode scalar values before the position on its line (a tab counts as one), never bytes and
   never UTF 16 units (SPEC 12.1). `lex_column_counts_scalars` pins this: its expected column is
   21, while counting bytes would give 26 and counting UTF 16 units 22.
5. The position is the start of the offending token or character named in the table below.
6. One fault gives one diagnostic. Follow up errors caused by the same fault (cascades) are a bug.
   The checker does not run on a file with lexer or parser errors.
7. Every case contains a valid `fn main()` unless the case is about `main`, so no case also
   triggers E0400. `ostrel check` reports E0400 to E0402 as well as `ostrel run`, because a
   script program without a valid `main` is incomplete (SPEC 12.2).
8. Every case uses only constructs that the v0.1 parser (T1-3: items, `fn`, `let`, `if`/`else`,
   `return`, expressions) reads, so each expected diagnostic is reachable at v0.1.

## Diagnostic codes

| Code | Case | Position | Message |
|---|---|---|---|
| E0001 | `lex_tab_indent` | 2:1 | tab in indentation; indent with two spaces |
| E0002 | `lex_odd_indent` | 2:1 | indentation must be a multiple of two spaces |
| E0003 | `lex_unterminated_string` | 2:11 | unterminated string literal |
| E0004 | `lex_unknown_escape` | 2:12 | unknown escape sequence `\q` |
| E0004 | `lex_escape_empty_unicode` | 2:12 | malformed escape `\u{}`; write 1 to 6 hex digits between the braces |
| E0004 | `lex_escape_unicode_too_long` | 2:12 | malformed escape `\u{0000041}`; write 1 to 6 hex digits between the braces |
| E0005 | `lex_escape_not_scalar` | 2:14 | `\u{110000}` is not a Unicode scalar value |
| E0005 | `lex_escape_surrogate` | 2:12 | `\u{D800}` is not a Unicode scalar value |
| E0006 | `lex_string_in_interpolation` | 2:13 | string literal inside interpolation; bind it with `let` first |
| E0007 | `lex_interpolation_too_deep` | 3:42 | interpolation nests braces deeper than 32 |
| E0008 | `lex_unexpected_char` | 2:13 | unexpected character `@` |
| E0008 | `lex_non_ascii_name` | 2:9 | unexpected character `ü` |
| E0008 | `lex_column_counts_scalars` | 2:21 | unexpected character `@` |
| E0009 | `lex_semicolon` | 2:12 | `;` is not used in Ostrel; write one statement per line |
| E0010 | `lex_int_literal_too_large` | 2:11 | integer literal is outside the `Int` range of -9007199254740991 to 9007199254740991 |
| E0020 | `parse_compound_assign` | 3:5 | Ostrel assigns with `=`; compound assignment does not exist |
| E0020 | `parse_walrus_assign` | 2:5 | Ostrel assigns with `=`; `:=` does not exist, declare a new name with `let` |
| E0021 | `parse_is_operator` | 3:8 | `is` is not Ostrel; to unwrap an optional write `if let y = x` |
| E0022 | `parse_brace_block` | 1:11 | blocks use indentation, not braces; remove `{` |
| E0023 | `parse_colon_block_head` | 3:11 | block heads have no `:`; remove it and indent the block |
| E0024 | `parse_else_if_one_line` | 5:8 | `else` ends its line; put the `if` on the next line and indent it |
| E0025 | `parse_unclosed_paren` | 2:8 | unclosed `(` |
| E0026 | `parse_keyword_as_name` | 2:7 | `if` is a keyword and cannot be used as a name |
| E0027 | `parse_comparison_chain` | 5:15 | comparisons do not chain; write `a < b and b < c` |
| E0100 | `check_float_type_v0_1` | 1:14 | type `Float` is not available in v0.1 |
| E0200 | `name_unknown` | 3:9 | unknown name `y` |
| E0201 | `name_duplicate_fn` | 4:4 | function `f` is already declared |
| E0202 | `name_duplicate_param` | 1:16 | parameter `a` is already declared |
| E0203 | `name_unknown_type` | 1:13 | unknown type `Integer`; v0.1 has `Int`, `Text` and `Bool` |
| E0300 | `type_text_plus` | 2:13 | `+` does not join text; join text with interpolation |
| E0301 | `type_condition_not_bool` | 2:6 | condition must be `Bool`, found `Int` |
| E0302 | `type_arity` | 5:9 | `inc` takes 1 argument, found 2 |
| E0303 | `type_implicit_return_in_branch` | 5:5 | add `return`; a function body with branches returns only through `return` |
| E0400 | `entry_missing_main` | 1:1 | missing `fn main()`; `ostrel run` starts a script program there |
| E0401 | `entry_main_params` | 1:9 | `fn main` takes no parameters |
| E0402 | `entry_main_result` | 1:11 | `fn main` returns nothing; remove the result type |

Code ranges: E0001 to E0019 lexer, E0020 to E0099 parser, E0100 construct outside the v0.1
slice, E0200 to E0299 names, E0300 to E0399 types, E0400 to E0499 program entry.

Codes enter this catalog with their first golden and are never renumbered or reused once they
are on `dev` (SPEC 12.1).

## Interpolation depth

The `{` that opens an interpolation is depth 1; each further unescaped `{` adds 1 and each `}`
subtracts 1. Depth 32 is accepted; the brace that would reach depth 33 is E0007 at that brace
(SPEC 12.1). `lex_interpolation_too_deep` opens 33 braces, so the error is at the 33rd, 3:42.

## Assumptions

* `check_float_type_v0_1`: `Float` is a type of the language (SPEC 5.0 AC-20) but outside the
  v0.1 slice, so the checker reports it with E0100, while a name that is no type at all, such as
  `Integer`, is E0203. The parser reads any type name, so this case needs no construct beyond
  T1-3.
* `type_implicit_return_in_branch`: the diagnostic is at the start of the expression that a
  reader would take as the implicit result of the branch (SYNTAX 4.10 and 9).

## Sources

* Lexical rules, interpolation and the brace limit of 32: SYNTAX 3, ARCHITECTURE 3.2 G1.
* `Int` literal range: ARCHITECTURE 3.2 G9.
* False friends (`+=`, `:=`, `is`, text joined with `+`, implicit return in a branch): SYNTAX 9.
* Diagnostic format, columns, interpolation depth, code catalog: SPEC 12.1.
* Identifiers are ASCII only, `else if` on one line is a parser diagnostic: SPEC 12.2.
* Escapes, including the malformed `\u{}` forms: SPEC 12.4.
* Block structure without braces or `:` heads, one statement per line: SYNTAX 2 P1 and P2.
* Non chaining comparison: SYNTAX 7 (`cmpExpr`).
* `E0100` and the v0.1 slice: SYNTAX 4.11, ARCHITECTURE 3.4.
* `fn main()` rules: SYNTAX 4.11, ARCHITECTURE 3.2 G15, AC-52.

## Not covered here

`for x of xs` (SYNTAX 9, parser suggests `in`) and list literals rejected with E0100 need parser
support beyond T1-3. Their goldens are added once the parser reads `for` and `[...]` (v0.2).

Runtime errors (`IntOverflow`, `DivisionByZero`, `CallDepth`, `StepLimit`) exit with 1 from
`ostrel run` and have their own goldens (T2-4, AC-52). Parser depth and size limits belong to
the hostile corpus in `tests/hostile/` (AC-04).
