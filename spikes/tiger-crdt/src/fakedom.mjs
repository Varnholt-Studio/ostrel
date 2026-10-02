// Minimal DOM for Node runs and tests: just the calls the views use. It is not a
// performance model of a browser; Node view timings are labelled as such.
class Node {
  constructor(tag) {
    this.tagName = tag;
    this.childNodes = [];
    this.parentNode = null;
    this.attrs = {};
    this.text = '';
  }

  get firstChild() {
    return this.childNodes[0] || null;
  }

  get nextSibling() {
    if (!this.parentNode) return null;
    const sibs = this.parentNode.childNodes;
    return sibs[sibs.indexOf(this) + 1] || null;
  }

  appendChild(n) {
    return this.insertBefore(n, null);
  }

  insertBefore(n, ref) {
    if (n.parentNode) n.parentNode.removeChild(n);
    const at = ref === null ? this.childNodes.length : this.childNodes.indexOf(ref);
    if (at < 0) throw new Error('reference is not a child');
    this.childNodes.splice(at, 0, n);
    n.parentNode = this;
    return n;
  }

  removeChild(n) {
    const at = this.childNodes.indexOf(n);
    if (at < 0) throw new Error('not a child');
    this.childNodes.splice(at, 1);
    n.parentNode = null;
    return n;
  }

  setAttribute(k, v) {
    this.attrs[k] = String(v);
  }

  getAttribute(k) {
    return this.attrs[k] ?? null;
  }

  set textContent(v) {
    this.childNodes = [];
    this.text = String(v);
  }

  get textContent() {
    return this.childNodes.length ? this.childNodes.map((c) => c.textContent).join('') : this.text;
  }
}

export function createDocument() {
  return {
    createElement: (tag) => new Node(tag),
    body: new Node('body'),
  };
}
