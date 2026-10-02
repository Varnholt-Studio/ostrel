// View core of the Ostrel runtime: virtual nodes, mounting and keyed DOM patching.
//
// Rendering safety (ARCHITECTURE 7.5): this module never parses markup. Text is
// written only through text nodes, attributes only through the injected sinks
// (see createRenderer), event handlers only through addEventListener.
//
// A virtual node is a plain object:
//   { kind: "text", text }
//   { kind: "el", tag, key, attrs, on, children }
// Mounting stores the created DOM node in `node`. A virtual node belongs to at
// most one place in a tree; reusing a mounted node elsewhere is an error.

/** Error raised for invalid view descriptions. `code` is stable for tests. */
export class ViewError extends Error {
  constructor(code, message) {
    super(`${code}: ${message}`);
    this.name = "ViewError";
    this.code = code;
  }
}

// Tags the core refuses to create, because they execute code, load documents
// or change how the page resolves URLs. Std elements never need them.
const BLOCKED_TAGS = new Set([
  "base",
  "embed",
  "frame",
  "frameset",
  "iframe",
  "link",
  "meta",
  "noscript",
  "object",
  "script",
  "style",
  "template",
]);

const TAG_SYNTAX = /^[a-z][a-z0-9]*(-[a-z0-9]+)*$/;
const EVENT_SYNTAX = /^[a-z]+$/;

function checkTag(tag) {
  if (typeof tag !== "string" || !TAG_SYNTAX.test(tag)) {
    throw new ViewError("TagName", `invalid tag name ${JSON.stringify(String(tag))}`);
  }
  if (BLOCKED_TAGS.has(tag)) {
    throw new ViewError("TagBlocked", `tag <${tag}> is not allowed in views`);
  }
}

function toText(value) {
  if (typeof value === "string") return value;
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  throw new ViewError("TextType", `text must be a string or a finite number, got ${typeof value}`);
}

function keyId(key) {
  if (typeof key === "string") return "s:" + key;
  if (typeof key === "number" && Number.isFinite(key)) return "n:" + key;
  throw new ViewError("KeyType", `key must be a string or a finite number, got ${typeof key}`);
}

/** Creates a text virtual node. */
export function text(value) {
  return { kind: "text", text: toText(value), node: null };
}

/**
 * Creates an element virtual node.
 * props: { key?, attrs?: { name: string }, on?: { event: function } }
 * children: array of virtual nodes, strings or numbers; null, undefined and false are skipped.
 */
export function el(tag, props, children) {
  checkTag(tag);
  const p = props ?? {};
  const attrs = p.attrs ?? {};
  const on = p.on ?? {};
  for (const name of Object.keys(attrs)) {
    if (typeof attrs[name] !== "string") {
      throw new ViewError("AttrType", `attribute ${name} must be a string`);
    }
  }
  for (const name of Object.keys(on)) {
    if (!EVENT_SYNTAX.test(name)) throw new ViewError("EventName", `invalid event name ${name}`);
    if (typeof on[name] !== "function") {
      throw new ViewError("EventHandler", `handler for ${name} must be a function`);
    }
  }
  const kids = [];
  for (const child of children ?? []) {
    if (child === null || child === undefined || child === false) continue;
    if (typeof child === "string" || typeof child === "number") kids.push(text(child));
    else if (child && (child.kind === "el" || child.kind === "text")) kids.push(child);
    else throw new ViewError("ChildType", "child must be a virtual node, string or number");
  }
  const key = p.key === undefined ? undefined : p.key;
  if (key !== undefined) keyId(key);
  return { kind: "el", tag, key, attrs, on, children: kids, node: null };
}

// Children are either all keyed or all unkeyed; keys are unique among siblings.
// Returns true for a keyed list.
function checkSiblings(children) {
  if (children.length === 0) return false;
  const keyed = children[0].kind === "el" && children[0].key !== undefined;
  const seen = new Set();
  for (const child of children) {
    const hasKey = child.kind === "el" && child.key !== undefined;
    if (hasKey !== keyed) {
      throw new ViewError("MixedKeys", "siblings must be all keyed or all unkeyed");
    }
    if (keyed) {
      const id = keyId(child.key);
      if (seen.has(id)) throw new ViewError("DuplicateKey", `duplicate key ${String(child.key)}`);
      seen.add(id);
    }
  }
  return keyed;
}

// Longest increasing subsequence of `seq`, ignoring entries equal to -1.
// Returns the positions in `seq` that belong to it.
function lisPositions(seq) {
  const tails = [];
  const prev = new Array(seq.length).fill(-1);
  for (let i = 0; i < seq.length; i++) {
    const v = seq[i];
    if (v < 0) continue;
    let lo = 0;
    let hi = tails.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (seq[tails[mid]] < v) lo = mid + 1;
      else hi = mid;
    }
    if (lo > 0) prev[i] = tails[lo - 1];
    tails[lo] = i;
  }
  const out = new Set();
  let k = tails.length ? tails[tails.length - 1] : -1;
  while (k >= 0) {
    out.add(k);
    k = prev[k];
  }
  return out;
}

/**
 * Creates a renderer bound to a document and to the attribute sinks.
 * sinks.setAttr(element, name, value) and sinks.removeAttr(element, name) are
 * the only way attributes reach the DOM; the safe sinks (runtime/js/view/safe/)
 * enforce the attribute allowlist and URL rules.
 */
