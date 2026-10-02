# v0.1 examples

Script programs for the v0.1 language subset. They are the evidence for acceptance criterion AC-02: `ostrel run NN_topic.ostl` must exit with code 0 and its stdout must equal
`NN_topic.expected` byte for byte.

Rules for this directory:

* Every expected file is written by hand from the language definition, before or independently of
  the compiler. Expected files are never generated or updated from compiler output. A mismatch is a
  bug in the compiler or in the definition, and it is raised against the specification first.
* Every `.ostl` file has exactly one `.expected` file with the same stem, and vice versa.
* Expected files are UTF-8 and end with the newline written by the last `print`.
* Only programs that exit with code 0 live here. Runtime error cases (exit code 1 with stdout and
  stderr goldens, AC-52) need their own runner and are not part of this directory yet.

Deliberately not covered until the definition states the behaviour:

* short circuit evaluation of `and` and `or`;
* calls to a function declared later in the file (forward references, mutual recursion);
* interpolation of `Bool` values inside Text.

| File | Covers |
|---|---|
| `01_hello` | `fn main`, `print` of Text |
| `02_arithmetic` | precedence, associativity, parentheses, unary minus |
| `03_division_signs` | `/` truncates toward zero, `%` takes the sign of the dividend |
| `04_let_interpolation` | `let`, interpolation of names, expressions and Text |
| `05_functions` | implicit and explicit results, functions without result, nested calls |
| `06_if_else` | `if` with and without `else`, early `return` |
| `07_recursion` | `fib(20)` from SYNTAX 4.11, factorial up to 18 |
| `08_booleans` | Bool printing, `not`/`and`/`or` precedence, comparisons |
| `09_text_escapes` | Text equality, every escape sequence, empty Text |
| `10_int_limits` | Int boundary values without overflow |
| `11_gcd` | recursion over `%` |
| `12_collatz` | several branches returning early |
| `13_fizzbuzz` | nested `if`/`else`, recursive function without result |
