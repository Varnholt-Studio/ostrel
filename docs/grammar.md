# Ostrel grammar (v0.1)

This document describes the syntax of the Ostrel programming language as accepted by the v0.1
compiler: the lexical structure, the grammar of the v0.1 subset in EBNF, operator precedence and
a set of checked examples. The grammar of later milestones is listed in the appendix for
orientation; constructs from it are parsed as the parser grows but are rejected by the v0.1
checker with `E0100 not available in v0.1`.

## How the examples are checked

Fenced code blocks in `docs/` are part of the test suite:

| Tag | Meaning |
|---|---|
| `ostl` | The block is a complete script program with `fn main()` and must exit with code 0. |
| `ostl-error` | The block must exit with code 1 and at least one diagnostic. |
| `ebnf` | Grammar notation, not checked. |
| `text` | Plain listing (keywords, tokens), not checked. |

Each block is checked on its own, as if it were a single `.ostl` file (SPEC 12.2). An `ostl` block
without `fn main()` is reported with `E0400`. For an `ostl-error` block, exit code 0, 2, 101 or a
signal is a failure.

## 1. Notation

The grammar uses ISO style EBNF:

| Form | Meaning |
|---|---|
| `a = b ;` | rule definition |
| `"x"` | the literal token `x` (a keyword, operator or punctuation) |
| `a b` | sequence |
| `a \| b` | alternative |
| `[ a ]` | optional |
| `{ a }` | zero or more repetitions |
| `( a )` | grouping |
| `(* text *)` | comment |

`INDENT`, `DEDENT` and `NL` are produced by the lexer (section 2.2). `Name` is an identifier
written in UpperCamel case, `name` one in lowerCamel case; the parser treats both as identifier
tokens, the case convention is enforced by the formatter and a lint, not by the parser (SYNTAX 3).
In the v0.1 subset the distinction never changes how a line is parsed. For the later grammar see
the open point at the end of the appendix.

## 2. Lexical structure

### 2.1 Source text

Source files are UTF 8 and use the extension `.ostl`. Lines are separated by LF (SPEC 12.1).
Diagnostic columns count Unicode scalar values, and a tab counts as one column.

Bidirectional control characters are not allowed anywhere in the source text (D53, ARCHITECTURE
3.1). The nine characters U+202A to U+202E and U+2066 to U+2069 are rejected wherever they appear
raw: in code, comments, doc comments, string literals and `style` bodies. Each occurrence is
reported with the lexer diagnostic for bidirectional control characters, whose code is registered
in `tests/errors/README.md` (D53). The span is the character itself, and the lexer continues
after it, so every occurrence is reported. The same characters written as an escape in a string
(for example `\u{202E}`) stay valid, because the escape is visible. Other invisible characters
(U+200B to U+200F, U+061C and U+2060) stay allowed in strings and comments; they cannot reverse
the reading order, and joiners are needed for emoji. The reason is Trojan Source
(CVE-2021-42574): a reviewer must read the same code the compiler runs.

### 2.2 Lines and indentation

Ostrel is line based. A statement ends at the end of its line, and nesting is expressed by
indentation of exactly two spaces per level.

* The lexer emits `NL` at the end of every logical line, `INDENT` when a line is indented one
  level deeper than the previous one, and one `DEDENT` per level when indentation decreases.
* Blank lines and lines that contain only a comment do not produce `NL`, `INDENT` or `DEDENT`,
  and their leading whitespace is not checked (SPEC 12.2).
* A tab character in indentation is a lexical error.
* A line continues onto the next physical line while a `(`, `[` or `{` is open (SYNTAX 3). The
  v0.1 subset has no list, set or map literals, so in v0.1 only an open `(` continues a line;
  `[` and `{` continue lines from v0.2 on (SPEC 12.2). Inside an open bracket, line breaks and
  indentation are ignored.

