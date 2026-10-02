import { decodeOps } from './decode.mjs';

// One incoming `Ops` message through the four budget steps of ARCHITECTURE 5.8.
// Returns the time of each step in milliseconds.
export function handleMessage(text, store, board, now) {
  const t0 = now();
  const ops = decodeOps(text);
  const t1 = now();
  const touched = store.apply(ops);
  const t2 = now();
  const changes = store.invalidate(touched);
  const t3 = now();
  board.update(changes, touched);
  const t4 = now();
  return { decode: t1 - t0, merge: t2 - t1, query: t3 - t2, view: t4 - t3, ops: ops.length };
}
