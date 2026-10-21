// Hostile strings in the view runtime (SPEC 2 "Hostile input": any string rendered by a
// view is untrusted; ARCHITECTURE 7.5; AC-47).
//
// These tests drive the view core (core.js) together with the production attribute sinks
// (safe/sinks.js) and check, for every string of a hostile corpus and of a seeded random
// generator, that:
//   * text reaches the DOM only as one text node whose data equals the input exactly;
//   * a text attribute keeps its name and its exact value, and no other attribute appears;
//   * a URL attribute is either left unset or, resolved the way a browser resolves it,
//     has the scheme http, https or mailto. The test uses `new URL(value, base)` here only
//     as an independent model of the browser; the sinks themselves apply the narrower host
//     rule H of safe/url.js (ARCHITECTURE 7.5, D144) and never call the WHATWG parser;
//   * the DOM is touched only through a small set of methods that never parse markup.
//
// The DOM used here is a strict stand in: reading or writing any property outside its
// allowlist throws, so a code path that reached for a markup parsing API would fail the
// test instead of silently passing on a fake that does not implement it.
//
// rawHtml (T79) is out of scope here.

import { test } from "node:test";
import assert from "node:assert/strict";
import { el, createRenderer, ViewError } from "../core.js";
import { safeSinks, SinkError } from "../safe/sinks.js";

// Strict DOM

const NODE_PROPS = new Set(["nodeType", "parentNode", "childNodes", "ownerDocument"]);
const ELEMENT_METHODS = new Set([
  "appendChild",
  "insertBefore",
  "removeChild",
  "setAttribute",
  "removeAttribute",
  "addEventListener",
  "removeEventListener",
]);
const ELEMENT_PROPS = new Set([...NODE_PROPS, "localName", "tagName", "attrs", "listeners"]);
const TEXT_PROPS = new Set([...NODE_PROPS, "data"]);
// Read by the runtime or by node:assert when it inspects or prints a value.
const HARMLESS = new Set(["then", "constructor", "toJSON", "nodeName"]);

function guard(target, allowed, writable) {
  return new Proxy(target, {
    get(t, prop, receiver) {
      if (typeof prop === "symbol" || allowed.has(prop) || HARMLESS.has(prop)) {
        return Reflect.get(t, prop, receiver);
      }
      throw new Error(`view runtime read DOM property ${String(prop)}`);
    },
    set(t, prop, value, receiver) {
      if (writable.has(prop)) return Reflect.set(t, prop, value, receiver);
      throw new Error(`view runtime wrote DOM property ${String(prop)}`);
    },
  });
}

class StrictDocument {
  constructor() {
    this.log = [];
  }

  createElement(tag) {
    this.log.push(["createElement", tag]);
    const doc = this;
    const node = {
      nodeType: 1,
      localName: tag,
      tagName: tag.toUpperCase(),
      parentNode: null,
      childNodes: [],
      ownerDocument: this,
      attrs: new Map(),
      listeners: new Map(),
      appendChild(child) {
        return node.insertBefore(child, null);
      },
      insertBefore(child, ref) {
        doc.log.push(["insertBefore"]);
        if (child.parentNode) {
          const old = child.parentNode.childNodes;
          old.splice(old.indexOf(child), 1);
        }
        const at = ref === null ? node.childNodes.length : node.childNodes.indexOf(ref);
        assert.ok(at >= 0, "reference node is not a child");
        node.childNodes.splice(at, 0, child);
        child.parentNode = proxy;
        return child;
      },
      removeChild(child) {
        doc.log.push(["removeChild"]);
        const i = node.childNodes.indexOf(child);
        assert.ok(i >= 0, "node is not a child");
        node.childNodes.splice(i, 1);
        child.parentNode = null;
        return child;
      },
      setAttribute(name, value) {
        doc.log.push(["setAttribute", name, value]);
        assert.equal(typeof value, "string", "setAttribute gets a string");
        node.attrs.set(name, value);
      },
      removeAttribute(name) {
        doc.log.push(["removeAttribute", name]);
        node.attrs.delete(name);
      },
      addEventListener(name, fn) {
        doc.log.push(["addEventListener", name]);
        assert.equal(typeof fn, "function", "listener is a function");
        node.listeners.set(name, fn);
      },
      removeEventListener(name) {
        doc.log.push(["removeEventListener", name]);
        node.listeners.delete(name);
      },
    };
    const proxy = guard(node, new Set([...ELEMENT_PROPS, ...ELEMENT_METHODS]), new Set(["parentNode"]));
    return proxy;
  }

