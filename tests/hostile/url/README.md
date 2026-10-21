# URL corpus

`corpus.txt` is the one shared test corpus of the URL rule of `Url.parse` (ARCHITECTURE 7.5,
SPEC 12.13, AC-47). The client runs it in `runtime/js/view/safe/safe.test.mjs` against
`parseUrl`; the VM side of `Url.parse` runs the same file. Both sides must give the result
written here, which is not necessarily the result of the WHATWG URL Standard.

One case per line:

* `ok`, TAB, input, TAB, expected text: the input is a `Url` with the expected text.
* `none`, TAB, input: the input is refused.

Escapes are `\t`, `\\` and `\u{H}` (a Unicode scalar value in hex), nothing else. Raw
control characters never appear in the file; the empty input is written as nothing after
the TAB.

The file starts with the cases SPEC 12.13 requires, followed by the host rule cases of D144
and further cases for each part H1 to H7. Add a case only when the rule in ARCHITECTURE 7.5
decides its result.
