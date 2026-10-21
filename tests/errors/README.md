# Error goldens (AC-03)

Each case is a pair:

* `NAME.ostl`: a v0.1 program (SYNTAX 4.11) with exactly one fault. The cases of the v0.2
  front end live in `v0_2/` (see below).
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
| E0002 | `lex_odd_indent_too_deep` | 2:1 | indentation must be a multiple of two spaces |
| E0003 | `lex_unterminated_string` | 2:11 | unterminated string literal |
| E0004 | `lex_unknown_escape` | 2:12 | unknown escape sequence `\q` |
| E0004 | `lex_escape_zero_width` | 2:12 | unknown escape sequence `\U+200B` |
| E0004 | `lex_escape_empty_unicode` | 2:12 | malformed escape `\u{}`; write 1 to 6 hex digits between the braces |
| E0004 | `lex_escape_unicode_too_long` | 2:12 | malformed escape `\u{0000041}`; write 1 to 6 hex digits between the braces |
| E0005 | `lex_escape_not_scalar` | 2:14 | `\u{110000}` is not a Unicode scalar value |
| E0005 | `lex_escape_surrogate` | 2:12 | `\u{D800}` is not a Unicode scalar value |
| E0006 | `lex_string_in_interpolation` | 2:13 | string literal inside interpolation; bind it with `let` first |
| E0007 | `lex_interpolation_too_deep` | 3:42 | interpolation nests braces deeper than 32 |
| E0008 | `lex_unexpected_char` | 2:13 | unexpected character `@` |
| E0008 | `lex_non_ascii_name` | 2:9 | unexpected character U+00FC |
| E0008 | `lex_confusable_letter` | 2:7 | unexpected character U+0430 |
| E0008 | `lex_zero_width_in_code` | 2:8 | unexpected character U+200B |
| E0008 | `lex_column_counts_scalars` | 2:21 | unexpected character `@` |
| E0008 | `lex_bom_at_start` | 1:1 | unexpected character `@` |
| E0008 | `lex_bom_in_code` | 3:12 | unexpected character U+FEFF |
| E0009 | `lex_semicolon` | 2:12 | `;` is not used in Ostrel; write one statement per line |
| E0010 | `lex_int_literal_too_large` | 2:11 | integer literal is outside the `Int` range of -9007199254740991 to 9007199254740991 |
| E0011 | `lex_bidi_in_comment` | 2:16 | bidirectional control character U+202E is not allowed in source; write `\u{202E}` inside a string if it is needed |
| E0011 | `lex_bidi_in_string` | 2:12 | bidirectional control character U+2066 is not allowed in source; write `\u{2066}` inside a string if it is needed |
| E0012 | `lex_lone_close_brace` | 2:10 | `}` in a string literal must be written as `\}` |
| E0013 | `lex_indent_jump` | 2:1 | indentation is more than one level deeper than the line above; indent a block by two spaces |
| E0014 | `lex_invalid_utf8` | 2:18 | source is not valid UTF-8 (byte 0xFF) |
| E0014 | `lex_invalid_utf8_truncated` | 2:14 | source is not valid UTF-8 (byte 0xE2) |
| E0014 | `lex_bom_invalid_utf8` | 1:1 | source is not valid UTF-8 (byte 0xFF) |
| E0020 | `parse_compound_assign` | 3:5 | Ostrel assigns with `=`; compound assignment does not exist |
| E0020 | `parse_walrus_assign` | 2:5 | Ostrel assigns with `=`; `:=` does not exist, declare a new name with `let` |
| E0021 | `parse_is_operator` | 3:8 | `is` is not Ostrel; to unwrap an optional write `if let y = x` |
| E0022 | `parse_brace_block` | 1:11 | blocks use indentation, not braces; remove `{` |
| E0023 | `parse_colon_block_head` | 3:11 | block heads have no `:`; remove it and indent the block |
| E0024 | `parse_else_if_one_line` | 5:8 | `else` ends its line; put the `if` on the next line and indent it |
| E0025 | `parse_unclosed_paren` | 2:8 | unclosed `(` |
| E0026 | `parse_keyword_as_name` | 2:7 | `if` is a keyword and cannot be used as a name |
| E0027 | `parse_comparison_chain` | 5:15 | comparisons do not chain; write `a < b and b < c` |
| E0028 | `parse_bare_return` | 2:9 | expected a value after `return`, found end of line |
| E0100 | `check_float_type_v0_1` | 1:14 | type `Float` is not available in v0.1 |
| E0100 | `check_v0_2_enum_declaration` | 4:1 | `enum` declaration is not available in v0.1 |
| E0100 | `check_v0_2_server_fn` | 4:1 | `server fn` is not available in v0.1 |
| E0200 | `name_unknown` | 3:9 | unknown name `y` |
| E0201 | `name_duplicate_fn` | 4:4 | function `f` is already declared |
| E0201 | `name_duplicate_main` | 4:4 | function `main` is already declared |
| E0202 | `name_duplicate_param` | 1:16 | parameter `a` is already declared |
| E0203 | `name_unknown_type` | 1:13 | unknown type `Integer`; v0.1 has `Int`, `Text` and `Bool` |
| E0300 | `type_text_plus` | 2:13 | `+` does not join text; join text with interpolation |
| E0301 | `type_condition_not_bool` | 2:6 | condition must be `Bool`, found `Int` |
| E0302 | `type_arity` | 5:9 | `inc` takes 1 argument, found 2 |
| E0303 | `type_implicit_return_in_branch` | 5:5 | add `return`; a function body with branches returns only through `return` |
| E0304 | `type_mismatch_argument` | 5:13 | expected `Int`, found `Text` |
| E0304 | `type_mismatch_equality` | 3:11 | expected `Int`, found `Text` |
| E0304 | `type_return_value_without_result` | 3:3 | this function returns nothing, found `Int` |
| E0305 | `type_missing_return` | 1:4 | `sign` can end without returning `Int`; add `return` |
| E0400 | `entry_missing_main` | 1:1 | missing `fn main()`; `ostrel run` starts a script program there |
| E0401 | `entry_main_params` | 1:9 | `fn main` takes no parameters |
| E0402 | `entry_main_result` | 1:11 | `fn main` returns nothing; remove the result type |