  createTextNode(data) {
    this.log.push(["createTextNode", data]);
    assert.equal(typeof data, "string", "createTextNode gets a string");
    const node = { nodeType: 3, data, parentNode: null, childNodes: [], ownerDocument: this };
    const doc = this;
    return guard(
      new Proxy(node, {
        set(t, prop, value) {
          if (prop === "data") doc.log.push(["data", value]);
          t[prop] = value;
          return true;
        },
      }),
      TEXT_PROPS,
      new Set(["data", "parentNode"]),
    );
  }
}

function setup() {
  const doc = new StrictDocument();
  const root = doc.createElement("div");
  const r = createRenderer(doc, safeSinks);
  doc.log.length = 0;
  return { doc, root, r };
}

function errorCode(fn) {
  try {
    fn();
  } catch (e) {
    assert.ok(e instanceof SinkError || e instanceof ViewError, `expected SinkError or ViewError, got ${e}`);
    return e.code;
  }
  assert.fail("expected an error");
}

// Every DOM call the view runtime made must be one of these, and createElement may only
// receive the tags the test itself rendered.
function assertOnlySafeCalls(doc, tags) {
  const allowed = new Set([
    "createElement",
    "createTextNode",
    "insertBefore",
    "removeChild",
    "setAttribute",
    "removeAttribute",
    "addEventListener",
    "removeEventListener",
    "data",
  ]);
  for (const entry of doc.log) {
    assert.ok(allowed.has(entry[0]), `unexpected DOM call ${entry[0]}`);
    if (entry[0] === "createElement") assert.ok(tags.has(entry[1]), `unexpected element <${entry[1]}>`);
  }
}

// Corpus

const BIDI = [
  "\u202a",
  "\u202b",
  "\u202c",
  "\u202d",
  "\u202e",
  "\u2066",
  "\u2067",
  "\u2068",
  "\u2069",
  "\u200e",
  "\u200f",
  "\u061c",
];

const SCRIPT_TAGS = [
  "<script>alert(1)</script>",
  "<SCRIPT SRC=//x.example/x.js></SCRIPT>",
  "<scr<script>ipt>alert(1)</scr</script>ipt>",
  "</p><script>alert(1)</script>",
  "</title></textarea></style><script>alert(1)</script>",
  "<!--<script>alert(1)//-->",
  "<![CDATA[<script>alert(1)</script>]]>",
  "<svg onload=alert(1)>",
  "<svg><script>alert(1)</script></svg>",
  "<math><mi><mglyph><style><img src=x onerror=alert(1)>",
  "<iframe srcdoc='<script>alert(1)</script>'>",
  "<img src=x onerror=alert(1)>",
  "&lt;script&gt;alert(1)&lt;/script&gt;",
  "<script>alert(1)</script>",
  "<script\u0000>alert(1)</script>",
];

const ATTRIBUTE_BREAKOUTS = [
  '"><img src=x onerror=alert(1)>',
  "'><img src=x onerror=alert(1)>",
  '" onmouseover="alert(1)',
  "' onfocus='alert(1)' autofocus='",
  "` onclick=alert(1) `",
  "x onerror=alert(1)",
  'x"/onerror="alert(1)',
  "&quot; onclick=&quot;alert(1)",
  '"\u0000 onclick="alert(1)',
  '" style="background:url(javascript:alert(1))',
  "\"><a href=javascript:alert(1)>x</a>",
];

const SCRIPT_URLS = [
  "javascript:alert(1)",
  "JaVaScRiPt:alert(1)",
  "  javascript:alert(1)  ",
  "\tjavascript:alert(1)",
  "\njavascript:alert(1)",
  "\u0000javascript:alert(1)",
  "\u0001javascript:alert(1)",
  "\u001fjavascript:alert(1)",
  "java\tscript:alert(1)",
  "java\nscript:alert(1)",
  "java\rscript:alert(1)",
  "java\u0000script:alert(1)",
  "javascript\u0000:alert(1)",
  "javascript:alert(1)\u0000",
  "javascript://%0aalert(1)",
  "javascript:/*x*/alert(1)",
  "jav&#x61;script:alert(1)",
  "&#106;avascript:alert(1)",
  "javascript&colon;alert(1)",
  "\u202ejavascript:alert(1)",
  "java\u202escript:alert(1)",
  "\ufeffjavascript:alert(1)",
  "\u00a0javascript:alert(1)",
  "\u3000javascript:alert(1)",
  "java\u00adscript:alert(1)",
  "javascript\uff1aalert(1)",
  "vbscript:msgbox(1)",
  "VBScript:msgbox(1)",
  "livescript:alert(1)",
  "data:text/html,<script>alert(1)</script>",
  "DATA:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==",
  " data:image/svg+xml,<svg onload=alert(1)>",
  "blob:https://example.org/1",
  "filesystem:https://example.org/x",
  "file:///etc/passwd",
  "x:javascript:alert(1)",
  "https:javascript:alert(1)",
  "/\\javascript:alert(1)",
  "\\\\javascript:alert(1)",
  "//javascript:alert(1)",
];

