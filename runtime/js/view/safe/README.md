# runtime/js/view/safe

Safe rendering sinks of the Ostrel programming language runtime (work package T3-1b, ARCHITECTURE 7.5, AC-47). The view core (`../core.js`) writes attributes only through the object exported here.

| File | Content |
|---|---|
| `sinks.js` | `safeSinks` (`{ setAttr(element, name, value), removeAttr(element, name) }`), `SinkError` |
| `attrs.js` | Attribute allowlist: global names, names per tag, the value rule of each |
| `url.js` | `parseUrl(text)`: the URL rule of `Url.parse` with the host rule H (ARCHITECTURE 7.5, SPEC 12.13, SYNTAX 4.8) |
| `safe.test.mjs` | Unit tests, run by `ci/checks/50_tests.sh` with `node --test`; they also run every case of the shared URL corpus `tests/hostile/url/corpus.txt` |
| `raw_html.js` | Not present yet. The only file allowed to use an HTML parsing sink, for the std element `rawHtml` (follow up work) |

## Rules

* Attribute names must be on the allowlist for the element's tag (`attrs.js`); anything else is a `SinkError` with code `AttrName`. Event handlers (`on*`), `style`, `id`, `name`, form actions and namespaced names are never allowed.
* Values are strings (`AttrType`). Enumerated, integer and boolean attributes accept only their listed values (`AttrValue`).
* `href` on `a` and `src` on `img` go through `parseUrl`: relative URLs, `mailto` and `http` or `https` URLs that pass the host rule H are written; any other scheme, any control character or an `http` or `https` URL that fails H removes the attribute. A link without `href` is inert text. This is not an error, because the value comes from user data.
* The host rule H (D144) is the project's own rule, not the WHATWG URL Standard, and `url.js` does not call `new URL`. The VM side of `Url.parse` applies the same rule, and both sides are tested against one corpus, `tests/hostile/url/corpus.txt`. H is deliberately narrower than browsers: host names only in ASCII (international names in their `xn--` form), no IPv6 literals, no user info, no empty port, IPv4 only as four plain decimal parts, at most one trailing `.` (D146). `docs/url.md` lists these limits with examples.
* Safety rests on the control character rule and the scheme allowlist. H only decides whether a link is usable and fails closed.
* Text attributes (`title`, `alt`, `aria-*`, `placeholder`, ...) take any string as plain data; the DOM never parses them as markup.

## Error codes

`AttrName`, `AttrType`, `AttrValue`, `Element`.
