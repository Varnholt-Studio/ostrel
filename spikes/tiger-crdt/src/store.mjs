import { seqId } from './ids.mjs';
import { OrSet } from './orset.mjs';
import { Seq } from './seq.mjs';
import { compareText } from './text.mjs';

const LWW = ['title', 'status', 'priority', 'assignee', 'rank'];

// Client replica of the issue model with incremental live queries.
// Step 2 (merge) is `apply`, step 3 (live query invalidation) is `invalidate`.
export class Store {
  constructor() {
    this.rows = new Map();
    this.applied = new Map(); // replica -> highest applied seq (contiguous per replica)
    this.queries = [];
  }

  load(issues, seedHlc, seedReplica) {
    for (const it of issues) {
      const row = { id: it.id };
      for (const f of LWW) row[f] = { v: it[f], h: seedHlc };
      row.labels = OrSet.fromSnapshot(it.labels.map((l) => [l.e, l.tag]));
      row.desc = new Seq(it.desc, seedReplica);
      this.rows.set(it.id, row);
    }
  }

  value(row, field) {
    if (field === 'labels') return row.labels.values();
    if (field === 'desc') return row.desc.text();
    return row[field].v;
  }

  // Applies decoded ops in order. Returns the touched rows and the fields that changed.
  apply(ops) {
    const touched = new Map();
    for (const op of ops) {
      const replica = op.id.slice(0, 16);
      const seq = parseInt(op.id.slice(16), 16);
      const last = this.applied.get(replica) || 0;
      if (seq <= last) continue; // idempotent replay
      this.applied.set(replica, seq);
      const row = this.rows.get(op.row);
      if (!row) continue; // unknown row: not in this replica's scope
      let t = touched.get(op.row);
      if (!t) {
        t = { row, fields: new Set() };
        touched.set(op.row, t);
      }
      let changed = false;
      switch (op.k) {
        case 'set': {
          const reg = row[op.f];
          if (op.hlc > reg.h) {
            changed = reg.v !== op.v;
            reg.v = op.v;
            reg.h = op.hlc;
          }
          break;
        }
        case 'sadd':
          changed = row.labels.add(op.e, op.id);
          break;
        case 'srem':
          changed = row.labels.remove(op.e, op.tags);
          break;
        case 'ins':
          changed = row.desc.insert(op.after, seqId(op.c, replica), op.s);
          break;
        case 'del':
          changed = row.desc.remove(op.at);
          break;
      }
      if (changed) t.fields.add(op.f);
    }
    return touched;
  }

  query(filterField, filterValue, sortField) {
    const q = new LiveQuery(filterField, filterValue, sortField);
    for (const row of this.rows.values()) if (q.matches(row[filterField].v)) q.items.push(row);
    q.items.sort((a, b) => q.compare(a[sortField].v, a.id, b[sortField].v, b.id));
    q.reindex();
    this.queries.push(q);
    return q;
  }

  invalidate(touched) {
    const out = [];
    for (const q of this.queries) out.push(q.update(touched));
    return out;
  }
}

export class LiveQuery {
  constructor(filterField, filterValue, sortField) {
    this.filterField = filterField;
    this.filterValue = filterValue;
    this.sortField = sortField;
    this.items = [];
    // Sort key each member was indexed under. Searches use these keys, never the live row
    // values: when one batch moves several rows, live values and array order disagree
    // until every row is processed.
    this.keys = new Map();
  }

  matches(v) {
    return v === this.filterValue;
  }

  compare(ka, ia, kb, ib) {
    return compareText(ka, kb) || compareText(ia, ib);
  }

  // First index whose indexed (key, id) is not less than (key, id).
  lowerBound(key, id) {
    let lo = 0;
    let hi = this.items.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      const r = this.items[mid];
      if (this.compare(this.keys.get(r.id), r.id, key, id) < 0) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  }

  reindex() {
    this.keys.clear();
    for (const r of this.items) this.keys.set(r.id, r[this.sortField].v);
  }

  // Incremental update: O(log n) search plus one array shift per moved row.
  update(touched) {
    const change = { query: this, moved: false, content: new Set() };
    const ff = this.filterField;
    const sf = this.sortField;
    for (const t of touched.values()) {
      if (t.fields.size === 0) continue;
      const row = t.row;
      const oldKey = this.keys.get(row.id);
      const wasIn = oldKey !== undefined;
      const isIn = this.matches(row[ff].v);
      const newKey = row[sf].v;
      if (wasIn && isIn && oldKey === newKey) {
        change.content.add(row.id);
        continue;
      }
      if (wasIn) {
        const at = this.lowerBound(oldKey, row.id);
        if (this.items[at] !== row) throw new Error('index out of sync for ' + row.id);
        this.items.splice(at, 1);
        this.keys.delete(row.id);
        change.moved = true;
      }
      if (isIn) {
        this.items.splice(this.lowerBound(newKey, row.id), 0, row);
        this.keys.set(row.id, newKey);
        change.moved = true;
        change.content.add(row.id);
      }
    }
    return change;
  }
}