Not decided yet (QUESTION #136 in `#arch`): indentation by an odd number of spaces, an indent of
more than one level at once, a dedent to a column that was never an indentation level, a CR
before the LF, a byte order mark at the start of the file, and the tokens emitted at the end of
the file. No example in this document depends on these cases.

### 2.3 Comments

A comment starts with `//` and runs to the end of the line. Comments are kept as trivia for the
formatter and have no effect on the program. A comment may contain any character except the
bidirectional control characters of section 2.1, which are rejected inside comments as well
(D53). There are no block comments in Ostrel code (`/* */`
exists only inside `style` bodies, which are not part of v0.1).

### 2.4 Identifiers and keywords

An identifier starts with an ASCII letter, followed by ASCII letters, digits and underscores
(SPEC 12.2, binding for v0.1 to v0.3). Any other character where a token is expected is reported
with `E0008`, except a bidirectional control character, which gets only the D53 diagnostic of
section 2.1 (one diagnostic per error, SPEC 12.1). Values, parameters and functions are
written in `lowerCamel`, types in `UpperCamel`.

The following 45 words are keywords. All of them are reserved from v0.1 on, even though most of
them are only meaningful in later milestones:

```text
app auth home data enum check merge fixed server serial per var let fn call as system return
if else for in where sort desc limit last see make edit drop view style extern on and or not
me now none signed true false catch
```

The keywords used by the v0.1 subset are `fn`, `let`, `if`, `else`, `return`, `and`, `or`,
`not`, `true` and `false`. Using any keyword as an identifier is an error. Words such as `is`,
`set`, `from` or `use` are not keywords in Ostrel and are ordinary identifiers.

### 2.5 Integer literals

```ebnf
Number = digit { digit } ;
digit  = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
```

An integer literal is a sequence of decimal digits. `Int` is a signed integer in the range
plus and minus 9007199254740991 (2^53 minus 1). A literal outside this range is a compile error.
A negative number is written with the unary `-` operator.

### 2.6 String literals and interpolation

```ebnf
String        = '"' { char | escape | "{" interpolation "}" } '"' ;
char          = (* any Unicode scalar value except '"', "\", "{", "}", a line end and the
                   bidirectional control characters U+202A to U+202E and U+2066 to U+2069 *) ;
escape        = "\{" | "\}" | '\"' | "\\" | "\n" | "\t"
              | "\u{" hex [ hex ] [ hex ] [ hex ] [ hex ] [ hex ] "}" ;
hex           = digit | "a" | "b" | "c" | "d" | "e" | "f" | "A" | "B" | "C" | "D" | "E" | "F" ;
interpolation = (* the tokens of one expr, containing no string literal *) ;
```

* A string literal is enclosed in double quotes and does not span lines: a line end before the
  closing quote is an unterminated string (`E0003`), reported at the opening quote (SPEC 12.1).
* The escapes are exactly the ones listed above. `\u{h}` takes 1 to 6 hex digits in either
  case; a value that is not a Unicode scalar value (a surrogate or above 10FFFF) is `E0005`.
  Every other escape, including `\r`, `\0`, an empty `\u{}`, more than 6 digits or a missing
  `}`, is `E0004` (SPEC 12.4).
* A raw bidirectional control character (U+202A to U+202E, U+2066 to U+2069) inside a string
  literal is rejected with the D53 lexer diagnostic of section 2.1. Written as an escape, for
  example `\u{202E}`, it is valid. U+200B to U+200F, U+061C and U+2060 are allowed raw.
* `{expr}` inserts the text form of the expression. `Int` values are written in decimal, `Bool`
  values as `true` or `false`.
* A string literal inside an interpolation is a lexical error. Bind the inner text with `let`
  first and interpolate the name.
* Inside an interpolation the lexer counts braces up to a nesting depth of 32. The opening
  brace is depth 1; the brace that would reach depth 33 is `E0007`. Escaped braces do not count
  (SPEC 12.1).
* To write a literal brace, use `\{` or `\}`. The sequence `{{` has no special meaning.
* Text values are joined by interpolation, not by `+`.
* Not decided yet (QUESTION #136): an unescaped `}` outside an interpolation. The grammar above
  does not produce it; write `\}`.

### 2.7 Operators and punctuation

The v0.1 subset uses these tokens:

```text
+  -  *  /  %  ==  !=  <  <=  >  >=  (  )  ,  :  ->  =
```

## 3. Grammar of the v0.1 subset

A v0.1 program is a script program: a sequence of function declarations, one of which is
`fn main()`.

```ebnf
program    = { fnDecl } ;
fnDecl     = "fn" name params [ "->" type ] NL INDENT { stmt } DEDENT ;
params     = "(" [ param { "," param } ] ")" ;
param      = name ":" type ;
type       = Name ;                                 (* Int, Text or Bool *)
block      = NL INDENT { stmt } DEDENT ;
stmt       = "let" name "=" expr NL
           | "if" expr block [ "else" block ]
           | "return" expr NL
           | expr NL ;
expr       = orExpr ;
orExpr     = andExpr { "or" andExpr } ;
andExpr    = notExpr { "and" notExpr } ;
notExpr    = "not" notExpr | cmpExpr ;
cmpExpr    = addExpr [ cmpOp addExpr ] ;
cmpOp      = "==" | "!=" | "<" | "<=" | ">" | ">=" ;
addExpr    = mulExpr { ( "+" | "-" ) mulExpr } ;
mulExpr    = unary { ( "*" | "/" | "%" ) unary } ;
unary      = [ "-" ] postfix ;
postfix    = primary { "(" [ expr { "," expr } ] ")" } ;
primary    = Number | String | "true" | "false" | name | "(" expr ")" ;
```

Notes:

* A function body and every `if` or `else` block contain at least one statement, because the
  lexer emits `INDENT` only for an indented line.
* There is no `else if` on one line. Write the second `if` inside the `else` block.
* A comparison takes exactly two operands: `a < b < c` is a syntax error.
* `not` binds more loosely than comparisons, so `not a == b` means `not (a == b)`.
* Unary `-` applies once; write `-(-x)` for a double negation.
* `let` bindings are immutable. Assignment (`x = e`) belongs to later milestones.
* In the full grammar the expression levels between `cmpExpr` and `addExpr` (`??`, `catch`,
  `..`) and the postfix forms `.name` and `[expr]` exist as well (see the appendix). In v0.1
  they are rejected by the checker with `E0100`.

Parser limits (D43, D54, ARCHITECTURE 3.4). Each limit has its own parser diagnostic, whose code
is registered in `tests/errors/README.md`; the values are also listed in `tests/hostile/README`:

* Nesting depth 256: brackets and blocks as written in the source.
* AST height 2 048: the longest path from the root of the syntax tree to a leaf. Operator chains
  are parsed iteratively, but `1 + 1 + 1 ...` still builds a left leaning tree whose height grows
  with the number of operands, so a chain of about 2 100 operands exceeds the limit.
* AST nodes 1 000 000 per file: every node the parser creates counts, tokens and trivia do not.
  On overflow there is one diagnostic at the triggering token, the rest of the file is abandoned
  without follow up diagnostics, and the exit code is 1.

None of the limits can be changed from the command line in v0.1.

## 4. Operator precedence

From lowest to highest binding:

| Level | Operators | Associativity | Operand types in v0.1 |
|---|---|---|---|
| 1 | `or` | left | `Bool` |
| 2 | `and` | left | `Bool` |
| 3 | `not` | prefix | `Bool` |
| 4 | `== !=` | none | `Int`, `Text`, `Bool` (both sides the same type) |
| 4 | `< <= > >=` | none | `Int` |
| 5 | `+ -` | left | `Int` |
| 6 | `* / %` | left | `Int` |
| 7 | unary `-` | prefix | `Int` |
| 8 | call `f(...)` | left | |

Integer `/` truncates toward zero,
and `%` takes the sign of the dividend. All `Int` arithmetic is checked: a result outside the
`Int` range raises the runtime error `IntOverflow`, and division or remainder by zero raises
`DivisionByZero`.

## 5. Functions, entry point and output

* A function declares typed parameters and an optional result type after `->`.
* A function with a result type returns its value with `return`. If the body contains no `if`,
  the last expression of the body is the result and `return` may be omitted. If the body
  branches, every path needs an explicit `return`.
* `ostrel run file.ostl` calls `fn main()`, which takes no parameters and returns nothing. A
  missing `main` or one with parameters or a result type is a compile error.
* `print(x)` accepts an `Int`, `Text` or `Bool` and writes its text form followed by a newline
  to standard output.
* Runtime errors are `IntOverflow`, `DivisionByZero`, `CallDepth` (the call that would create
  frame 10 001; `main` is frame 1), `StepLimit` (more than 100 000 000 steps, adjustable with
  `--max-steps`), `TextLimit` (a `Text` longer than 16 777 216 UTF 8 bytes) and `HeapLimit`
  (a VM heap above 256 MiB) (SPEC 12.4). Each one prints
  `file:line:column: runtime error[Kind]: message` to standard error and ends the program with
  exit code 1. Output already printed is kept.
* Exit codes of the command line tool: 0 for success, 1 for any diagnostic or runtime error,
  2 for usage errors.

## 6. Examples

### 6.1 Recursion

```ostl
fn fib(n: Int) -> Int
  if n < 2
    return n
  return fib(n - 1) + fib(n - 2)

fn main()
  let x = fib(20)
  print("fib(20) = {x}")
```

### 6.2 Integer arithmetic

```ostl
fn main()
  let a = 7
  let b = -2
  print(a / b) // -3, division truncates toward zero
  print(a % b) // 1, the remainder has the sign of the dividend
  print(-a + 3) // -4, unary minus binds tighter than +
  print(2 * -3) // -6, a unary minus may follow a binary operator
  print(1 + 2 * 3) // 7
  print((1 + 2) * 3) // 9
```

### 6.3 Booleans, text and implicit results

```ostl
// The body has no branches, so its last expression is the result.
fn isSmall(n: Int) -> Bool
  n < 10

fn parity(n: Int) -> Text
  if n % 2 == 0
    return "even"
  return "odd"

fn main()
  let n = 12
  let small = isSmall(n)
  print("{n} is {parity(n)}")
  print(small)
  print(not small and n != 0)
  print(parity(n) == "even" or small)
```

### 6.4 Nested conditions

```ostl
fn sign(n: Int) -> Int
  if n < 0
    return -1
  else
    if n == 0
      return 0
  return 1

fn main()
  print(sign(-5))
  print(sign(0))
  print(sign(9))
```

### 6.5 Escapes and line continuation

```ostl
fn add(a: Int, b: Int) -> Int
  a + b

fn main()
  let total = add(
    40,
    2
  )
  print("total: {total}")
  print("a brace \{ and a quote \" inside text")
  print("tab:\tend")
```

### 6.6 Errors

Each of the following blocks contains exactly one error and must be rejected by `ostrel check`.

A string literal inside an interpolation:

```ostl-error
fn main()
  print("hello {"world"}")
```

Text is joined by interpolation, not by `+`:

```ostl-error
fn main()
  let greeting = "hello" + "world"
  print(greeting)
```

Compound assignment does not exist:

```ostl-error
fn main()
  let x = 1
  x += 1
  print(x)
```

A comparison takes exactly two operands:

```ostl-error
fn main()
  print(1 < 2 < 3)
```

An integer literal outside the `Int` range:

```ostl-error
fn main()
  print(9007199254740992)
```

A tab in indentation:

```ostl-error
fn main()
	print(1)
```

`is` is not an operator; it is an ordinary identifier, so this line does not parse:

```ostl-error
fn main()
  let x = 1
  if x is 1
    print(x)
```

A function that branches needs an explicit `return` on every path:

```ostl-error
fn clamp(n: Int) -> Int
  if n < 0
    return 0
  n

fn main()
  print(clamp(5))
```

A construct from a later milestone (here a `for` loop over a list) is rejected with `E0100`.
This block has to be replaced when `for` and list literals become available (AC-13):

```ostl-error
fn main()
  for x in [1, 2, 3]
    print(x)
```

## Appendix: core grammar of later milestones (informative)

The grammar below is the core grammar planned for v0.3. It is listed so that readers can see
where the v0.1 subset is heading. Everything outside section 3 is rejected by the v0.1 checker
with `E0100`. Quoted words that are not keywords (`js`, `client`, `new`, `added`, `removed`,
`it`) are contextual and match an identifier token with that text. The grammar is LL(1) except
where marked LL(2).

```ebnf
program    = { decl } ;
decl       = appDecl | enumDecl | dataDecl | varDecl | fnDecl | viewDecl | styleDecl | externDecl ;
appDecl    = "app" Name NL [ INDENT { appItem NL } DEDENT ] ;
appItem    = "auth" name [ name ] | "home" Name ;
enumDecl   = "enum" Name "=" name { "|" name } NL ;
dataDecl   = "data" Name NL INDENT { member NL } DEDENT ;
member     = field | rule | "check" expr ;
field      = name ":" type [ "=" expr ] [ mode ] [ "merge" name ] ;
mode       = "fixed" | "server" | "serial" [ "per" name ] ;
rule       = verb [ name ] "if" expr ;
verb       = "see" | "make" | "edit" | "drop" ;
type       = Name [ "[" type { "," type } "]" ] [ "?" ] ;
varDecl    = "var" name ( ":" type [ "=" expr ] | "=" expr ) NL ;
fnDecl     = [ "server" | "client" ] "fn" name params [ "->" type ] [ "as" "system" ] NL
             INDENT [ "call" "if" expr NL ] { stmt } DEDENT ;
params     = "(" [ name ":" type { "," name ":" type } ] ")" ;
block      = NL INDENT { stmt } DEDENT ;
stmt       = "let" name "=" expr NL
           | "if" cond block [ "else" block ]
           | "for" name "in" source block
           | "drop" source NL
           | "return" expr NL
           | expr [ "=" expr ] NL ;                 (* target of "=" checked in sema *)
cond       = "let" name "=" expr | expr ;
source     = query | expr ;                         (* query iff first token is Name *)
query      = Name [ "where" expr ] [ "sort" name [ "desc" ] ] [ ( "limit" | "last" ) expr ] ;
expr       = orExpr ;
orExpr     = andExpr { "or" andExpr } ;
andExpr    = notExpr { "and" notExpr } ;
notExpr    = "not" notExpr | cmpExpr ;
cmpExpr    = fbExpr [ cmpOp fbExpr ] ;
cmpOp      = "==" | "!=" | "<" | "<=" | ">" | ">=" | "in" | "not" "in" ;
fbExpr     = rangeExpr [ "catch" Name { "," Name } ] { "??" rangeExpr } ;
rangeExpr  = addExpr [ ".." addExpr ] ;
addExpr    = mulExpr { ( "+" | "-" ) mulExpr } ;
mulExpr    = unary { ( "*" | "/" | "%" ) unary } ;
unary      = [ "-" ] postfix ;
postfix    = primary { "." name | "(" [ expr { "," expr } ] ")" | "[" expr "]" } ;
primary    = Number | String | "true" | "false" | "none" | "me" | "now" | "signed" | name
           | "(" expr ")" | "{" [ expr { "," expr } ] "}" | "[" [ listOrMap ] "]"
           | "make" Name "{" [ name ":" expr { "," name ":" expr } ] "}" ;
listOrMap  = ":" | expr ( { "," expr } | ":" expr { "," expr ":" expr } ) ;
viewDecl   = "view" Name [ params ] NL INDENT { node } DEDENT ;
node       = element | "if" cond NL nodes [ "else" NL nodes ] | "for" name "in" source NL nodes ;
nodes      = INDENT { node } DEDENT ;
element    = ( name | Name ) [ value ] { attr } [ handler ] NL [ INDENT { node | handler NL } DEDENT ] ;
value      = expr ;                                 (* not taken if next is name ":" , LL(2) *)
attr       = "~" name [ "(" expr ")" ] | name ":" expr ;
handler    = "on" name ( expr [ "=" expr ] ) ;
styleDecl  = "style" NL INDENT CssBody DEDENT ;     (* CSS mode lexing *)
externDecl = "extern" "js" ( "client" | "server" ) String "as" name NL INDENT { fnSig NL } DEDENT ;
fnSig      = "fn" name params [ "->" type ] ;
```

In the full grammar, `{` starts a set literal everywhere except after `make Name`, where it starts
a record value. Lists and maps use square brackets, and `[:]` is the empty map. Type arguments are
bracketed, as in `Set[User]`. Inside a `(`, `[` or `{` a line continues onto the next physical
line.

Open point (QUESTION #136): the rule `source` chooses `query` when the first token is a `Name`,
but section 1 and SYNTAX 3 say that the parser does not tell `Name` from `name`. Until this is
decided, the appendix does not define how a parser makes that choice.