const BIDI_AND_NUL = [
  "",
  "\u0000",
  "a\u0000b",
  "\u0000\u0000\u0000",
  "\u0001\u001f\u007f\u0085",
  "\ufeff",
  "\ufffe\uffff",
  "\ud800",
  "\udc00x\ud800",
  "\u2028\u2029",
  "\u200b\u200c\u200d\u2060",
  "abc\u202edcba",
  "file\u202egnp.exe",
  "\u2067admin\u2069 user",
  "\u202a\u202b\u202c\u202d\u202e\u2066\u2067\u2068\u2069",
  ...BIDI.map((c) => `x${c}y`),
];

const ALL = [...SCRIPT_TAGS, ...ATTRIBUTE_BREAKOUTS, ...SCRIPT_URLS, ...BIDI_AND_NUL];

// Seeded generator (xorshift32) over an alphabet built for markup, attribute and URL
// breakouts, so a failure is reproducible from the seed in the message.
function generator(seed) {
  let s = seed >>> 0 || 1;
  const next = () => {
    s ^= s << 13;
    s >>>= 0;
    s ^= s >>> 17;
    s ^= s << 5;
    s >>>= 0;
    return s;
  };
  const pieces = [
    "<", ">", '"', "'", "`", "=", "&", "#", ";", ":", "/", "\\", " ", "\t", "\n", "\r",
    "\u0000", "\u0001", "\u001f", "\u007f", "\u00a0", "\u00ad", "\ufeff", "\u3000", "%0a",
    "script", "javascript", "JAVASCRIPT", "java", "data", "vbscript", "http", "https",
    "mailto", "onerror", "onclick", "alert(1)", "img", "src", "href", "&#58;", "&colon;",
    "x", "1", ...BIDI,
  ];
  return () => {
    const length = next() % 12;
    let out = "";
    for (let i = 0; i < length; i++) out += pieces[next() % pieces.length];
    return out;
  };
}

// Text

test("hostile text is rendered as exactly one text node with the exact value", () => {
  for (const value of ALL) {
    const { doc, root, r } = setup();
    r.mount(root, el("p", {}, [value]));
    const p = root.childNodes[0];
    assert.equal(p.childNodes.length, 1, JSON.stringify(value));
    assert.equal(p.childNodes[0].nodeType, 3, JSON.stringify(value));
    assert.equal(p.childNodes[0].data, value, JSON.stringify(value));
    assert.equal(p.attrs.size, 0, JSON.stringify(value));
    assertOnlySafeCalls(doc, new Set(["p"]));
  }
});

test("hostile text set by a patch replaces the text node data and nothing else", () => {
  for (const value of ALL) {
    const { doc, root, r } = setup();
    const before = r.mount(root, el("p", {}, ["safe"]));
    const textNode = root.childNodes[0].childNodes[0];
    doc.log.length = 0;
    r.patch(root, before, el("p", {}, [value]));
    assert.equal(root.childNodes[0].childNodes[0], textNode, "the text node is kept");
    assert.equal(textNode.data, value, JSON.stringify(value));
    const expected = value === "safe" ? [] : [["data", value]];
    assert.deepEqual(doc.log, expected, JSON.stringify(value));
  }
});

test("hostile text next to elements and inside keyed rows stays text", () => {
  const { doc, root, r } = setup();
  const rows = ALL.map((value, i) => el("li", { key: i, attrs: { title: value } }, [value, el("b", {}, [value])]));
  const v = r.mount(root, el("ul", {}, rows));
  const ul = root.childNodes[0];
  assert.equal(ul.childNodes.length, ALL.length);
  ul.childNodes.forEach((li, i) => {
    assert.equal(li.localName, "li");
    assert.equal(li.childNodes.length, 2);
    assert.equal(li.childNodes[0].data, ALL[i]);
    assert.equal(li.childNodes[1].localName, "b");
    assert.equal(li.childNodes[1].childNodes[0].data, ALL[i]);
    assert.deepEqual([...li.attrs], [["title", ALL[i]]]);
  });
  // Reverse the rows: every node is reused, and still only text.
  const reversed = [...ALL].reverse();
  const rows2 = reversed.map((value, j) => {
    const i = ALL.length - 1 - j;
    return el("li", { key: i, attrs: { title: value } }, [value, el("b", {}, [value])]);
  });
  r.patch(root, v, el("ul", {}, rows2));
  ul.childNodes.forEach((li, j) => {
    assert.equal(li.childNodes[0].data, reversed[j]);
    assert.deepEqual([...li.attrs], [["title", reversed[j]]]);
  });
  assertOnlySafeCalls(doc, new Set(["ul", "li", "b"]));
});

