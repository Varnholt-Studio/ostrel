import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { parseUrl } from "./url.js";
import { safeSinks, setAttr, removeAttr, SinkError } from "./sinks.js";

// Minimal element: records attributes like the DOM, and nothing else.
function element(tag) {
  const attributes = new Map();
  return {
    localName: tag,
    attributes,
    setAttribute(name, value) {
      attributes.set(name, String(value));
    },
    removeAttribute(name) {
      attributes.delete(name);
    },
  };
}

function code(fn) {
  try {
    fn();
  } catch (e) {
    assert.ok(e instanceof SinkError, `expected SinkError, got ${e}`);
    return e.code;
  }
  assert.fail("expected a SinkError");
}

// URL rule

test("parseUrl accepts relative URLs unchanged", () => {
  for (const url of ["/chat", "chat/1", "./a", "../b", "?q=1", "#top", "//cdn.example/x.png", "a/b:c", "x?y=javascript:1"]) {
    assert.equal(parseUrl(url), url, url);
  }
});

test("parseUrl accepts http, https and mailto in any case", () => {
  for (const url of ["https://example.org/a?b#c", "http://example.org", "HTTPS://EXAMPLE.ORG", "mailto:legal@elchi.dev", "MailTo:a@b.c"]) {
    assert.equal(parseUrl(url), url, url);
  }
});

test("parseUrl trims surrounding spaces only", () => {
  assert.equal(parseUrl("  https://example.org  "), "https://example.org");
  assert.equal(parseUrl("  "), null);
  assert.equal(parseUrl(""), null);
});

test("parseUrl refuses script, data and unknown schemes", () => {
  const refused = [
    "javascript:alert(1)",
    "JavaScript:alert(1)",
    "JAVASCRIPT:alert(1)",
    " javascript:alert(1)",
    "data:text/html,<script>alert(1)</script>",
    "data:image/svg+xml;base64,PHN2Zz4=",
    "vbscript:msgbox(1)",
    "blob:https://example.org/1",
    "file:///etc/passwd",
    "ftp://example.org",
    "x-custom:thing",
  ];
  for (const url of refused) assert.equal(parseUrl(url), null, url);
});

test("parseUrl refuses control characters that browsers would strip", () => {
  const refused = [
    "java\tscript:alert(1)",
    "java\nscript:alert(1)",
    "java\rscript:alert(1)",
    "\u0000javascript:alert(1)",
    "\u0001javascript:alert(1)",
    "javascript\u0000:alert(1)",
    "https://example.org/\u007f",
    "/path\u0085",
  ];
  for (const url of refused) assert.equal(parseUrl(url), null, JSON.stringify(url));
});

test("parseUrl refuses absolute http URLs the URL parser rejects", () => {
  assert.equal(parseUrl("http://"), null);
  assert.equal(parseUrl("https://exa mple.org"), null);
});

test("parseUrl refuses non strings", () => {
  for (const value of [null, undefined, 1, {}, ["https://x.org"]]) assert.equal(parseUrl(value), null);
});

// Attribute sinks

test("safeSinks has the shape the view core expects", () => {
  assert.equal(typeof safeSinks.setAttr, "function");
  assert.equal(typeof safeSinks.removeAttr, "function");
  assert.ok(Object.isFrozen(safeSinks));
});

test("global attributes are allowed on every element", () => {
  const e = element("div");
  setAttr(e, "class", "o-row chat");
  setAttr(e, "role", "list");
  setAttr(e, "aria-label", "Messages <script>");
  setAttr(e, "aria-current", "page");
  setAttr(e, "data-look", "muted");
  setAttr(e, "data-pending", "");
  setAttr(e, "tabindex", "-1");
  setAttr(e, "hidden", "");
  assert.equal(e.attributes.get("aria-label"), "Messages <script>");
  assert.equal(e.attributes.size, 8);
});

test("event handlers, style, id and unknown names are refused", () => {
  const e = element("div");
  for (const name of ["onclick", "onerror", "ONCLICK", "style", "id", "name", "xlink:href", "formaction", "src", "href", "data-x", ""]) {
    assert.equal(code(() => setAttr(e, name, "x")), "AttrName", name);
  }
  assert.equal(code(() => setAttr(e, 1, "x")), "AttrName");
  assert.equal(e.attributes.size, 0);
});

test("tag specific attributes are allowed only on their tag", () => {
  const img = element("img");
  setAttr(img, "alt", "avatar");
  setAttr(img, "width", "32");
  assert.equal(img.attributes.get("alt"), "avatar");
  assert.equal(code(() => setAttr(element("span"), "alt", "x")), "AttrName");
  assert.equal(code(() => setAttr(element("button"), "href", "/x")), "AttrName");
  assert.equal(code(() => setAttr(element("form"), "action", "/x")), "AttrName");
});