Code ranges: E0001 to E0019 lexer, E0020 to E0099 parser, E0100 construct outside the v0.1
slice, E0200 to E0299 names, E0300 to E0399 types, E0400 to E0499 program entry.

Codes enter this catalog with their first golden and are never renumbered or reused once they
are on `dev` (SPEC 12.1).

## v0.2 goldens (`tests/errors/v0_2/`)

`ostrel check` sends an app (a program with an `app` declaration, SPEC 2) through name
resolution into the HIR (`ostrel_sema::hir::resolve`) and then the HIR checker; a script program
keeps the v0.1 checker above, so E0100 for `enum` or `server fn` in a script program is
unchanged. The goldens in `tests/errors/v0_2/` follow the contract above (exit code 1, stderr
byte for byte, line format, positions, one diagnostic per fault), with two differences:

* Every case is an app: its first line is `app Golden`, so it reaches the HIR front end. It has no
  `fn main()`, because an app has no entry function and E0400 does not apply.
* Cases use the v0.2 grammar (`data`, `enum`, `view`, queries, `make`, rules, `serial per`).

Codes and positions follow SPEC 12.6 (v1.12 to v1.16) and were written by hand from it. Message
texts are the owner's (SPEC 12.6) and are the texts of `crates/ostrel_sema/src/hir/resolve.rs`.

| Code | Case | Position | Message |
|---|---|---|---|
| E0200 | `name_query_head_unknown` | 4:12 | unknown data type `Nope` |
| E0200 | `name_rule_unknown` | 5:10 | unknown name `nope` |
| E0200 | `name_where_unknown` | 8:19 | unknown name `nope` |
| E0201 | `name_duplicate_top_level` | 6:4 | `f` is already declared |
| E0201 | `name_second_app` | 2:5 | a program has one `app`; this is the second |
| E0203 | `type_field_unknown` | 4:8 | unknown type `Integer` |
| E0203 | `type_make_function` | 7:8 | `g` is not a type |
| E0203 | `type_make_unknown` | 4:8 | unknown data type `Nope` |
| E0204 | `field_author_make_without_auth` | 8:15 | data type `Post` has no field `author`; every row has `author` only when the `app` has `auth paseto` |
| E0204 | `field_author_read_without_auth` | 8:5 | data type `Post` has no field `author`; every row has `author` only when the `app` has `auth paseto` |
| E0204 | `field_author_serial_per_without_auth` | 4:21 | data type `Post` has no field `author`; every row has `author` only when the `app` has `auth paseto` |
| E0204 | `field_author_where_without_auth` | 8:19 | data type `Post` has no field `author`; every row has `author` only when the `app` has `auth paseto` |
| E0204 | `field_unknown_access` | 8:5 | data type `Post` has no field `nope` |
| E0204 | `field_unknown_make` | 8:15 | data type `Post` has no field `nope` |
| E0204 | `field_unknown_serial_per` | 4:21 | data type `Post` has no field `nope` |
| E0204 | `field_unknown_sort` | 8:22 | data type `Post` has no field `nope` |
| E0205 | `model_make_builtin_type` | 4:8 | `make` needs a data type; `Int` is not one |
| E0205 | `model_make_enum` | 6:8 | `make` needs a data type; `Color` is not one |
| E0205 | `model_query_head_enum` | 6:12 | a query with `where`, `sort`, `limit` or `last` needs a data type; `Color` is not one |
| E0206 | `member_author_declared_with_auth` | 5:3 | field `author` is an implicit field of every row in `Post` |
| E0206 | `member_duplicate_app_line` | 3:3 | `home` is already declared in this `app` |
| E0206 | `member_duplicate_field` | 5:3 | field `pos` is already declared in `Post` |
| E0206 | `member_duplicate_make_field` | 8:23 | field `pos` is already given in this `make` |
| E0206 | `member_duplicate_variant` | 3:28 | variant `red` is already declared in `Color` |
| E0206 | `member_implicit_field_declared` | 4:3 | field `id` is an implicit field of every row in `Post` |
| E0207 | `query_local_names_field` | 8:19 | local `pos` has the name of a field of `Post`; rename the local |
| E0208 | `implicit_assign_made` | 8:5 | `made` is an implicit field; the system sets it |
| E0208 | `implicit_author_assign_with_auth` | 9:5 | `author` is an implicit field; the system sets it |
| E0208 | `implicit_edit_rule` | 5:8 | `made` is an implicit field; the system sets it |
| E0208 | `implicit_make_id` | 8:15 | `id` is an implicit field; the system sets it |
| E0309 | `type_args_missing` | 4:9 | `Set` takes 1 type argument, found 0 |
| E0309 | `type_args_on_scalar` | 4:6 | `Int` takes 0 type arguments, found 1 |
| E0309 | `type_args_wrong_count` | 4:6 | `Map` takes 2 type arguments, found 1 |

