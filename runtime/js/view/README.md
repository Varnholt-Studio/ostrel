# runtime/js/view

View core of the Ostrel programming language runtime (work package T3-1, ARCHITECTURE 7.5 and 5.8).

| File | Content |
|---|---|
| `core.js` | Virtual nodes (`el`, `text`), `createRenderer(document, sinks)` with `mount`, `patch`, `unmount`, keyed child reconciliation with minimal moves, typed `ViewError` |
| `core.test.mjs` | Unit tests, run by `ci/checks/50_tests.sh` with `node --test` |
| `testing/fake_dom.js` | Minimal DOM stand in for the tests; counts every mutation |
| `safe/` | Safe sinks: attribute allowlist, URL rules, `raw_html.js` (T3-1b, separate owner) |

## Rules

* Text reaches the DOM only as text nodes. Text values are strings or finite numbers; anything else is a `ViewError` (`TextType`).
* Attributes reach the DOM only through `sinks.setAttr(element, name, value)` and `sinks.removeAttr(element, name)`. The renderer refuses to start without both. The production sinks come from `safe/`.
* Event handlers are attached with `addEventListener`, one listener per element and event; a patch swaps the handler without registering again.
* Tag names must match `[a-z][a-z0-9]*(-[a-z0-9]+)*`. The tags `base`, `embed`, `frame`, `frameset`, `iframe`, `link`, `meta`, `noscript`, `object`, `script`, `style` and `template` are refused (`TagBlocked`).
* Siblings are either all keyed or all unkeyed; keys are strings or finite numbers and unique among siblings. Keyed lists keep the DOM node of every surviving key and move only nodes outside the longest increasing subsequence.
* No file here uses an HTML parsing sink (pattern list in ARCHITECTURE 7.5, gate check `ci/checks/58_html_sinks.sh`).

## Error codes

`TagName`, `TagBlocked`, `TextType`, `AttrType`, `EventName`, `EventHandler`, `ChildType`, `KeyType`, `MixedKeys`, `DuplicateKey`, `VnodeReused`, `Document`, `Sinks`.
