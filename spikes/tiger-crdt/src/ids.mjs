// Fixed width lowercase hex ids (ARCHITECTURE 5.3). String order equals value order.
const HEX = /^[0-9a-f]+$/;

export const WIDTH = { replica: 16, opId: 24, rowId: 32, hlc: 32, seqId: 24 };

export function isHex(s, width) {
  return typeof s === 'string' && s.length === width && HEX.test(s);
}

export function hex(n, width) {
  const s = n.toString(16);
  if (s.length > width) throw new RangeError('value too wide');
  return '0'.repeat(width - s.length) + s;
}

// Hlc: wall_ms (12) | counter (4) | replica (16).
export function hlc(wallMs, counter, replica) {
  return hex(wallMs, 12) + hex(counter, 4) + replica;
}

// OpId: replica (16) | seq (8).
export function opId(replica, seq) {
  return replica + hex(seq, 8);
}

// Spike only: id of one sequence element, Lamport counter (8) | replica (16).
// The counter comes first so that string order is the RGA tie order.
export function seqId(counter, replica) {
  return hex(counter, 8) + replica;
}

export function seqCounter(id) {
  return parseInt(id.slice(0, 8), 16);
}
