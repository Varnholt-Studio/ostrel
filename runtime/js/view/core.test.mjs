import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { el, text, createRenderer, ViewError } from "./core.js";
import { FakeDocument, plainSinks, show } from "./testing/fake_dom.js";

function setup() {
  const doc = new FakeDocument();
  const root = doc.createElement("div");
  const r = createRenderer(doc, plainSinks);
  doc.resetOps();
  return { doc, root, r };
}

function list(keys) {
  return el("ul", {}, keys.map((k) => el("li", { key: k }, [String(k)])));
}

function code(fn) {
  try {
    fn();
  } catch (e) {
    assert.ok(e instanceof ViewError, `expected ViewError, got ${e}`);
    return e.code;
  }
  assert.fail("expected a ViewError");
}

test("mount builds the tree with text nodes and attributes", () => {
  const { root, r } = setup();
  r.mount(root, el("p", { attrs: { class: "x" } }, ["hi ", 3, null, false, el("b", {}, ["!"])]));
  assert.equal(show(root), '<div><p class="x">"hi ""3"<b>"!"</b></p></div>');
});

test("markup in text stays text", () => {
  const { root, r } = setup();
  const payload = "<img src=x onerror=alert(1)>";
  const v = r.mount(root, el("p", {}, [payload]));
  const p = root.childNodes[0];
  assert.equal(p.childNodes.length, 1);
  assert.equal(p.childNodes[0].nodeType, 3);
  assert.equal(p.textContent, payload);
  r.patch(root, v, el("p", {}, ["</p><script>x</script>"]));
  assert.equal(p.childNodes[0].nodeType, 3);
  assert.equal(p.textContent, "</p><script>x</script>");
});

test("text accepts only strings and finite numbers", () => {
  assert.equal(code(() => text({})), "TextType");
  assert.equal(code(() => text(NaN)), "TextType");
  assert.equal(code(() => text(true)), "TextType");
  assert.equal(code(() => el("p", {}, [{ toString: () => "x" }])), "ChildType");
  assert.equal(text(-0.5).text, "-0.5");
});

test("tags are checked by syntax and blocklist", () => {
  for (const tag of ["script", "iframe", "object", "embed", "style", "base", "meta", "link"]) {
    assert.equal(code(() => el(tag, {}, [])), "TagBlocked", tag);
  }
  for (const tag of ["SCRIPT", "img onerror", "", "1a", "a-", "x:y"]) {
    assert.equal(code(() => el(tag, {}, [])), "TagName", tag);
  }
  assert.equal(el("my-widget", {}, []).tag, "my-widget");
});

test("attributes must be strings and reach the DOM only through the sinks", () => {
  assert.equal(code(() => el("a", { attrs: { href: 1 } }, [])), "AttrType");
  const doc = new FakeDocument();
  const root = doc.createElement("div");
  const calls = [];
  const r = createRenderer(doc, {
    setAttr: (e, n, v) => calls.push(["set", n, v]),
    removeAttr: (e, n) => calls.push(["remove", n]),
  });
  const a = r.mount(root, el("a", { attrs: { href: "/x", title: "t" } }, []));
  r.patch(root, a, el("a", { attrs: { href: "/y", lang: "de" } }, []));
  assert.deepEqual(calls, [
    ["set", "href", "/x"],
    ["set", "title", "t"],
    ["remove", "title"],
    ["set", "href", "/y"],
    ["set", "lang", "de"],
  ]);
  assert.equal(root.childNodes[0].attributes.size, 0);
});

test("renderer requires a document and both sinks", () => {
  const doc = new FakeDocument();
  assert.equal(code(() => createRenderer(null, plainSinks)), "Document");
  assert.equal(code(() => createRenderer(doc)), "Sinks");
  assert.equal(code(() => createRenderer(doc, { setAttr() {} })), "Sinks");
});

test("unchanged patch touches nothing", () => {
  const { doc, root, r } = setup();
  const v = r.mount(root, el("p", { attrs: { id: "a" } }, ["x", el("i", {}, ["y"])]));
  doc.resetOps();
  r.patch(root, v, el("p", { attrs: { id: "a" } }, ["x", el("i", {}, ["y"])]));
  assert.deepEqual(doc.ops, { create: 0, insert: 0, remove: 0, text: 0, attr: 0, listen: 0 });
});

