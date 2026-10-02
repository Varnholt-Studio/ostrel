# runtime/js/view/safe

Safe rendering sinks of the Ostrel programming language runtime (work package T3-1b, ARCHITECTURE 7.5, AC-47). The view core (`../core.js`) writes attributes only through the object exported here.

| File | Content |
|---|---|
| `sinks.js` | `safeSinks` (`{ setAttr(element, name, value), removeAttr(element, name) }`), `SinkError` |
| `attrs.js` | Attribute allowlist: global names, names per tag, the value rule of each |
| `url.js` | `parseUrl(text)`: the URL rule of `Url.parse` (SYNTAX 4.8) |
| `safe.test.mjs` | Unit tests, run by `ci/checks/50_tests.sh` with `node --test` |
| `raw_html.js` | Not present yet. The only file allowed to use an HTML parsing sink, for the std element `rawHtml` (follow up work) |

## Rules

* Attribute names must be on the allowlist for the element's tag (`attrs.js`); anything else is a `SinkError` with code `AttrName`. Event handlers (`on*`), `style`, `id`, `name`, form actions and namespaced names are never allowed.
* Values are strings (`AttrType`). Enumerated, integer and boolean attributes accept only their listed values (`AttrValue`).
* `href` on `a` and `src` on `img` go through `parseUrl`: relative URLs and `http`, `https`, `mailto` are written; any other scheme, any control character or an unparsable absolute URL removes the attribute. A link without `href` is inert text. This is not an error, because the value comes from user data.
* Text attributes (`title`, `alt`, `aria-*`, `placeholder`, ...) take any string as plain data; the DOM never parses them as markup.

## Error codes

`AttrName`, `AttrType`, `AttrValue`, `Element`.
