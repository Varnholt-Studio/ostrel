import { seqId, seqCounter } from './ids.mjs';

// Sequence CRDT for `Text merge text`, spike version: RGA with Lamport ordered element
// ids and tombstones. ARCHITECTURE 5.2 asks for a Fugue style tree with run length
// encoding; this spike measures the cost class (linear scans over at most a few thousand
// elements), not the final algorithm. A snapshot string stays a plain string until the
// first remote op touches it, which stands in for one run of the run length encoding.
// The expansion cost is paid inside the merge step and therefore counted.
export class Seq {
  constructor(text, seedReplica) {
    this.plain = text;
    this.seed = seedReplica;
    this.elems = null;
    this.ids = null;
    this.maxCounter = text.length;
    this.cache = text;
  }

  expand() {
    if (this.elems) return;
    const chars = Array.from(this.plain);
    this.elems = new Array(chars.length);
    this.ids = new Set();
    for (let i = 0; i < chars.length; i++) {
      const id = seqId(i + 1, this.seed);
      this.elems[i] = { id, ch: chars[i], del: false };
      this.ids.add(id);
    }
    this.maxCounter = Math.max(this.maxCounter, chars.length);
    this.plain = null;
  }

  indexOf(id) {
    const elems = this.elems;
    for (let i = 0; i < elems.length; i++) if (elems[i].id === id) return i;
    return -2;
  }

  // Inserts element `id` with character `ch` right after `after` (null = start).
  // Concurrent inserts at the same place are ordered by descending id (RGA).
  insert(after, id, ch) {
    this.expand();
    if (this.ids.has(id)) return false;
    let i = 0;
    if (after !== null) {
      const at = this.indexOf(after);
      if (at < 0) throw new Error('unknown origin ' + after);
      i = at + 1;
    }
    const elems = this.elems;
    while (i < elems.length && elems[i].id > id) i++;
    elems.splice(i, 0, { id, ch, del: false });
    this.ids.add(id);
    const c = seqCounter(id);
    if (c > this.maxCounter) this.maxCounter = c;
    this.cache = null;
    return true;
  }

  remove(id) {
    this.expand();
    const at = this.indexOf(id);
    if (at < 0) throw new Error('unknown element ' + id);
    if (this.elems[at].del) return false;
    this.elems[at].del = true;
    this.cache = null;
    return true;
  }

  text() {
    if (this.cache !== null) return this.cache;
    let s = '';
    for (const e of this.elems) if (!e.del) s += e.ch;
    this.cache = s;
    return s;
  }

  // Ids of the visible elements, used by editors to address positions.
  visibleIds() {
    this.expand();
    const out = [];
    for (const e of this.elems) if (!e.del) out.push(e.id);
    return out;
  }

  clone() {
    const c = new Seq('', this.seed);
    if (this.elems) {
      c.plain = null;
      c.elems = this.elems.map((e) => ({ id: e.id, ch: e.ch, del: e.del }));
      c.ids = new Set(this.ids);
      c.cache = null;
    } else {
      c.plain = this.plain;
      c.cache = this.plain;
    }
    c.maxCounter = this.maxCounter;
    return c;
  }
}
