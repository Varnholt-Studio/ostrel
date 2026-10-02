// Step 4 of the KPI A budget: view diff and DOM patch. Keyed, virtualised lists (only a
// window of rows has DOM nodes) and a detail panel. Nodes are patched only when the
// rendered string differs. Text goes through `textContent` only (no HTML sinks, 7.5).
const ROW_FIELDS = ['title', 'priority', 'assignee', 'labels'];

function cellText(store, row, field) {
  const v = store.value(row, field);
  return Array.isArray(v) ? v.join(', ') : v;
}

export class ListView {
  constructor(doc, parent, store, query, size) {
    this.doc = doc;
    this.store = store;
    this.query = query;
    this.size = size;
    this.start = 0;
    this.el = doc.createElement('ol');
    parent.appendChild(this.el);
    this.nodes = new Map(); // row id -> { el, cells, vals }
    this.reconcile();
  }

  visible() {
    const items = this.query.items;
    const start = Math.max(0, Math.min(this.start, items.length - this.size));
    return items.slice(start, start + this.size);
  }

  makeNode(row) {
    const el = this.doc.createElement('li');
    el.setAttribute('data-id', row.id);
    const cells = [];
    const vals = [];
    for (const f of ROW_FIELDS) {
      const c = this.doc.createElement('span');
      const v = cellText(this.store, row, f);
      c.textContent = v;
      el.appendChild(c);
      cells.push(c);
      vals.push(v);
    }
    return { el, cells, vals, row };
  }

  patchNode(rec) {
    for (let i = 0; i < ROW_FIELDS.length; i++) {
      const v = cellText(this.store, rec.row, ROW_FIELDS[i]);
      if (v !== rec.vals[i]) {
        rec.cells[i].textContent = v;
        rec.vals[i] = v;
      }
    }
  }

  reconcile() {
    const rows = this.visible();
    const keep = new Set();
    for (const r of rows) keep.add(r.id);
    for (const [id, rec] of this.nodes) {
      if (!keep.has(id)) {
        this.el.removeChild(rec.el);
        this.nodes.delete(id);
      }
    }
    let cursor = this.el.firstChild;
    for (const row of rows) {
      let rec = this.nodes.get(row.id);
      if (!rec) {
        rec = this.makeNode(row);
        this.nodes.set(row.id, rec);
      } else {
        this.patchNode(rec);
      }
      if (rec.el === cursor) cursor = cursor.nextSibling;
      else this.el.insertBefore(rec.el, cursor);
    }
  }

  // `change` is this view's query change from Store.invalidate.
  update(change) {
    if (change.moved) {
      this.reconcile();
      return;
    }
    for (const id of change.content) {
      const rec = this.nodes.get(id);
      if (rec) this.patchNode(rec);
    }
  }

  scrollTo(index) {
    this.start = index;
    this.reconcile();
  }
}

const DETAIL_FIELDS = ['title', 'status', 'priority', 'assignee', 'labels', 'desc'];

export class DetailView {
  constructor(doc, parent, store) {
    this.store = store;
    this.row = null;
    this.el = doc.createElement('section');
    this.cells = {};
    this.vals = {};
    for (const f of DETAIL_FIELDS) {
      const c = doc.createElement(f === 'desc' ? 'p' : 'div');
      c.setAttribute('data-field', f);
      this.el.appendChild(c);
      this.cells[f] = c;
      this.vals[f] = '';
    }
    parent.appendChild(this.el);
  }

  show(row) {
    this.row = row;
    this.patch();
  }

  patch() {
    if (!this.row) return;
    for (const f of DETAIL_FIELDS) {
      const v = cellText(this.store, this.row, f);
      if (v !== this.vals[f]) {
        this.cells[f].textContent = v;
        this.vals[f] = v;
      }
    }
  }

  update(touched) {
    if (this.row && touched.has(this.row.id)) this.patch();
  }

  rendered(field) {
    return this.cells[field].textContent;
  }
}

// One board column per status plus the detail panel, like the benchmark app.
export class Board {
  constructor(doc, root, store, statuses, windowSize) {
    this.columns = statuses.map((s) => {
      const q = store.query('status', s, 'rank');
      return new ListView(doc, root, store, q, windowSize);
    });
    this.detail = new DetailView(doc, root, store);
  }

  update(changes, touched) {
    for (let i = 0; i < this.columns.length; i++) this.columns[i].update(changes[i]);
    this.detail.update(touched);
  }
}
