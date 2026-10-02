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
4. `LINE` and `COLUMN` are 1 based. `COLUMN` counts Unicode scalar values. All cases are ASCII
   up to the error position, so byte and scalar counts agree today.
5. The position is the start of the offending token or character named in the table below.
6. One fault gives one diagnostic. Follow up errors caused by the same fault (cascades) are a bug.
   The checker does not run on a file with lexer or parser errors.
7. Every case contains a valid `fn main()` unless the case is about `main`, so no case also
   triggers E0400.

## Diagnostic codes

| Code | Case | Position | Message |
|---|---|---|---|
| E0001 | `lex_tab_indent` | 2:1 | tab in indentation; indent with two spaces |
| E0002 | `lex_odd_indent` | 2:1 | indentation must be a multiple of two spaces |
| E0003 | `lex_unterminated_string` | 2:11 | unterminated string literal |
| E0004 | `lex_unknown_escape` | 2:12 | unknown escape sequence `\q` |
| E0005 | `lex_escape_not_scalar` | 2:14 | `\u{110000}` is not a Unicode scalar value |
| E0005 | `lex_escape_surrogate` | 2:12 | `\u{D800}` is not a Unicode scalar value |
| E0006 | `lex_string_in_interpolation` | 2:13 | string literal inside interpolation; bind it with `let` first |
| E0007 | `lex_interpolation_too_deep` | 3:42 | interpolation nests braces deeper than 32 |
| E0008 | `lex_unexpected_char` | 2:13 | unexpected character `@` |
| E0009 | `lex_semicolon` | 2:12 | `;` is not used in Ostrel; write one statement per line |
| E0010 | `lex_int_literal_too_large` | 2:11 | integer literal is outside the `Int` range of -9007199254740991 to 9007199254740991 |
| E0020 | `parse_compound_assign` | 3:5 | Ostrel assigns with `=`; compound assignment does not exist |
| E0020 | `parse_walrus_assign` | 2:5 | Ostrel assigns with `=`; compound assignment does not exist |
| E0021 | `parse_is_operator` | 3:8 | `is` is not Ostrel; to unwrap an optional write `if let y = x` |
| E0022 | `parse_brace_block` | 1:11 | blocks use indentation, not braces; remove `{` |
| E0023 | `parse_colon_block_head` | 3:11 | block heads have no `:`; remove it and indent the block |
| E0024 | `parse_for_of` | 3:9 | expected `in` after the loop variable; write `for x in xs` |
| E0025 | `parse_unclosed_paren` | 2:8 | unclosed `(` |
| E0026 | `parse_keyword_as_name` | 2:7 | `if` is a keyword and cannot be used as a name |
| E0027 | `parse_comparison_chain` | 5:15 | comparisons do not chain; write `a < b and b < c` |
| E0100 | `check_list_literal_v0_1` | 2:12 | list literal is not available in v0.1 |
| E0200 | `name_unknown` | 3:9 | unknown name `y` |
| E0201 | `name_duplicate_fn` | 4:4 | function `f` is already declared |
| E0202 | `name_duplicate_param` | 1:16 | parameter `a` is already declared |
| E0203 | `name_unknown_type` | 1:13 | unknown type `Integer`; v0.1 has `Int`, `Text` and `Bool` |
| E0300 | `type_text_plus` | 2:13 | `+` does not join text; join text with interpolation |
| E0301 | `type_condition_not_bool` | 2:6 | condition must be `Bool`, found `Int` |
| E0302 | `type_arity` | 5:9 | `inc` takes 1 argument, found 2 |
| E0400 | `entry_missing_main` | 1:1 | missing `fn main()`; `ostrel run` starts a script program there |
| E0401 | `entry_main_params` | 1:9 | `fn main` takes no parameters |
| E0402 | `entry_main_result` | 1:11 | `fn main` returns nothing; remove the result type |

Code ranges: E0001 to E0019 lexer, E0020 to E0099 parser, E0100 construct outside the v0.1
slice, E0200 to E0299 names, E0300 to E0399 types, E0400 to E0499 program entry.

## Sources

* Lexical rules, interpolation and the brace limit of 32: SYNTAX 3, ARCHITECTURE 3.2 G1.
* `Int` literal range: ARCHITECTURE 3.2 G9.
* False friends (`+=`, `:=`, `is`, `for x of`, text joined with `+`): SYNTAX 9.
* Block structure without braces or `:` heads, one statement per line: SYNTAX 2 P1 and P2.
* Non chaining comparison: SYNTAX 7 (`cmpExpr`).
* `E0100` and the v0.1 slice: SYNTAX 4.11, ARCHITECTURE 3.4.
* `fn main()` rules: SYNTAX 4.11, ARCHITECTURE 3.2 G15, AC-52.

## Not covered here

Runtime errors (`IntOverflow`, `DivisionByZero`, `CallDepth`, `StepLimit`) exit with 1 from
`ostrel run` and have their own goldens (T2-4, AC-52). Parser depth and size limits belong to
the hostile corpus in `tests/hostile/` (AC-04).
