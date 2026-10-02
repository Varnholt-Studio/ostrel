import assert from "node:assert/strict";
import test from "node:test";

import { createRenderer, el } from "../core.js";
import { FakeDocument, plainSinks, show } from "../testing/fake_dom.js";
import { createPatchTimer, percentile, VIEW_PATCH_BUDGET_MS } from "./patch_timer.js";
import { compareSortKeys, ListError, SortedIndex } from "./sorted_index.js";
import { createStyleSizer, createVirtualList } from "./virtual_list.js";
import { computeWindow } from "./window.js";

// Deterministic generator (mulberry32) so every failure reproduces from its seed.
function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function bruteOrder(keys) {
  return [...keys.entries()]
    .sort(([ia, ka], [ib, kb]) => compareSortKeys(ka, kb) || (ia < ib ? -1 : ia > ib ? 1 : 0))
    .map(([id]) => id);
}

function idOf(i) {
  return "r" + String(i).padStart(6, "0");
}

// Recording sizer for the fake DOM, which has no CSSOM.
function recordingSizer() {
  const heights = new Map();
  return {
    heights,
    setBlockSize(element, px) {
      heights.set(element, px);
    },
  };
}

function setup({ count, rowHeight = 20, viewportHeight = 400, overscan = 2, timer } = {}) {
  const doc = new FakeDocument();
  const root = doc.createElement("main");
  const renderer = createRenderer(doc, plainSinks);
  const sizer = recordingSizer();
  const index = new SortedIndex();
  const content = new Map();
  index.apply(Array.from({ length: count }, (_, i) => [idOf(i), i]));
  for (let i = 0; i < count; i++) content.set(idOf(i), `title ${i}`);
  const renders = [];
  const renderRow = (id) => {
    renders.push(id);
    return el("span", {}, [content.get(id)]);
  };
  const list = createVirtualList(renderer, sizer, {
    index,
    renderRow,
    rowHeight,
    viewportHeight,
    overscan,
    label: "Issues",
    timer,
  });
  list.mount(root);
  return { doc, root, sizer, index, content, renders, list };
}

function rowTexts(list) {
  return list.listElement().childNodes.map((n) => n.textContent);
}

function spacerHeights(t) {
  const [top, , bottom] = t.root.childNodes[0].childNodes;
  return [t.sizer.heights.get(top), t.sizer.heights.get(bottom)];
}

// Sorted index

test("sorted index orders strings by code point, not by UTF-16 units", () => {
  const index = new SortedIndex();
  index.apply([
    ["a", "\u{1F600}"],
    ["b", "！"],
    ["c", "Z"],
  ]);
  assert.deepEqual([index.at(0), index.at(1), index.at(2)], ["c", "b", "a"]);
});

test("sorted index breaks key ties by id and orders numbers, strings, arrays", () => {
  assert.ok(compareSortKeys(2, 10) < 0);
  assert.ok(compareSortKeys(10, "1") < 0);
  assert.ok(compareSortKeys("z", [0]) < 0);
  assert.ok(compareSortKeys([1, "b"], [1, "c"]) < 0);
  assert.ok(compareSortKeys([1], [1, 0]) < 0);
  const index = new SortedIndex();
  index.apply([
    ["y", 1],
    ["x", 1],
  ]);
  assert.deepEqual(index.ids, ["x", "y"]);
});

test("sorted index finds rows by indexed key when one batch swaps their keys", () => {
  const index = new SortedIndex();
  index.apply([
    ["a", 1],
    ["b", 2],
    ["c", 3],
  ]);
  const result = index.apply([
    ["a", 3],
    ["c", 1],
  ]);
  assert.deepEqual(index.ids, ["c", "b", "a"]);
  assert.deepEqual(result, { moved: true, first: 0 });
  assert.equal(index.indexOf("a"), 2);
  assert.equal(index.keyOf("c"), 1);
});

