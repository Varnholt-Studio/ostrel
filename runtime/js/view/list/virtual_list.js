// Virtualised list (ARCHITECTURE 5.8: "views patch only changed nodes and virtualise long
// lists"). Only the rows inside the visible window, plus a few rows of overscan, have DOM
// nodes; two spacer elements stand for the rows above and below. A patch therefore costs
// work in proportion to the window, not to the length of the list.
//
// Row content is rendered once per row while the row stays in the window. A row is rendered
// again only when its id is passed in `changed`; otherwise its virtual node is reused, and
// the view core skips it without diffing (core.js patchNode, identical nodes).
//
// Spacer heights are the only layout values written outside the attribute sinks. Inline
// style attributes are not on the allowlist (safe/attrs.js), so the sizer writes the single
// CSS property `height` through the CSSOM, and only as a whole number of pixels.

import { el } from "../core.js";
import { ListError, SortedIndex } from "./sorted_index.js";
import { computeWindow } from "./window.js";

/** Largest spacer height the sizer writes, in pixels. */
export const MAX_BLOCK_SIZE = 2 ** 31 - 1;

/** Sizer for browsers: writes `height: <n>px` through the CSSOM, skips unchanged values. */
export function createStyleSizer() {
  const written = new WeakMap();
  return {
    setBlockSize(element, px) {
      if (!Number.isSafeInteger(px) || px < 0 || px > MAX_BLOCK_SIZE) {
        throw new ListError("BlockSize", "block size must be a whole number of pixels");
      }
      if (written.get(element) === px) return;
      element.style.setProperty("height", `${px}px`);
      written.set(element, px);
    },
  };
}

function checkOptions(options) {
  if (!options || !(options.index instanceof SortedIndex)) {
    throw new ListError("Options", "index must be a SortedIndex");
  }
  if (typeof options.renderRow !== "function") {
    throw new ListError("Options", "renderRow must be a function");
  }
}

/**
 * Creates a virtualised list over `options.index`.
 *   renderer   from createRenderer (core.js)
 *   sizer      { setBlockSize(element, px) }, createStyleSizer() in browsers
 *   options    { index, renderRow(id) -> element vnode, rowHeight, viewportHeight,
 *                overscan = 4, label?, timer? }
 * `timer` (patch_timer.js) records the duration of every update that patches the DOM.
 */
export function createVirtualList(renderer, sizer, options) {
  if (!renderer || typeof renderer.patch !== "function" || typeof renderer.mount !== "function") {
    throw new ListError("Options", "renderer must come from createRenderer");
  }
  if (!sizer || typeof sizer.setBlockSize !== "function") {
    throw new ListError("Options", "sizer must have setBlockSize");
  }
  checkOptions(options);
  const { index, renderRow, rowHeight, label, timer } = options;
  const overscan = options.overscan ?? 4;
  let viewportHeight = options.viewportHeight;
  let scrollTop = 0;
  // Checks the geometry once, before anything is mounted.
  computeWindow({ count: 0, rowHeight, viewportHeight, scrollTop, overscan });
  // Mounted tree, its parent element, and id -> row content vnode currently mounted.
  let tree = null;
  let parent = null;
  let rows = new Map();

  function renderContent(id) {
    const vnode = renderRow(id);
    if (!vnode || vnode.kind !== "el") {
      throw new ListError("RowType", `renderRow(${JSON.stringify(id)}) must return an element`);
    }
    return vnode;
  }

  // Builds the next tree. Returns it with the row map that belongs to it; nothing is
  // committed until the patch succeeded.
  function build(stale) {
    const win = computeWindow({ count: index.size, rowHeight, viewportHeight, scrollTop, overscan });
    const next = new Map();
    const items = [];
    const setSize = String(index.size);
    for (let i = win.start; i < win.end; i++) {
      const id = index.at(i);
      let content = stale.has(id) ? undefined : rows.get(id);
      if (content === undefined) content = renderContent(id);
      next.set(id, content);
      const attrs = { role: "listitem", "aria-posinset": String(i + 1), "aria-setsize": setSize };
      items.push(el("div", { key: id, attrs }, [content]));
    }
    const listAttrs = { role: "list" };
    if (label !== undefined) listAttrs["aria-label"] = String(label);
    const root = el("div", { attrs: { class: "ostrel-vlist" } }, [
      el("div", { attrs: { "aria-hidden": "true" } }, []),
      el("div", { attrs: listAttrs }, items),
      el("div", { attrs: { "aria-hidden": "true" } }, []),
    ]);
    return { root, next, win };
  }

  function size(win) {
    sizer.setBlockSize(tree.children[0].node, win.before);
    sizer.setBlockSize(tree.children[2].node, win.after);
  }

  function readIds(changed) {
    const stale = new Set();
    for (const id of changed ?? []) {
      if (typeof id !== "string") throw new ListError("IdType", "row id must be a string");
      stale.add(id);
    }
    return stale;
  }

  function run(fn) {
    return timer ? timer.time(fn) : fn();
  }

  return {
    /** Mounts the list under `target`. */
    mount(target) {
      if (tree) throw new ListError("Mounted", "list is already mounted");
      const { root, next, win } = build(new Set());
      renderer.mount(target, root);
      tree = root;
      parent = target;
      rows = next;
      size(win);
    },

    /**
     * Brings the DOM up to date with the index. `changed` lists ids whose row content must
     * be rendered again; `scrollTop` and `viewportHeight` move or resize the window. Call it
     * after every index batch, scroll or resize.
     */
    update({ changed, scrollTop: top, viewportHeight: height } = {}) {
      if (!tree) throw new ListError("Mounted", "list is not mounted");
      const stale = readIds(changed);
      const nextTop = top ?? scrollTop;
      const nextHeight = height ?? viewportHeight;
      computeWindow({ count: 0, rowHeight, viewportHeight: nextHeight, scrollTop: nextTop, overscan });
      scrollTop = nextTop;
      viewportHeight = nextHeight;
      run(() => {
        const { root, next, win } = build(stale);
        renderer.patch(parent, tree, root);
        tree = root;
        rows = next;
        size(win);
      });
    },

    /** Removes the list from the DOM. */
    unmount() {
      if (!tree) throw new ListError("Mounted", "list is not mounted");
      renderer.unmount(parent, tree);
      tree = null;
      parent = null;
      rows = new Map();
    },

    /** Ids that currently have DOM nodes, in list order. */
    renderedIds() {
      return [...rows.keys()];
    },

    /** The list element (role list) that holds the row elements, or null. */
    listElement() {
      return tree ? tree.children[1].node : null;
    },
  };
}