test("text change updates the text node in place", () => {
  const { doc, root, r } = setup();
  const v = r.mount(root, el("p", {}, ["a"]));
  const node = root.childNodes[0].childNodes[0];
  doc.resetOps();
  r.patch(root, v, el("p", {}, ["b"]));
  assert.equal(root.childNodes[0].childNodes[0], node);
  assert.equal(node.data, "b");
  assert.deepEqual(doc.ops, { create: 0, insert: 0, remove: 0, text: 1, attr: 0, listen: 0 });
});

test("tag change replaces the element", () => {
  const { root, r } = setup();
  const v = r.mount(root, el("div", {}, [el("p", {}, ["a"]), "t"]));
  r.patch(root, v, el("div", {}, [el("h1", {}, ["a"]), el("i", {}, [])]));
  assert.equal(show(root), '<div><div><h1>"a"</h1><i></i></div></div>');
});

test("unkeyed children grow and shrink by index", () => {
  const { root, r } = setup();
  let v = r.mount(root, el("ol", {}, ["a"]));
  v = r.patch(root, v, el("ol", {}, ["a", "b", "c"]));
  assert.equal(show(root), '<div><ol>"a""b""c"</ol></div>');
  r.patch(root, v, el("ol", {}, ["z"]));
  assert.equal(show(root), '<div><ol>"z"</ol></div>');
});

test("events dispatch to the current handler without re-registering", () => {
  const { doc, root, r } = setup();
  const seen = [];
  const v = r.mount(root, el("button", { on: { click: () => seen.push(1) } }, ["go"]));
  const b = root.childNodes[0];
  b.dispatch("click");
  doc.resetOps();
  const v2 = r.patch(root, v, el("button", { on: { click: () => seen.push(2) } }, ["go"]));
  assert.equal(doc.ops.listen, 0);
  b.dispatch("click");
  assert.deepEqual(seen, [1, 2]);
  r.patch(root, v2, el("button", {}, ["go"]));
  assert.equal(b.listenerCount("click"), 0);
  b.dispatch("click");
  assert.deepEqual(seen, [1, 2]);
});

test("event names and handlers are checked", () => {
  assert.equal(code(() => el("a", { on: { onclick: "x" } }, [])), "EventHandler");
  assert.equal(code(() => el("a", { on: { "on-click": () => {} } }, [])), "EventName");
});

test("keyed siblings: duplicates and mixing are errors", () => {
  assert.equal(code(() => setup().r.mount(setup().root, list(["a", "a"]))), "DuplicateKey");
  const mixed = el("ul", {}, [el("li", { key: "a" }, []), el("li", {}, [])]);
  assert.equal(code(() => setup().r.mount(setup().root, mixed)), "MixedKeys");
  assert.equal(code(() => el("li", { key: {} }, [])), "KeyType");
  // A string key and a number key with the same digits are different keys.
  const { root, r } = setup();
  r.mount(root, list(["1", 1]));
  assert.equal(root.childNodes[0].childNodes.length, 2);
});

test("a mounted virtual node cannot be mounted again", () => {
  const { root, r } = setup();
  const child = el("i", {}, []);
  r.mount(root, el("p", {}, [child]));
  assert.equal(code(() => r.mount(root, el("p", {}, [child]))), "VnodeReused");
});

test("keyed reverse keeps every node and moves n minus 1", () => {
  const { doc, root, r } = setup();
  const keys = ["a", "b", "c", "d", "e"];
  const v = r.mount(root, list(keys));
  const before = new Map(root.childNodes[0].childNodes.map((n) => [n.textContent, n]));
  doc.resetOps();
  r.patch(root, v, list([...keys].reverse()));
  const ul = root.childNodes[0];
  assert.equal(ul.textContent, "edcba");
  for (const n of ul.childNodes) assert.equal(before.get(n.textContent), n);
  assert.deepEqual(doc.ops, { create: 0, insert: 4, remove: 0, text: 0, attr: 0, listen: 0 });
});

test("keyed move of one item is one insert", () => {
  const { doc, root, r } = setup();
  const v = r.mount(root, list(["a", "b", "c", "d", "e", "f"]));
  doc.resetOps();
  r.patch(root, v, list(["a", "c", "d", "e", "f", "b"]));
  assert.equal(root.childNodes[0].textContent, "acdefb");
  assert.equal(doc.ops.insert, 1);
  assert.equal(doc.ops.create, 0);
});