test("sorted index stays equal to a full sort over 500 random multi row batches", () => {
  const next = rng(78);
  const index = new SortedIndex();
  const model = new Map();
  for (let round = 0; round < 500; round++) {
    const batch = new Map();
    const size = 1 + Math.floor(next() * 12);
    for (let i = 0; i < size; i++) {
      const id = idOf(Math.floor(next() * 60));
      const roll = next();
      // Few distinct keys, so many rows share a key and ties matter.
      const key = roll < 0.15 ? undefined : roll < 0.6 ? Math.floor(next() * 8) : ["k", Math.floor(next() * 4)];
      batch.set(id, key);
    }
    const before = index.ids.slice();
    const result = index.apply(batch);
    for (const [id, key] of batch) {
      if (key === undefined) model.delete(id);
      else model.set(id, key);
    }
    const expected = bruteOrder(model);
    assert.deepEqual(index.ids, expected, `round ${round}`);
    for (const id of expected) assert.equal(index.indexOf(id), expected.indexOf(id));
    const firstDiff = expected.findIndex((id, i) => before[i] !== id);
    const changed = firstDiff >= 0 || before.length !== expected.length;
    if (changed) {
      assert.ok(result.moved, `round ${round}: change not reported`);
      const lowest = firstDiff >= 0 ? firstDiff : Math.min(before.length, expected.length);
      assert.ok(result.first <= lowest, `round ${round}: first ${result.first} > ${lowest}`);
    }
  }
});

test("sorted index validates the whole batch before the first change", () => {
  const index = new SortedIndex();
  index.apply([
    ["a", 1],
    ["b", 2],
  ]);
  const bad = [
    [
      ["b", 0],
      ["a", Number.NaN],
    ],
    [
      ["b", 0],
      ["a", { x: 1 }],
    ],
    [
      ["b", 0],
      ["a", [[1]]],
    ],
    [
      ["b", 0],
      [7, 1],
    ],
    [
      ["b", 0],
      ["b", 3],
    ],
  ];
  const codes = [];
  for (const batch of bad) {
    assert.throws(
      () => index.apply(batch),
      (e) => e instanceof ListError && codes.push(e.code) > 0,
    );
    assert.deepEqual(index.ids, ["a", "b"]);
  }
  assert.deepEqual(codes, ["KeyType", "KeyType", "KeyType", "IdType", "DuplicateUpdate"]);
});

test("sorted index reports no move when keys stay equal", () => {
  const index = new SortedIndex();
  index.apply([["a", 1]]);
  assert.deepEqual(index.apply([["a", 1]]), { moved: false, first: -1 });
  assert.deepEqual(index.apply([["missing", undefined]]), { moved: false, first: -1 });
});

// Window

test("window covers the viewport plus overscan and clamps the scroll position", () => {
  const base = { count: 100, rowHeight: 10, viewportHeight: 50, overscan: 2 };
  assert.deepEqual(computeWindow({ ...base, scrollTop: 0 }), { start: 0, end: 7, before: 0, after: 930 });
  assert.deepEqual(computeWindow({ ...base, scrollTop: 205 }), {
    start: 18,
    end: 28,
    before: 180,
    after: 720,
  });
  assert.deepEqual(computeWindow({ ...base, scrollTop: 1e9 }), {
    start: 93,
    end: 100,
    before: 930,
    after: 0,
  });
  assert.deepEqual(computeWindow({ ...base, scrollTop: -5 }), computeWindow({ ...base, scrollTop: 0 }));
  assert.deepEqual(computeWindow({ ...base, count: 0, scrollTop: 30 }), {
    start: 0,
    end: 0,
    before: 0,
    after: 0,
  });
  assert.deepEqual(computeWindow({ ...base, viewportHeight: 0, overscan: 0, scrollTop: 0 }), {
    start: 0,
    end: 1,
    before: 0,
    after: 990,
  });
});