test("tag names are compared in lowercase, as the DOM reports them", () => {
  const e = { tagName: "A", attributes: new Map(), setAttribute(n, v) { this.attributes.set(n, v); }, removeAttribute(n) { this.attributes.delete(n); } };
  setAttr(e, "href", "/x");
  assert.equal(e.attributes.get("href"), "/x");
  assert.equal(code(() => setAttr({}, "class", "x")), "Element");
});

test("enumerated, integer and flag values are checked", () => {
  const input = element("input");
  setAttr(input, "type", "email");
  setAttr(input, "maxlength", "200");
  setAttr(input, "disabled", "disabled");
  assert.equal(code(() => setAttr(input, "type", "image")), "AttrValue");
  assert.equal(code(() => setAttr(input, "type", "Email")), "AttrValue");
  assert.equal(code(() => setAttr(input, "maxlength", "1e3")), "AttrValue");
  assert.equal(code(() => setAttr(input, "maxlength", "")), "AttrValue");
  assert.equal(code(() => setAttr(input, "disabled", "false")), "AttrValue");
  assert.equal(code(() => setAttr(element("button"), "type", "image")), "AttrValue");
  assert.equal(code(() => setAttr(element("div"), "dir", "up")), "AttrValue");
  assert.equal(input.attributes.get("type"), "email");
});

test("values must be strings", () => {
  for (const value of [1, null, undefined, true, {}]) {
    assert.equal(code(() => setAttr(element("div"), "class", value)), "AttrType");
  }
});

test("allowed URLs are written, refused URLs leave the attribute unset", () => {
  const a = element("a");
  setAttr(a, "href", " https://example.org ");
  assert.equal(a.attributes.get("href"), "https://example.org");
  setAttr(a, "href", "javascript:alert(1)");
  assert.equal(a.attributes.has("href"), false, "a refused URL also clears the previous one");

  const img = element("img");
  setAttr(img, "src", "data:image/png;base64,AAAA");
  assert.equal(img.attributes.has("src"), false);
  setAttr(img, "src", "/avatar.png");
  assert.equal(img.attributes.get("src"), "/avatar.png");
});

test("removeAttr removes allowed attributes and refuses unknown names", () => {
  const a = element("a");
  setAttr(a, "href", "/x");
  removeAttr(a, "href");
  assert.equal(a.attributes.has("href"), false);
  assert.equal(code(() => removeAttr(a, "onclick")), "AttrName");
});

test("XSS corpus never reaches the DOM as a live attribute", () => {
  const corpus = [
    "<script>alert(1)</script>",
    "\"><img src=x onerror=alert(1)>",
    "javascript:alert(1)",
    "  JaVaScRiPt:alert(1)",
    "java\u0000script:alert(1)",
    "data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==",
    "vbscript:x",
  ];
  for (const value of corpus) {
    const a = element("a");
    setAttr(a, "href", value);
    const href = a.attributes.get("href");
    assert.ok(href === undefined || parseUrl(href) === href, JSON.stringify(value));
    assert.ok(href === undefined || !/^\s*[a-z]+script:|^\s*data:/i.test(href), JSON.stringify(value));

    const span = element("span");
    setAttr(span, "title", value);
    assert.equal(span.attributes.get("title"), value, "text attributes keep the value as data");
  }
});

test("safe sink sources contain no HTML parsing sinks", () => {
  // Patterns of ARCHITECTURE 7.5, built from parts so this file passes the gate grep itself.
  // raw_html.js is the one allowed exception and is skipped when it exists.
  const patterns = [
    ["inner", "HTML"],
    ["outer", "HTML"],
    ["insertAdjacent", "HTML"],
    ["document", ".write"],
    ["write", "ln"],
    ["createContextual", "Fragment"],
    ["setHTML", "Unsafe"],
    ["parseHTML", "Unsafe"],
    ["DOM", "Parser"],
    ["src", "doc"],
    ["new ", "Function"],
    ["ev", "al("],
  ].map((p) => p.join(""));
  const dir = new URL(".", import.meta.url);
  const files = readdirSync(dir).filter((f) => /\.m?js$/.test(f) && f !== "raw_html.js");
  assert.ok(files.length >= 4);
  for (const file of files) {
    const source = readFileSync(new URL(file, dir), "utf8");
    for (const pattern of patterns) assert.ok(!source.includes(pattern), `${file} contains ${pattern}`);
  }
});