export function createRenderer(doc, sinks) {
  if (!doc || typeof doc.createElement !== "function" || typeof doc.createTextNode !== "function") {
    throw new ViewError("Document", "renderer needs a document");
  }
  if (!sinks || typeof sinks.setAttr !== "function" || typeof sinks.removeAttr !== "function") {
    throw new ViewError("Sinks", "renderer needs setAttr and removeAttr sinks");
  }

  // element -> { handlers, listeners }: one listener per event, dispatching to
  // the handler of the current virtual node, so patches never re-register.
  const events = new WeakMap();

  function setEvents(element, on, previous) {
    let state = events.get(element);
    if (!state) {
      state = { handlers: Object.create(null), listeners: Object.create(null) };
      events.set(element, state);
    }
    for (const name of Object.keys(previous)) {
      if (!Object.hasOwn(on, name)) {
        element.removeEventListener(name, state.listeners[name]);
        delete state.listeners[name];
        delete state.handlers[name];
      }
    }
    for (const name of Object.keys(on)) {
      state.handlers[name] = on[name];
      if (!state.listeners[name]) {
        const listener = (event) => state.handlers[name](event);
        state.listeners[name] = listener;
        element.addEventListener(name, listener);
      }
    }
  }

  function create(vnode) {
    if (vnode.node) throw new ViewError("VnodeReused", "virtual node is already mounted");
    if (vnode.kind === "text") {
      vnode.node = doc.createTextNode(vnode.text);
      return vnode.node;
    }
    const element = doc.createElement(vnode.tag);
    for (const name of Object.keys(vnode.attrs)) sinks.setAttr(element, name, vnode.attrs[name]);
    setEvents(element, vnode.on, {});
    checkSiblings(vnode.children);
    for (const child of vnode.children) element.appendChild(create(child));
    vnode.node = element;
    return element;
  }

  function same(a, b) {
    if (a.kind !== b.kind) return false;
    if (a.kind === "text") return true;
    return a.tag === b.tag && a.key === b.key;
  }

  function patchNode(parent, oldV, newV) {
    if (oldV === newV) return;
    if (!same(oldV, newV)) {
      parent.insertBefore(create(newV), oldV.node);
      parent.removeChild(oldV.node);
      return;
    }
    if (newV.node) throw new ViewError("VnodeReused", "virtual node is already mounted");
    const node = oldV.node;
    newV.node = node;
    if (newV.kind === "text") {
      if (oldV.text !== newV.text) node.data = newV.text;
      return;
    }
    for (const name of Object.keys(oldV.attrs)) {
      if (!Object.hasOwn(newV.attrs, name)) sinks.removeAttr(node, name);
    }
    for (const name of Object.keys(newV.attrs)) {
      const value = newV.attrs[name];
      if (oldV.attrs[name] !== value) sinks.setAttr(node, name, value);
    }
    setEvents(node, newV.on, oldV.on);
    patchChildren(node, oldV.children, newV.children);
  }

  function patchChildren(parent, oldKids, newKids) {
    const keyed = checkSiblings(newKids);
    if (keyed && oldKids.length > 0 && oldKids[0].kind === "el" && oldKids[0].key !== undefined) {
      patchKeyed(parent, oldKids, newKids);
      return;
    }
    const common = Math.min(oldKids.length, newKids.length);
    for (let i = 0; i < common; i++) patchNode(parent, oldKids[i], newKids[i]);
    for (let i = common; i < newKids.length; i++) parent.appendChild(create(newKids[i]));
    for (let i = common; i < oldKids.length; i++) parent.removeChild(oldKids[i].node);
  }

  function patchKeyed(parent, oldKids, newKids) {
    const oldIndex = new Map();
    oldKids.forEach((child, i) => oldIndex.set(keyId(child.key), i));
    const used = new Array(oldKids.length).fill(false);
    // For each new child: index of the reused old child, or -1 for a new node.
    const sources = newKids.map((child) => {
      const i = oldIndex.get(keyId(child.key));
      if (i === undefined || oldKids[i].tag !== child.tag) return -1;
      used[i] = true;
      return i;
    });
    for (let i = 0; i < oldKids.length; i++) {
      if (!used[i]) parent.removeChild(oldKids[i].node);
    }
    const stay = lisPositions(sources);
    let anchor = null;
    for (let j = newKids.length - 1; j >= 0; j--) {
      const child = newKids[j];
      const src = sources[j];
      if (src < 0) {
        parent.insertBefore(create(child), anchor);
      } else {
        patchNode(parent, oldKids[src], child);
        if (!stay.has(j)) parent.insertBefore(child.node, anchor);
      }
      anchor = child.node;
    }
  }

  return {
    /** Appends the DOM for `vnode` to `parent`; returns `vnode`. */
    mount(parent, vnode) {
      parent.appendChild(create(vnode));
      return vnode;
    },
    /** Updates the DOM of `oldV` under `parent` to match `newV`; returns `newV`. */
    patch(parent, oldV, newV) {
      patchNode(parent, oldV, newV);
      return newV;
    },
    /** Removes the DOM of `vnode` from `parent`. */
    unmount(parent, vnode) {
      parent.removeChild(vnode.node);
    },
  };
}