test("window rejects invalid geometry", () => {
  const base = { count: 10, rowHeight: 10, viewportHeight: 50, overscan: 0, scrollTop: 0 };
  for (const patch of [
    { rowHeight: 0 },
    { rowHeight: 1.5 },
    { count: -1 },
    { viewportHeight: Number.NaN },
    { overscan: -1 },
    { scrollTop: Number.POSITIVE_INFINITY },
    { scrollTop: "0" },
  ]) {
    assert.throws(() => computeWindow({ ...base, ...patch }), (e) => e.code === "Window");
  }
});

// Virtual list

test("virtual list renders only the window and sizes the spacers for the rest", () => {
  const t = setup({ count: 10000 });
  // 400 px viewport, 20 px rows: 20 visible rows plus 2 overscan below.
  assert.equal(t.list.renderedIds().length, 22);
  assert.equal(t.renders.length, 22);
  assert.deepEqual(spacerHeights(t), [0, (10000 - 22) * 20]);
  assert.equal(rowTexts(t.list)[0], "title 0");
  const first = t.list.listElement().childNodes[0];
  assert.equal(
    show(first),
    '<div role="listitem" aria-posinset="1" aria-setsize="10000"><span>"title 0"</span></div>',
  );
  assert.equal(t.list.listElement().getAttribute("aria-label"), "Issues");
});

test("virtual list scrolls by moving one row, without rendering kept rows again", () => {
  const t = setup({ count: 10000 });
  t.list.update({ scrollTop: 2000 });
  t.renders.length = 0;
  t.doc.resetOps();
  t.list.update({ scrollTop: 2020 });
  assert.deepEqual(t.renders, [idOf(122)]);
  assert.equal(t.doc.ops.remove, 1);
  // One new row (wrapper, content element, text node), no other node created.
  assert.equal(t.doc.ops.create, 3);
  assert.equal(rowTexts(t.list)[0], "title 99");
  assert.deepEqual(spacerHeights(t), [99 * 20, (10000 - 123) * 20]);
});

test("virtual list patches one text node for a visible content change", () => {
  const t = setup({ count: 10000 });
  t.content.set(idOf(5), "renamed");
  t.renders.length = 0;
  t.doc.resetOps();
  t.list.update({ changed: [idOf(5)] });
  assert.deepEqual(t.renders, [idOf(5)]);
  assert.deepEqual(t.doc.ops, { create: 0, insert: 0, remove: 0, text: 1, attr: 0, listen: 0 });
  assert.equal(rowTexts(t.list)[5], "renamed");
});

test("virtual list touches no DOM for changes outside the window", () => {
  const t = setup({ count: 10000 });
  t.content.set(idOf(5000), "renamed");
  // Swap two rows far below the window: order changes, membership does not.
  t.index.apply([
    [idOf(6000), 7000.5],
    [idOf(7000), 6000.5],
  ]);
  t.renders.length = 0;
  t.doc.resetOps();
  t.list.update({ changed: [idOf(5000)] });
  assert.deepEqual(t.renders, []);
  assert.deepEqual(t.doc.ops, { create: 0, insert: 0, remove: 0, text: 0, attr: 0, listen: 0 });
});

test("virtual list moves visible rows on reorder and keeps their DOM nodes", () => {
  const t = setup({ count: 10000 });
  const before = new Map(t.list.listElement().childNodes.map((n) => [n.textContent, n]));
  t.index.apply([
    [idOf(3), -1],
    [idOf(10), 2.5],
  ]);
  t.renders.length = 0;
  t.doc.resetOps();
  t.list.update({});
  assert.deepEqual(t.renders, []);
  assert.equal(t.doc.ops.create, 0);
  assert.ok(t.doc.ops.insert <= 2, `inserts ${t.doc.ops.insert}`);
  const texts = rowTexts(t.list);
  assert.deepEqual(texts.slice(0, 5), ["title 3", "title 0", "title 1", "title 2", "title 10"]);
  for (const node of t.list.listElement().childNodes) {
    assert.equal(before.get(node.textContent), node, `node of ${node.textContent} replaced`);
  }
});