test("tag names built from hostile strings are refused before any DOM call", () => {
  const { doc } = setup();
  for (const value of [...SCRIPT_TAGS, ...ATTRIBUTE_BREAKOUTS, ...BIDI_AND_NUL, "script", "p\u0000", "p\u202e", "p "]) {
    assert.ok(["TagName", "TagBlocked"].includes(errorCode(() => el(value, {}, []))), JSON.stringify(value));
  }
  assert.deepEqual(doc.log, []);
});

// Attributes

const TEXT_ATTRIBUTES = [
  ["span", "title"],
  ["span", "class"],
  ["span", "lang"],
  ["span", "role"],
  ["span", "aria-label"],
  ["span", "data-look"],
  ["img", "alt"],
  ["input", "value"],
  ["input", "placeholder"],
  ["input", "min"],
  ["textarea", "placeholder"],
  ["option", "value"],
];

test("hostile values of text attributes are stored verbatim under their own name only", () => {
  for (const [tag, name] of TEXT_ATTRIBUTES) {
    for (const value of ALL) {
      const { doc, root, r } = setup();
      r.mount(root, el(tag, { attrs: { [name]: value } }, []));
      const node = root.childNodes[0];
      assert.deepEqual([...node.attrs], [[name, value]], `${tag} ${name} ${JSON.stringify(value)}`);
      assert.equal(node.childNodes.length, 0);
      assert.equal(node.listeners.size, 0);
      assertOnlySafeCalls(doc, new Set([tag]));
    }
  }
});

test("hostile values of text attributes set by a patch are stored verbatim", () => {
  for (const value of ALL) {
    const { root, r } = setup();
    const v = r.mount(root, el("span", { attrs: { title: "a", "aria-label": "b" } }, []));
    r.patch(root, v, el("span", { attrs: { title: value, "aria-label": value } }, []));
    assert.deepEqual([...root.childNodes[0].attrs], [["title", value], ["aria-label", value]]);
  }
});

test("hostile values of enumerated, integer and flag attributes are refused", () => {
  const cases = [
    ["span", "dir"],
    ["span", "tabindex"],
    ["span", "hidden"],
    ["span", "aria-current"],
    ["button", "type"],
    ["input", "type"],
    ["input", "maxlength"],
    ["img", "loading"],
    ["img", "width"],
  ];
  for (const [tag, name] of cases) {
    for (const value of [...SCRIPT_TAGS, ...ATTRIBUTE_BREAKOUTS, "rtl\u0000", "ltr\u202e", "1\u0000", "1 ", "\u202e1", "hidden\u0000"]) {
      const { root, r } = setup();
      assert.equal(errorCode(() => r.mount(root, el(tag, { attrs: { [name]: value } }, []))), "AttrValue", `${tag} ${name} ${JSON.stringify(value)}`);
    }
  }
});

test("event handler and style attributes are refused whatever their value", () => {
  for (const name of ["onclick", "onerror", "onload", "onmouseover", "ONCLICK", "style", "srcset", "formaction", "xlink:href", "href"]) {
    for (const value of ["alert(1)", "javascript:alert(1)", ""]) {
      const { doc, root, r } = setup();
      assert.equal(errorCode(() => r.mount(root, el("span", { attrs: { [name]: value } }, []))), "AttrName", name);
      assert.ok(!doc.log.some((e) => e[0] === "setAttribute"), name);
    }
  }
});

test("a string is never accepted as an event handler", () => {
  for (const value of ["alert(1)", "javascript:alert(1)", ""]) {
    assert.equal(errorCode(() => el("button", { on: { click: value } }, [])), "EventHandler");
  }
  for (const name of ["click\u0000", "click ", "cl\u202eick", ""]) {
    assert.equal(errorCode(() => el("button", { on: { [name]: () => {} } }, [])), "EventName", JSON.stringify(name));
  }
});

// URLs

const BASE = "https://app.example/rooms/1";
const SAFE_PROTOCOLS = new Set(["http:", "https:", "mailto:"]);