test("keyed insert and remove create and delete only the changed rows", () => {
  const { doc, root, r } = setup();
  const v = r.mount(root, list(["a", "b", "c"]));
  doc.resetOps();
  r.patch(root, v, list(["a", "x", "c"]));
  assert.equal(root.childNodes[0].textContent, "axc");
  // one li plus its text node created, one inserted, one removed
  assert.deepEqual(doc.ops, { create: 2, insert: 2, remove: 1, text: 0, attr: 0, listen: 0 });
});

test("keyed child with same key but other tag is replaced", () => {
  const { root, r } = setup();
  const v = r.mount(root, el("div", {}, [el("p", { key: 1 }, ["p"]), el("i", { key: 2 }, ["i"])]));
  r.patch(root, v, el("div", {}, [el("i", { key: 2 }, ["i"]), el("b", { key: 1 }, ["b"])]));
  assert.equal(show(root), '<div><div><i>"i"</i><b>"b"</b></div></div>');
});

test("switching between keyed and unkeyed children", () => {
  const { root, r } = setup();
  let v = r.mount(root, list(["a", "b"]));
  v = r.patch(root, v, el("ul", {}, [el("li", {}, ["x"])]));
  assert.equal(show(root), '<div><ul><li>"x"</li></ul></div>');
  r.patch(root, v, list(["c"]));
  assert.equal(show(root), '<div><ul><li>"c"</li></ul></div>');
});

test("randomised keyed patches match the target and keep node identity", () => {
  let seed = 12345;
  const rand = (n) => {
    seed = (seed * 1103515245 + 12345) % 2147483648;
    return seed % n;
  };
  const { root, r } = setup();
  let keys = [];
  let v = r.mount(root, list(keys));
  for (let round = 0; round < 300; round++) {
    const pool = Array.from({ length: 12 }, (_, i) => "k" + i);
    const next = pool.filter(() => rand(3) > 0);
    for (let i = next.length - 1; i > 0; i--) {
      const j = rand(i + 1);
      [next[i], next[j]] = [next[j], next[i]];
    }
    const ul = root.childNodes[0];
    const before = new Map(ul.childNodes.map((n) => [n.textContent, n]));
    v = r.patch(root, v, list(next));
    assert.deepEqual(ul.childNodes.map((n) => n.textContent), next, `round ${round}`);
    for (const n of ul.childNodes) {
      if (before.has(n.textContent)) assert.equal(before.get(n.textContent), n);
    }
    keys = next;
  }
  assert.ok(keys.length >= 0);
});

test("view core source contains no HTML parsing sinks", () => {
  // Patterns of ARCHITECTURE 7.5, built from parts so this file passes the gate grep itself.
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
  for (const file of ["./core.js", "./testing/fake_dom.js"]) {
    const src = readFileSync(new URL(file, import.meta.url), "utf8");
    for (const p of patterns) assert.ok(!src.includes(p), `${file} contains ${p}`);
  }
});

// Finding #1408: only nodes made by el() and text() may reach the DOM. A plain object
// with the same shape (for example from JSON) must never bypass the tag blocklist.
function forgedScript() {
  return JSON.parse(
    '{"kind":"el","tag":"script","attrs":{},"on":{},"children":[{"kind":"text","text":"alert(1)"}],"node":null}',
  );
}

test("forged element and text objects are rejected as children", () => {
  assert.equal(code(() => el("div", {}, [forgedScript()])), "ChildType");
  assert.equal(code(() => el("div", {}, [{ kind: "text", text: "x", node: null }])), "ChildType");
  const copy = { ...el("b", {}, ["x"]) };
  assert.equal(code(() => el("div", {}, [copy])), "ChildType");
  const shell = Object.create(Object.getPrototypeOf(text("x")));
  Object.assign(shell, text("x"));
  assert.equal(code(() => el("div", {}, [shell])), "ChildType");
});