test("virtual list DOM work does not grow with the length of the list", () => {
  const ops = [];
  for (const count of [1000, 100000]) {
    const t = setup({ count });
    t.list.update({ scrollTop: 400 });
    // Insert a row above the window, remove one inside it, rename one inside it.
    t.index.apply([
      ["a-new", -1],
      [idOf(30), undefined],
    ]);
    t.content.set("a-new", "new");
    t.content.set(idOf(25), "renamed");
    t.doc.resetOps();
    t.list.update({ changed: [idOf(25)] });
    ops.push({ ...t.doc.ops });
    assert.equal(t.list.renderedIds().length, 24);
  }
  // aria-setsize differs in digits only; every count matches.
  assert.deepEqual(ops[0], ops[1]);
});

test("virtual list matches a full render after 300 random batches", () => {
  const next = rng(300);
  const t = setup({ count: 2000, overscan: 3 });
  const keys = new Map(Array.from({ length: 2000 }, (_, i) => [idOf(i), i]));
  let fresh = 2000;
  for (let round = 0; round < 300; round++) {
    const batch = new Map();
    const changed = [];
    for (let i = 0; i < 8; i++) {
      const id = idOf(Math.floor(next() * fresh));
      const roll = next();
      if (roll < 0.1) batch.set(id, undefined);
      else if (roll < 0.5) batch.set(id, Math.floor(next() * 2000));
      else if (roll < 0.6) {
        const added = idOf(fresh++);
        t.content.set(added, `title ${added}`);
        batch.set(added, Math.floor(next() * 2000));
      } else {
        t.content.set(id, `edit ${round} ${i}`);
        changed.push(id);
      }
    }
    t.index.apply(batch);
    for (const [id, key] of batch) {
      if (key === undefined) keys.delete(id);
      else keys.set(id, key);
    }
    const scrollTop = next() < 0.3 ? Math.floor(next() * 2000 * 20) : undefined;
    t.list.update({ changed, scrollTop });
    const order = bruteOrder(keys);
    const ids = t.list.renderedIds();
    const start = order.indexOf(ids[0]);
    assert.deepEqual(ids, order.slice(start, start + ids.length), `round ${round}`);
    assert.deepEqual(
      rowTexts(t.list),
      ids.map((id) => t.content.get(id)),
      `round ${round}`,
    );
    const [top, bottom] = spacerHeights(t);
    assert.equal(top + bottom + ids.length * 20, order.length * 20);
  }
});

test("virtual list keeps the mounted DOM when renderRow fails", () => {
  const t = setup({ count: 100 });
  const before = show(t.root);
  t.content.set(idOf(4), { not: "a child" });
  assert.throws(() => t.list.update({ changed: [idOf(4)] }), (e) => e.code === "ChildType");
  assert.equal(show(t.root), before);
  assert.throws(() => t.list.update({ changed: [4] }), (e) => e.code === "IdType");
  assert.throws(() => t.list.update({ scrollTop: Number.NaN }), (e) => e.code === "Window");
  assert.equal(show(t.root), before);
});

test("virtual list rejects bad options and rows that are not elements", () => {
  const doc = new FakeDocument();
  const renderer = createRenderer(doc, plainSinks);
  const sizer = recordingSizer();
  const index = new SortedIndex();
  index.apply([["a", 1]]);
  const base = { index, renderRow: () => el("span", {}, ["x"]), rowHeight: 10, viewportHeight: 10 };
  assert.throws(() => createVirtualList(renderer, sizer, { ...base, index: [] }), (e) => e.code === "Options");
  assert.throws(() => createVirtualList(renderer, {}, base), (e) => e.code === "Options");
  assert.throws(() => createVirtualList(renderer, sizer, { ...base, rowHeight: 0 }), (e) => e.code === "Window");
  const textRow = createVirtualList(renderer, sizer, { ...base, renderRow: () => "x" });
  assert.throws(() => textRow.mount(doc.createElement("main")), (e) => e.code === "RowType");
  const list = createVirtualList(renderer, sizer, base);
  assert.throws(() => list.update({}), (e) => e.code === "Mounted");
  const host = doc.createElement("main");
  list.mount(host);
  assert.throws(() => list.mount(host), (e) => e.code === "Mounted");
  list.unmount();
  assert.equal(host.childNodes.length, 0);
  list.mount(host);
  assert.equal(host.textContent, "x");
});

