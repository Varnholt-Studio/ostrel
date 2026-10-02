import { isHex, WIDTH } from './ids.mjs';
import { FIELDS, MAX_OPS_PER_BATCH, MAX_REMOVE_TAGS, MAX_TEXT_BYTES } from './schema.mjs';

// Step 1 of the KPI A budget: parse and validate one incoming `Ops` message. Every
// message is hostile input; anything malformed throws and nothing is applied.
// The wire shape is a spike assumption until `ostrel_sync::protocol` is fixed (F1b).
export class DecodeError extends Error {}

function fail(msg) {
  throw new DecodeError(msg);
}

function checkText(v) {
  if (typeof v !== 'string') fail('text expected');
  if (v.length * 3 > MAX_TEXT_BYTES && new TextEncoder().encode(v).length > MAX_TEXT_BYTES) {
    fail('text too large');
  }
  for (let i = 0; i < v.length; i++) {
    const c = v.charCodeAt(i);
    if (c >= 0xd800 && c <= 0xdfff) {
      const hi = c <= 0xdbff;
      const next = i + 1 < v.length ? v.charCodeAt(i + 1) : 0;
      if (!hi || next < 0xdc00 || next > 0xdfff) fail('lone surrogate');
      i++;
    }
  }
}

function checkOp(op) {
  if (op === null || typeof op !== 'object' || Array.isArray(op)) fail('op object expected');
  if (!isHex(op.id, WIDTH.opId)) fail('bad op id');
  if (!isHex(op.hlc, WIDTH.hlc)) fail('bad hlc');
  if (op.hlc.slice(16) !== op.id.slice(0, 16)) fail('hlc replica differs from op replica');
  if (op.m !== 'Issue') fail('unknown model');
  if (!isHex(op.row, WIDTH.rowId)) fail('bad row id');
  const field = Object.prototype.hasOwnProperty.call(FIELDS, op.f) ? FIELDS[op.f] : null;
  if (!field) fail('unknown field');
  switch (op.k) {
    case 'set':
      if (field.kind === 'set' || field.kind === 'seq') fail('set on merged field');
      if (field.kind === 'enum') {
        if (!field.values.includes(op.v)) fail('bad enum value');
      } else {
        checkText(op.v);
        if (field.kind === 'rank' && !/^[0-9a-z]+$/.test(op.v)) fail('bad rank');
      }
      break;
    case 'sadd':
      if (field.kind !== 'set') fail('sadd on non set');
      checkText(op.e);
      break;
    case 'srem':
      if (field.kind !== 'set') fail('srem on non set');
      checkText(op.e);
      if (!Array.isArray(op.tags) || op.tags.length === 0 || op.tags.length > MAX_REMOVE_TAGS) {
        fail('bad tag list');
      }
      for (const t of op.tags) if (!isHex(t, WIDTH.opId)) fail('bad tag');
      break;
    case 'ins':
      if (field.kind !== 'seq') fail('ins on non text');
      if (op.after !== null && !isHex(op.after, WIDTH.seqId)) fail('bad origin');
      if (!Number.isInteger(op.c) || op.c < 1 || op.c > 0xffffffff) fail('bad counter');
      if (typeof op.s !== 'string') fail('bad char');
      checkText(op.s);
      if ([...op.s].length !== 1) fail('one code point per ins');
      break;
    case 'del':
      if (field.kind !== 'seq') fail('del on non text');
      if (!isHex(op.at, WIDTH.seqId)) fail('bad target');
      break;
    default:
      fail('unknown op kind');
  }
}

export function decodeOps(message) {
  if (typeof message !== 'string') fail('text frame expected');
  let msg;
  try {
    msg = JSON.parse(message);
  } catch {
    fail('invalid json');
  }
  if (msg === null || typeof msg !== 'object') fail('object expected');
  if (msg.v !== 0 || msg.t !== 'Ops') fail('unexpected message');
  if (!Array.isArray(msg.ops) || msg.ops.length > MAX_OPS_PER_BATCH) fail('bad batch');
  for (const op of msg.ops) checkOp(op);
  return msg.ops;
}