// Resolves a written URL attribute the way a browser does and checks its scheme.
function assertSafeUrl(written, label) {
  let resolved;
  try {
    resolved = new URL(written, BASE);
  } catch {
    return; // a URL the browser cannot parse is not followed
  }
  assert.ok(SAFE_PROTOCOLS.has(resolved.protocol), `${label}: written ${JSON.stringify(written)} resolves to ${resolved.protocol}`);
}

function renderUrl(tag, name, value) {
  const { doc, root, r } = setup();
  r.mount(root, el(tag, { attrs: { [name]: value } }, ["x"]));
  assertOnlySafeCalls(doc, new Set([tag]));
  return root.childNodes[0].attrs.get(name);
}

test("script, data and other scheme URLs never reach href or src", () => {
  for (const value of SCRIPT_URLS) {
    for (const [tag, name] of [["a", "href"], ["img", "src"]]) {
      const written = renderUrl(tag, name, value);
      if (written !== undefined) assertSafeUrl(written, `${tag} ${name} ${JSON.stringify(value)}`);
    }
  }
});

test("every hostile string written to href or src resolves to http, https or mailto", () => {
  for (const value of ALL) {
    for (const [tag, name] of [["a", "href"], ["img", "src"]]) {
      const written = renderUrl(tag, name, value);
      if (written !== undefined) assertSafeUrl(written, `${tag} ${name} ${JSON.stringify(value)}`);
    }
  }
});

test("a URL patched from safe to hostile is removed, not kept", () => {
  for (const value of ["javascript:alert(1)", "data:text/html,x", "java\tscript:alert(1)", "\u0000javascript:x"]) {
    const { root, r } = setup();
    const v = r.mount(root, el("a", { attrs: { href: "https://example.org/" } }, ["x"]));
    assert.equal(root.childNodes[0].attrs.get("href"), "https://example.org/");
    r.patch(root, v, el("a", { attrs: { href: value } }, ["x"]));
    assert.equal(root.childNodes[0].attrs.has("href"), false, JSON.stringify(value));
  }
});

test("NUL and bidi characters in URLs never yield a script URL", () => {
  for (const c of [...BIDI, "\u0000", "\ufeff", "\u200b"]) {
    for (const value of [`${c}javascript:alert(1)`, `java${c}script:alert(1)`, `javascript${c}:alert(1)`, `https://example.org/${c}`, `/a${c}b`]) {
      const written = renderUrl("a", "href", value);
      if (written !== undefined) assertSafeUrl(written, JSON.stringify(value));
    }
  }
});

// Random strings

test("seeded random hostile strings: text exact, text attributes exact, URLs safe", () => {
  for (const seed of [1, 140, 2026, 0x5eed]) {
    const next = generator(seed);
    for (let i = 0; i < 1500; i++) {
      const value = next();
      const label = `seed ${seed} case ${i} ${JSON.stringify(value)}`;
      const { doc, root, r } = setup();
      r.mount(root, el("div", {}, [
        el("p", { attrs: { title: value, "aria-label": value } }, [value]),
        el("a", { attrs: { href: value } }, [value]),
        el("img", { attrs: { src: value, alt: value } }, []),
      ]));
      const [p, a, img] = root.childNodes[0].childNodes;
      assert.equal(p.childNodes.length, 1, label);
      assert.equal(p.childNodes[0].data, value, label);
      assert.deepEqual([...p.attrs], [["title", value], ["aria-label", value]], label);
      assert.equal(a.childNodes.length, 1, label);
      assert.equal(a.childNodes[0].data, value, label);
      assert.ok([...a.attrs.keys()].every((k) => k === "href"), label);
      assert.ok([...img.attrs.keys()].every((k) => k === "src" || k === "alt"), label);
      assert.equal(img.attrs.get("alt"), value, label);
      for (const url of [a.attrs.get("href"), img.attrs.get("src")]) {
        if (url !== undefined) assertSafeUrl(url, label);
      }
      assertOnlySafeCalls(doc, new Set(["div", "p", "a", "img"]));
    }
  }
});

// Large values

test("a 1 MiB hostile string stays one text node and one attribute value", () => {
  const value = "<script>\u202e\u0000\"'>".repeat(1 << 17).slice(0, 1 << 20);
  const { root, r } = setup();
  r.mount(root, el("p", { attrs: { title: value } }, [value]));
  const p = root.childNodes[0];
  assert.equal(p.childNodes.length, 1);
  assert.equal(p.childNodes[0].data.length, 1 << 20);
  assert.equal(p.attrs.get("title"), value);
});