Notes on the v0.2 cases:

* `name_where_unknown`, `field_unknown_sort` and `field_author_where_without_auth` are the three
  cases SPEC 12.6 v1.16 asks for: an unbound bare name in `where` is E0200, a name after `sort`
  that is no field is E0204, and a bare `author` in `where` without `auth` is E0204 with the
  `auth paseto` hint.
* `field_author_*_without_auth` cover reading (also through `.handle`, one diagnostic), `where`,
  `make` (E0204, not E0208) and `serial per` (E0204, not E0307), SPEC 12.6 v1.14.
* `implicit_author_assign_with_auth` and `member_author_declared_with_auth` show that with `auth`
  the field exists: a write is E0208 and a declared field `author` is E0206.
* `name_query_head_unknown` (E0200) and `type_make_unknown` (E0203) are the same unknown name in
  the two positions SPEC 12.6 v1.13 keeps apart; `model_*` are E0205 for a declared name that is
  no model.
* E0309 goldens start with `type_args_`; `type_arity` above is the call arity code E0302
  (SPEC 12.6, v1.15).
* Not covered yet: the std `User` cases of D142 (E0203 with the `auth paseto` hint, E0201 for a
  declared `User` beside `auth`) and the E0200 of a bare word resolved by its expected type; they
  wait for the D142 work in `ostrel_sema`. E0306 and E0307 come from the HIR checker, which is
  still a stub.

## Interpolation depth

The `{` that opens an interpolation is depth 1; each further unescaped `{` adds 1 and each `}`
subtracts 1. Depth 32 is accepted; the brace that would reach depth 33 is E0007 at that brace
(SPEC 12.1). `lex_interpolation_too_deep` opens 33 braces, so the error is at the 33rd, 3:42.

## Layout, code points and bidi (D68)

* Indentation is a stack of columns. A line may open exactly one level, two columns deeper than
  the line above. Deeper is E0013 at column 1 of the line (`lex_indent_jump`); the following lines
  of that block give no further diagnostic. An odd column is E0002, also when it is too deep as
  well, and a fault never gives both (`lex_odd_indent` with 3 spaces, `lex_odd_indent_too_deep`
  with 5 spaces, SPEC 12.6).
* A raw bidi control character (U+202A to U+202E, U+2066 to U+2069) is E0011 anywhere, in comments
  and strings too (D53). The message names the code point with at least four upper case hex
  digits and never contains the raw character, so `lex_bidi_in_comment.ostl` and `lex_bidi_in_string.ostl` are the only files in
  this directory that carry such a character, and no `.expected_err` file ever does. The lexer
  continues after it, so the rest of the string or comment gives no second diagnostic.
* `}` in string text outside an interpolation is E0012 at the brace; it is written `\}`.
* A source file that is not valid UTF-8 is E0014 at the first invalid byte, with line and column
  counted over the valid prefix (D84, SPEC 12.6). The message names that byte with two upper case
  hex digits. Nothing after it is lexed, so a later invalid byte or an unexpected character gives
  no second diagnostic. `lex_invalid_utf8` puts byte 0xFF after `Grüße ` on line 2: column 18
  counts scalars, while counting bytes would give 20. Its comment holds a second invalid byte
  (0xC0) and an `@`, which must not be reported. Its `.expected_err` never carries the raw byte.