test("mount and patch refuse forged virtual nodes before touching the DOM", () => {
  const { doc, root, r } = setup();
  assert.equal(code(() => r.mount(root, forgedScript())), "ChildType");
  assert.equal(code(() => r.mount(root, { kind: "text", text: "x", node: null })), "ChildType");
  assert.equal(root.childNodes.length, 0);
  assert.equal(doc.ops.create, 0);
  const v = r.mount(root, el("p", {}, ["a"]));
  doc.resetOps();
  assert.equal(code(() => r.patch(root, v, forgedScript())), "ChildType");
  const sameShape = { kind: "el", tag: "p", attrs: { title: "x" }, on: {}, children: [], node: null };
  assert.equal(code(() => r.patch(root, v, sameShape)), "ChildType");
  assert.equal(code(() => r.patch(root, v.children[0], { kind: "text", text: "b", node: null })), "ChildType");
  assert.deepEqual(doc.ops, { create: 0, insert: 0, remove: 0, text: 0, attr: 0, listen: 0 });
  assert.equal(show(root), '<div><p>"a"</p></div>');
});

test("a forged node smuggled into a real tree after creation is rejected", () => {
  const { doc, root, r } = setup();
  const pushed = el("div", {}, []);
  pushed.children.push(forgedScript());
  assert.equal(code(() => r.mount(root, pushed)), "ChildType");
  assert.equal(root.childNodes.length, 0);

  const old = r.mount(root, el("ul", {}, [el("li", { key: 1 }, ["a"])]));
  const next = el("ul", {}, [el("li", { key: 1 }, ["a"])]);
  next.children.push(Object.assign(forgedScript(), { key: 2 }));
  doc.resetOps();
  assert.equal(code(() => r.patch(root, old, next)), "ChildType");
  assert.equal(doc.ops.create, 0);
  assert.ok(!show(root).includes("script"));
});

test("a real node whose tag is changed after creation is still checked", () => {
  const { root, r } = setup();
  const v = el("p", {}, ["alert(1)"]);
  v.tag = "script";
  assert.equal(code(() => r.mount(root, v)), "TagBlocked");
  const ok = r.mount(root, el("p", {}, ["a"]));
  const swapped = el("p", {}, ["a"]);
  swapped.tag = "iframe";
  assert.equal(code(() => r.patch(root, ok, swapped)), "TagBlocked");
  assert.equal(show(root), '<div><p>"a"</p></div>');
});

test("a patch with a nested forged node changes nothing in the DOM", () => {
  const { doc, root, r } = setup();
  const keyedOld = r.mount(root, el("ul", {}, [el("li", { key: 0 }, ["x"]), el("li", { key: 1 }, ["a"])]));
  const keyedNew = el("ul", {}, [el("li", { key: 0 }, ["x"]), el("li", { key: 1 }, ["b"])]);
  keyedNew.children[0].children.push(forgedScript());
  doc.resetOps();
  assert.equal(code(() => r.patch(root, keyedOld, keyedNew)), "ChildType");
  assert.deepEqual(doc.ops, { create: 0, insert: 0, remove: 0, text: 0, attr: 0, listen: 0 });
  assert.equal(show(root), '<div><ul><li>"x"</li><li>"a"</li></ul></div>');

  const second = setup();
  const plainOld = second.r.mount(second.root, el("div", {}, [el("p", {}, ["a"]), el("p", {}, ["c"])]));
  const plainNew = el("div", {}, [el("p", {}, ["b"]), el("p", {}, ["c"])]);
  plainNew.children[1].children.push(forgedScript());
  second.doc.resetOps();
  assert.equal(code(() => second.r.patch(second.root, plainOld, plainNew)), "ChildType");
  assert.deepEqual(second.doc.ops, { create: 0, insert: 0, remove: 0, text: 0, attr: 0, listen: 0 });
  assert.equal(show(second.root), '<div><div><p>"a"</p><p>"c"</p></div></div>');
});

test("children replaced by a non array after creation give a ViewError", () => {
  for (const bad of [null, undefined, "abc", { length: 1, 0: "x" }]) {
    const { doc, root, r } = setup();
    const v = el("div", {}, ["a"]);
    v.children = bad;
    assert.equal(code(() => r.mount(root, v)), "ChildType");
    assert.equal(root.childNodes.length, 0);
    const old = r.mount(root, el("div", {}, ["a"]));
    const next = el("div", {}, ["b"]);
    next.children = bad;
    doc.resetOps();
    assert.equal(code(() => r.patch(root, old, next)), "ChildType");
    assert.equal(doc.ops.text, 0);
  }
});
