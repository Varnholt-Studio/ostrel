// Reference model of the spike's sequence CRDT, used only as a test oracle. It shares no
// code with seq.mjs: elements form a tree (each element is a child of its origin, siblings
// in descending id order) and the text is the pre-order walk of that tree. Under the
// Lamport condition checked in decode (counter above origin) this is the RGA order that
// seq.mjs computes with a linear scan, so a mismatch points at a bug in one of the two.
const ROOT = '';

export class RgaReference {
  // `text` is a snapshot whose code points are elements 1..n of `seedReplica`, each one
  // inserted after the previous.
  constructor(text, seedReplica) {
    this.nodes = new Map([[ROOT, { ch: '', del: true, kids: [] }]]);
    let prev = ROOT;
    let counter = 0;
    for (const ch of text) {
      counter++;
      const id = counter.toString(16).padStart(8, '0') + seedReplica;
      this.nodes.set(id, { ch, del: false, kids: [] });
      this.nodes.get(prev).kids.push(id);
      prev = id;
    }
  }

  insert(after, id, ch) {
    if (this.nodes.has(id)) return;
    const parent = this.nodes.get(after === null ? ROOT : after);
    if (!parent) throw new Error('reference: unknown origin ' + after);
    this.nodes.set(id, { ch, del: false, kids: [] });
    parent.kids.push(id);
  }

  remove(id) {
    const node = this.nodes.get(id);
    if (!node) throw new Error('reference: unknown element ' + id);
    node.del = true;
  }

  // Applies wire ops of the spike (`ins` with origin and counter, `del` with target).
  applyOps(ops) {
    for (const op of ops) {
      if (op.k === 'ins') {
        const id = op.c.toString(16).padStart(8, '0') + op.id.slice(0, 16);
        this.insert(op.after, id, op.s);
      } else if (op.k === 'del') {
        this.remove(op.at);
      }
    }
  }

  text() {
    let out = '';
    const stack = [ROOT];
    while (stack.length) {
      const id = stack.pop();
      const node = this.nodes.get(id);
      if (!node.del) out += node.ch;
      // Children in descending id order are visited first, so push them ascending.
      const kids = [...node.kids].sort();
      for (const k of kids) stack.push(k);
    }
    return out;
  }
}