test("style sizer writes whole pixel heights once and refuses anything else", () => {
  const writes = [];
  const element = { style: { setProperty: (name, value) => writes.push([name, value]) } };
  const sizer = createStyleSizer();
  sizer.setBlockSize(element, 120);
  sizer.setBlockSize(element, 120);
  sizer.setBlockSize(element, 0);
  assert.deepEqual(writes, [
    ["height", "120px"],
    ["height", "0px"],
  ]);
  for (const bad of [-1, 1.5, Number.NaN, "10", 2 ** 31]) {
    assert.throws(() => sizer.setBlockSize(element, bad), (e) => e.code === "BlockSize");
  }
  assert.equal(writes.length, 2);
});

// Patch measurement

test("percentile uses the nearest rank", () => {
  const samples = Array.from({ length: 100 }, (_, i) => 100 - i);
  assert.equal(percentile(samples, 99), 99);
  assert.equal(percentile(samples, 100), 100);
  assert.equal(percentile([5], 99), 5);
  assert.ok(Number.isNaN(percentile([], 99)));
  assert.throws(() => percentile([1], 0), (e) => e.code === "Percentile");
});

test("patch timer records one sample per update and bounds its memory", () => {
  let clock = 0;
  const timer = createPatchTimer({ now: () => clock, capacity: 3 });
  for (const ms of [1, 30, 2, 4]) timer.time(() => (clock += ms));
  assert.deepEqual(timer.samples(), [30, 2, 4]);
  assert.equal(timer.dropped, 1);
  assert.equal(timer.overBudget(), 1);
  assert.equal(timer.p99(), 30);
  assert.throws(() => timer.time(() => {
    throw new Error("boom");
  }));
  assert.equal(timer.samples().length, 3);
  timer.reset();
  assert.deepEqual(timer.samples(), []);

  const t = setup({ count: 100, timer: createPatchTimer() });
  t.list.update({ scrollTop: 40 });
  t.list.update({ changed: [idOf(3)] });
  assert.equal(t.list.renderedIds().length > 0, true);
});

// CPU time of this process in milliseconds. The build machine is shared and often
// overloaded, so wall time measures the scheduler more than the patch (TIGER-1 review of
// T33: frame wait depends on load). The official wall clock number comes from bench/.
function cpuNow() {
  const used = process.cpuUsage();
  return (used.user + used.system) / 1000;
}

test("view patch on 10 000 rows stays under 20 ms CPU time at p99 (fake DOM, not a KPI value)", (ctx) => {
  const timer = createPatchTimer({ now: cpuNow });
  const wall = createPatchTimer();
  const t = setup({ count: 10000, viewportHeight: 800, overscan: 5, timer });
  const next = rng(20);
  let fresh = 10000;
  for (let round = 0; round < 400; round++) {
    const batch = new Map();
    const changed = [];
    for (let i = 0; i < 6; i++) {
      const id = idOf(Math.floor(next() * fresh));
      if (next() < 0.5) batch.set(id, Math.floor(next() * 10000));
      else {
        t.content.set(id, `edit ${round}`);
        changed.push(id);
      }
    }
    if (next() < 0.1) {
      const added = idOf(fresh++);
      t.content.set(added, "new");
      batch.set(added, Math.floor(next() * 10000));
    }
    t.index.apply(batch);
    const scrollTop = next() < 0.2 ? Math.floor(next() * 200000) : undefined;
    wall.time(() => t.list.update({ changed, scrollTop }));
  }
  assert.equal(timer.samples().length, 400);
  const cpuP99 = timer.p99();
  ctx.diagnostic(`view p99 cpu ${cpuP99.toFixed(3)} ms, wall ${wall.p99().toFixed(3)} ms`);
  assert.ok(cpuP99 < VIEW_PATCH_BUDGET_MS, `view p99 cpu ${cpuP99} ms`);
});
