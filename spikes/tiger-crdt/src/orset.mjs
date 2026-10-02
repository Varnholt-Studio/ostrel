import { compareText } from './text.mjs';

// Observed remove set with add tags (D49). An add creates one tag (its OpId), a remove
// names the tags it observed. At most one live tag per (element, replica). Delivery is in
// server order per row, so a remove never arrives before the add it observed.
export class OrSet {
  constructor() {
    this.tags = new Map(); // element -> Map(replica -> tag)
  }

  static fromSnapshot(entries) {
    const set = new OrSet();
    for (const [elem, tag] of entries) set.add(elem, tag);
    return set;
  }

  add(elem, tag) {
    let live = this.tags.get(elem);
    if (!live) {
      live = new Map();
      this.tags.set(elem, live);
    }
    const replica = tag.slice(0, 16);
    const old = live.get(replica);
    if (old !== undefined && old >= tag) return false;
    live.set(replica, tag);
    return true;
  }

  remove(elem, observed) {
    const live = this.tags.get(elem);
    if (!live) return false;
    let changed = false;
    for (const tag of observed) {
      const replica = tag.slice(0, 16);
      if (live.get(replica) === tag) {
        live.delete(replica);
        changed = true;
      }
    }
    if (live.size === 0) this.tags.delete(elem);
    return changed;
  }

  has(elem) {
    return this.tags.has(elem);
  }

  observed(elem) {
    const live = this.tags.get(elem);
    return live ? [...live.values()] : [];
  }

  // Iteration order of a Set is code point order of its elements (D50, D61), never the
  // UTF-16 order of a bare sort().
  values() {
    return [...this.tags.keys()].sort(compareText);
  }
}
