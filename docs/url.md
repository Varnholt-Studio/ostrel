# URLs in views

Status: derived from ARCHITECTURE 7.5 (host rule H, D144, D146) and SPEC 12.13. Refs #193.
This document adds nothing to those sections: if they ever disagree with it, they win and this
file is a bug.

It answers two questions for developers and reviewers: which texts `Url.parse` accepts, and
which URLs it refuses on purpose although a browser would open them.

## 1. Where the rule applies

`Url.parse(t)` takes `Text` and has the result type `Url?`. It is the only way from `Text` to
`Url`, and a `Url` is the only value accepted for `src` of `img` and `href` of `link`. A `Url?`
or a `Text` in those places is the compile error E0304; unwrap with `??` or `if let`.

The rule is implemented twice in plain code, without a dependency:

| Side | Code |
|---|---|
| Browser client | `parseUrl` in `runtime/js/view/safe/url.js`, used by the attribute sinks |
| Server | the VM side of `Url.parse` in `ostrel_vm` |

Both sides run against one corpus, `tests/hostile/url/corpus.txt`. The contract is that both
sides give the result written there. It is not equality with the WHATWG URL Standard, and the
client does not call `new URL`.

## 2. The rule

A text is checked in this order. The first step that fails gives `none`.

1. It contains no control character: nothing from U+0000 to U+001F or U+007F to U+009F.
2. Leading and trailing spaces (U+0020 only) are removed. An empty result is `none`.
3. Without a scheme the text is a relative URL and is accepted (`/x`, `?q`, `#f`, `//host/p`).
   A scheme is `[A-Za-z][A-Za-z0-9+.-]*` at the start, directly followed by `:`, compared
   without case. A `:` after `/`, `?` or `#` belongs to a relative URL.
4. The scheme is `http`, `https` or `mailto`. Anything else (`javascript`, `data`, `vbscript`,
   `file`, `blob`, unknown schemes) is `none`.
5. `mailto` is not checked further. `http` and `https` must pass the host rule H below.

The result is the trimmed text. It is never normalised: what the developer wrote is what the
DOM gets.

### Host rule H

| Part | Requirement | Refused examples |
|---|---|---|
| H1 | the `:` of the scheme is directly followed by `//` | `http:a.b`, `http:/a.b`, `http:\\a.b` |
| H2 | the authority runs from `//` to the first `/`, `?`, `#` or the end | |
| H3 | the authority is a host, optionally followed by `:` and a port, and nothing else | `http://a@b.c`, `https://[::1]`, `http://a%2eb`, `http://a b.c` |
| H4 | the host is labels of `[A-Za-z0-9-]`, each at least one character, separated by one `.`, with at most one trailing `.` | `http://a..b`, `http://.a`, `http://a_b.c`, `http://` |
| H5 | if the last label (without the trailing `.`) is all digits or starts with `0x`/`0X`, the host must be a dotted IPv4 address: four decimal parts from 0 to 255, no leading zeros | `http://999.1.1.1`, `http://a.123`, `http://a.0x1`, `http://01.2.3.4` |
| H6 | a port is 1 to 5 digits with a value of at most 65535 | `http://a.b:`, `http://a.b:65536`, `http://a.b:8x` |
| H7 | path, query and fragment are not checked | |

Accepted examples: `http://localhost`, `https://a-b.c:443`, `http://1.2.3.4`,
`http://a.b.:8080/x`, `https://a.b/p\q@r%20[s]` (characters after the authority are not
checked, H7).

## 3. Deliberate limits

The rule is narrower than what browsers accept. Each limit below is a choice, not a defect, and
is covered by a corpus case where the rule decides it.

* **International host names only in their `xn--` form.** `http://bücher.de` is `none` (H3, non
  ASCII character); write `http://xn--bcher-kva.de`. Converting to Punycode needs IDNA tables,
  which neither side carries.
* **An `xn--` label is not checked for valid Punycode.** A label such as `xn--zz` is accepted and
  gives a dead link in the browser. This affects only whether the link works, not safety.
* **No IPv6 literals.** `https://[::1]` is `none` (H3). Use a host name.
* **No user info.** `http://user@a.b` is `none` (H3). Credentials do not belong in a rendered
  link.
* **No empty port.** `http://a.b:` is `none` (H6), although browsers accept it.
* **No other IPv4 forms.** Browsers read `http://0x7f.0.0.1`, `http://1.2.3` or
  `http://01.2.3.4` as addresses; here they are `none` (H5).
* **One trailing dot is allowed (D146).** `http://a.b.` and `http://1.2.3.4.` are a `Url` with
  the text unchanged; H5 judges the host without that one dot. Two trailing dots
  (`http://1.2.3.4..`), `http://1.2.3.`, `http://1.2.3.4.5.`, `http://01.2.3.4.` and
  `http://1.2.3.256.` are `none`.
* **Ports keep their text.** `http://a.b:00080` is a `Url` and the text stays `:00080`; there is
  no normalisation.

## 4. Why the rule is safe and what it does not promise

Safety rests on steps 1 and 4: no control character (so `java\tscript:` cannot reach the DOM)
and a scheme allowlist. The host rule only decides whether a link is usable. It fails closed:
when in doubt, the text is `none` and the view renders it as inert text instead of a link.

The rule accepts almost only texts that browsers also parse, but it does not promise that an
accepted URL points anywhere. A view test that wants to know how a browser follows a written
attribute (as `runtime/js/view/test/hostile_strings.test.mjs` does) uses the browser's own
parser as an independent oracle; that oracle is not the rule.

## 5. Changing the rule

The rule belongs to ARCHITECTURE 7.5. A change needs an architect DECISION first, then the
corpus, both implementations and this document follow in that order. Add a corpus case only
when ARCHITECTURE 7.5 decides its result (`tests/hostile/url/README.md`).