* A multi byte sequence that is cut short is reported at its lead byte, the first byte after the
  longest valid prefix (SPEC 12.6, answer to #969). `lex_invalid_utf8_truncated` has `E2 82`
  followed by `A` after `€ 5 ` on line 2, so the diagnostic names byte 0xE2 at 2:14 (counting
  bytes would give 2:16, and naming the continuation byte 0x82 or the `A` is wrong). Its third
  line holds byte 0xFF and an `@`, which must not be reported. These two files and
  `lex_bom_invalid_utf8` are the only ones in this directory that are not valid UTF-8; their
  `.expected_err` files are valid UTF-8.
* Exactly one U+FEFF at byte 0 is skipped and column 1 of line 1 starts after it (D68,
  ARCHITECTURE 3.5). `lex_bom_at_start` has a BOM followed by `@`, so the diagnostic is at 1:1
  (counting the BOM would give 1:2). `lex_bom_invalid_utf8` has a BOM followed by byte 0xFF: the
  E0014 position is counted over the valid prefix with the same rule, so it is 1:1 as well.
* U+FEFF anywhere else is E0008 in code and allowed in strings and comments (D68).
  `lex_bom_in_code` holds one U+FEFF inside a string and one inside a comment on line 2, which
  must not be reported, and one after `1` on line 3, reported at 3:12.
* A lexer message writes a character raw only if it is printable ASCII (SPEC 12.6). E0008 names
  every character outside U+0021 to U+007E by its code point, `U+` and at least four upper case
  hex digits, and every source text quoted by E0004 or E0005 has each character outside U+0020
  to U+007E replaced the same way in its place. So `lex_bom_in_code` expects `U+FEFF`,
  `lex_non_ascii_name` expects `U+00FC`, `lex_zero_width_in_code` (a U+200B after `x` in a
  `let`) expects `U+200B`, `lex_confusable_letter` (a Cyrillic `а` that reads as ASCII `a`)
  expects `U+0430`, and `lex_escape_zero_width` (a backslash followed by U+200B in a string)
  expects `\U+200B`. Every `.expected_err` whose source holds a byte outside U+0020 to U+007E
  and LF contains only printable ASCII and LF; the golden runner checks this.
* `lex_non_ascii_name` holds exactly one non ASCII character, so its single diagnostic does not
  depend on whether the lexer merges a run of unexpected characters.

## Assumptions

* `type_return_value_without_result`: the message text is the checker owner's (SPEC 12.6 leaves
  texts of E0304 and E0305 to the owner); the position, the `return` keyword, is binding.
* `type_missing_return`: the body ends with an `if` that has no `else`, so no trailing expression
  exists and E0303 does not apply (SPEC 12.6).
* `check_float_type_v0_1`: `Float` is a type of the language (SPEC 5.0 AC-20) but outside the
  v0.1 slice, so the checker reports it with E0100, while a name that is no type at all, such as
  `Integer`, is E0203. The parser reads any type name, so this case needs no construct beyond
  T1-3.
* A bare `return` without a value is E0028 at the token after `return`, in every function, with
  or without a result type (D90, #917, SPEC 12.6). It is never E0304. `parse_bare_return` has it
  in a function without a result type, where a reader could expect it to be allowed; the
  diagnostic is at the end of line, 2:9, and the checker does not run.
* `type_implicit_return_in_branch`: the diagnostic is at the start of the expression that a
  reader would take as the implicit result of the branch (SYNTAX 4.10 and 9).

## Sources

* Lexical rules, interpolation and the brace limit of 32: SYNTAX 3, ARCHITECTURE 3.2 G1.
* `Int` literal range: ARCHITECTURE 3.2 G9.
* False friends (`+=`, `:=`, `is`, text joined with `+`, implicit return in a branch): SYNTAX 9.
* Diagnostic format, columns, interpolation depth, code catalog: SPEC 12.1.
* E0011 to E0013, E0201 for a second `main`, E0304, E0305: SPEC 12.6, D53, D68, ARCHITECTURE 3.5.
* E0014 for a source file that is not valid UTF-8: SPEC 12.6, D84, ARCHITECTURE 3.4; lead byte
  of a truncated sequence: SPEC 12.6 (answer to #969).
* E0028 for a bare `return`: SPEC 12.6, D90 (#917).
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
the hostile corpus in `tests/hostile/` (AC-04); their three codes (nesting depth, AST height,
node count, D54) enter this catalog with their first golden once the parser that reports them
is on `dev`.
