// Minimal DOM stand in for unit tests of the view core under `node --test`.
// It implements only the operations the core uses and counts every mutation,
// so tests can assert that a patch touched no more nodes than needed.

class FakeNode {
  constructor(doc) {
    this.ownerDocument = doc;
    this.parentNode = null;
    this.childNodes = [];
  }

  get nextSibling() {
    const p = this.parentNode;
    if (!p) return null;
    const i = p.childNodes.indexOf(this);
    return p.childNodes[i + 1] ?? null;
  }

  appendChild(child) {
    return this.insertBefore(child, null);
  }

  insertBefore(child, ref) {
    if (ref !== null && ref.parentNode !== this) throw new Error("reference is not a child");
    if (child.parentNode) {
      const old = child.parentNode.childNodes;
      old.splice(old.indexOf(child), 1);
    }
    const at = ref === null ? this.childNodes.length : this.childNodes.indexOf(ref);
    this.childNodes.splice(at, 0, child);
    child.parentNode = this;
    this.ownerDocument.ops.insert += 1;
    return child;
  }

  removeChild(child) {
    const i = this.childNodes.indexOf(child);
    if (i < 0) throw new Error("node is not a child");
    this.childNodes.splice(i, 1);
    child.parentNode = null;
    this.ownerDocument.ops.remove += 1;
    return child;
  }

  get textContent() {
    return this.childNodes.map((c) => c.textContent).join("");
  }
}

class FakeText extends FakeNode {
  constructor(doc, data) {
    super(doc);
    this.nodeType = 3;
    this._data = data;
  }

  get data() {
    return this._data;
  }

  set data(value) {
    this._data = String(value);
    this.ownerDocument.ops.text += 1;
  }

  get textContent() {
    return this._data;
  }
}

class FakeElement extends FakeNode {
  constructor(doc, tag) {
    super(doc);
    this.nodeType = 1;
    this.tagName = tag;
    this.attributes = new Map();
    this.listeners = new Map();
  }

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
    this.ownerDocument.ops.attr += 1;
  }

  removeAttribute(name) {
    this.attributes.delete(name);
    this.ownerDocument.ops.attr += 1;
  }

  getAttribute(name) {
    return this.attributes.has(name) ? this.attributes.get(name) : null;
  }

  addEventListener(name, fn) {
    if (!this.listeners.has(name)) this.listeners.set(name, new Set());
    this.listeners.get(name).add(fn);
    this.ownerDocument.ops.listen += 1;
  }

  removeEventListener(name, fn) {
    this.listeners.get(name)?.delete(fn);
    this.ownerDocument.ops.listen += 1;
  }

  dispatch(name, event = {}) {
    for (const fn of this.listeners.get(name) ?? []) fn(event);
  }

  listenerCount(name) {
    return this.listeners.get(name)?.size ?? 0;
  }
}

export class FakeDocument {
  constructor() {
    this.ops = {};
    this.resetOps();
  }

  resetOps() {
    this.ops = { create: 0, insert: 0, remove: 0, text: 0, attr: 0, listen: 0 };
  }

  createElement(tag) {
    this.ops.create += 1;
    return new FakeElement(this, tag);
  }

  createTextNode(data) {
    this.ops.create += 1;
    return new FakeText(this, String(data));
  }
}

/** Plain sinks for tests: write every attribute. The real allowlist lives in safe/. */
export const plainSinks = {
  setAttr(element, name, value) {
    element.setAttribute(name, value);
  },
  removeAttr(element, name) {
    element.removeAttribute(name);
  },
};

/** Serialises a fake tree for readable assertions, for example `<ul><li a="1">x</li></ul>`. */
export function show(node) {
  if (node.nodeType === 3) return JSON.stringify(node.data);
  const attrs = [...node.attributes].map(([k, v]) => ` ${k}=${JSON.stringify(v)}`).join("");
  return `<${node.tagName}${attrs}>${node.childNodes.map(show).join("")}</${node.tagName}>`;
}
