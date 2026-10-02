// Node sidecar host for `extern js server` modules (ARCHITECTURE 7.2).
//
// The server starts one host per server process:
//
//   node --permission --allow-fs-read=<app>/extern runtime/node/host.mjs \
//     --module <name>=<absolute path> [--module <name>=<absolute path> ...]
//
// Protocol: JSON-RPC 2.0, one JSON object per line on stdin and stdout.
// stdout carries protocol messages only; everything extern modules print goes
// to stderr, which the server forwards to its log. See README.md in this
// directory for the message shapes and error codes.
//
// The host is not a sandbox. Extern modules are trusted application code.

import { pathToFileURL } from "node:url";
import { isAbsolute } from "node:path";

/** Maximum size of one message in either direction, newline excluded. */
export const MAX_MESSAGE_BYTES = 1024 * 1024;
/** Maximum number of calls the host runs at the same time. */
export const MAX_IN_FLIGHT = 64;
/** Maximum nesting depth of a result value. */
export const MAX_DEPTH = 256;
/**
 * Maximum number of values visited when checking a result. Every value takes
 * at least one byte on the wire, so a larger result cannot fit into one
 * message anyway. The budget also bounds results that share references,
 * which would otherwise be visited once per path.
 */
export const MAX_NODES = MAX_MESSAGE_BYTES;
/** Version of the host protocol, announced in `ready` (D60). */
export const PROTOCOL_VERSION = 1;

/** JSON-RPC error codes used by the host. */
export const Codes = Object.freeze({
  PARSE_ERROR: -32700,
  INVALID_REQUEST: -32600,
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  EXTERN_ERROR: -32000,
});

const protocolWrite = process.stdout.write.bind(process.stdout);
const stderrWrite = process.stderr.write.bind(process.stderr);

/** Longest piece of caller or module text echoed in an error message. */
const MAX_ECHO = 256;

function clip(text) {
  const s = String(text).toWellFormed();
  return s.length > MAX_ECHO ? `${s.slice(0, MAX_ECHO)}...` : s;
}

/** A result that cannot cross the bridge; `kind` is Type or TooLarge. */
class WireError extends Error {
  constructor(message, kind = "Type") {
    super(message);
    this.kind = kind;
  }
}

/**
 * Checks that `value` is a JSON value that survives a round trip: null,
 * booleans, finite numbers, well formed strings, arrays and plain objects.
 * The round trip is exact except for negative zero, which JSON.stringify
 * writes as 0. Throws WireError with a path otherwise: kind TooLarge when
 * the value has more than MAX_NODES values counted per path, Type for
 * everything else.
 */
export function checkWireValue(value) {
  checkNode(value, "$", 0, new Set(), { left: MAX_NODES });
}

function checkNode(value, path, depth, seen, budget) {
  if (depth > MAX_DEPTH) throw new WireError(`${path}: nested deeper than ${MAX_DEPTH}`);
  if (--budget.left < 0) throw new WireError(`${path}: more than ${MAX_NODES} values`, "TooLarge");
  switch (typeof value) {
    case "boolean":
      return;
    case "number":
      if (!Number.isFinite(value)) throw new WireError(`${path}: number is not finite`);
      return;
    case "string":
      if (!value.isWellFormed()) throw new WireError(`${path}: string has a lone surrogate`);
      return;
    case "object":
      break;
    default:
      throw new WireError(`${path}: ${typeof value} is not a JSON value`);
  }
  if (value === null) return;
  if (seen.has(value)) throw new WireError(`${path}: cyclic value`);
  seen.add(value);
  if (Array.isArray(value)) {
    for (let i = 0; i < value.length; i++) {
      if (!Object.hasOwn(value, i)) throw new WireError(`${path}[${i}]: sparse array`);
      checkNode(value[i], `${path}[${i}]`, depth + 1, seen, budget);
    }
  } else {
    const proto = Object.getPrototypeOf(value);
    if (proto !== Object.prototype && proto !== null) {
      throw new WireError(`${path}: only plain objects cross the bridge`);
    }
    if (Object.getOwnPropertySymbols(value).length > 0) {
      throw new WireError(`${path}: symbol keys are not allowed`);
    }
    for (const key of Object.keys(value)) {
      if (!key.isWellFormed()) throw new WireError(`${path}: key has a lone surrogate`);
      const desc = Object.getOwnPropertyDescriptor(value, key);
      if (!("value" in desc)) throw new WireError(`${path}.${key}: accessor property`);
      checkNode(desc.value, `${path}.${key}`, depth + 1, seen, budget);
    }
  }
  seen.delete(value);
}

/** Parses `--module name=path` arguments into a Map of name to absolute path. */
export function parseArgs(argv) {
  const modules = new Map();
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] !== "--module" || i + 1 >= argv.length) {
      throw new Error(`unexpected argument: ${argv[i]}`);
    }
    const spec = argv[++i];
    const eq = spec.indexOf("=");
    const name = eq > 0 ? spec.slice(0, eq) : "";
    const file = eq > 0 ? spec.slice(eq + 1) : "";
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name) || !isAbsolute(file)) {
      throw new Error(`--module expects <name>=<absolute path>, got: ${spec}`);
    }
    if (modules.has(name)) throw new Error(`module named twice: ${name}`);
    modules.set(name, file);
  }
  return modules;
}

function send(message) {
  const line = JSON.stringify(message);
  if (Buffer.byteLength(line) > MAX_MESSAGE_BYTES) {
    throw new WireError(`response exceeds ${MAX_MESSAGE_BYTES} bytes`, "TooLarge");
  }
  protocolWrite(line + "\n");
}

function sendError(id, code, message, data) {
  const error = data === undefined ? { code, message } : { code, message, data };
  send({ jsonrpc: "2.0", id, error });
}

function describeThrown(err) {
  try {
    if (err instanceof Error) return clip(`${err.name}: ${err.message}`);
  } catch {
    // A throwing getter on name or message falls through to the generic text.
  }
  return "non Error value thrown";
}

function externError(id, kind, message) {
  sendError(id, Codes.EXTERN_ERROR, message, { kind });
}

function validId(id) {
  return (typeof id === "string" && id.isWellFormed()) || Number.isSafeInteger(id);
}

/** Release functions of the running calls, by request id. */
const running = new Map();

async function handle(modules, line) {
  let req;
  try {
    req = JSON.parse(line);
  } catch {
    return sendError(null, Codes.PARSE_ERROR, "line is not valid JSON");
  }
  if (req === null || typeof req !== "object" || Array.isArray(req) || req.jsonrpc !== "2.0") {
    return sendError(null, Codes.INVALID_REQUEST, "expected a JSON-RPC 2.0 request object");
  }
  const notification = !Object.hasOwn(req, "id");
  if (!notification && !validId(req.id)) {
    return sendError(null, Codes.INVALID_REQUEST, "id must be a string or a safe integer");
  }
  const id = notification ? null : req.id;
  // The host never answers a notification. The only one it acts on is
  // cancel, which frees the slot of a running call without an answer.
  if (notification) {
    if (req.method === "cancel" && req.params !== null && typeof req.params === "object" &&
        validId(req.params.id)) {
      running.get(req.params.id)?.();
    }
    return;
  }
  if (typeof req.method !== "string") {
    return sendError(id, Codes.INVALID_REQUEST, "method must be a string");
  }

  if (req.method === "ping") {
    return send({ jsonrpc: "2.0", id, result: "pong" });
  }
  if (req.method !== "call") {
    return sendError(id, Codes.METHOD_NOT_FOUND, `unknown method: ${clip(req.method)}`);
  }
  const p = req.params;
  if (p === null || typeof p !== "object" || typeof p.module !== "string" ||
      typeof p.fn !== "string" || !Array.isArray(p.args)) {
    return sendError(id, Codes.INVALID_PARAMS, "params must be {module, fn, args[]}");
  }
  const ns = modules.get(p.module);
  if (ns === undefined) {
    return sendError(id, Codes.INVALID_PARAMS, `unknown module: ${clip(p.module)}`);
  }
  const fn = Object.hasOwn(ns, p.fn) ? ns[p.fn] : undefined;
  if (typeof fn !== "function") {
    return sendError(id, Codes.INVALID_PARAMS, `unknown function: ${clip(p.module)}.${clip(p.fn)}`);
  }
  if (running.has(id)) {
    return sendError(id, Codes.INVALID_REQUEST, "a call with this id is still running");
  }
  if (running.size >= MAX_IN_FLIGHT) {
    return externError(id, "Busy", `more than ${MAX_IN_FLIGHT} calls in flight`);
  }

  // A call holds its slot until it settles or the server cancels it. The
  // server is the only clock (D60): when its deadline passes it sends
  // cancel. A cancelled call is never answered, also when it settles later.
  const release = () => {
    if (running.get(id) !== release) return false;
    running.delete(id);
    return true;
  };
  running.set(id, release);
  let result;
  try {
    result = await fn(...p.args);
  } catch (err) {
    if (release()) externError(id, "Threw", describeThrown(err));
    return;
  }
  if (!release()) return;
  if (result === undefined) {
    return externError(id, "Undefined", `${p.module}.${p.fn} returned undefined`);
  }
  try {
    checkWireValue(result);
    send({ jsonrpc: "2.0", id, result });
  } catch (err) {
    if (!(err instanceof WireError)) throw err;
    externError(id, err.kind, clip(`${p.module}.${p.fn}: ${err.message}`));
  }
}

/**
 * Splits a byte stream into lines of at most MAX_MESSAGE_BYTES. Oversized
 * lines are dropped up to the next newline and reported via `onOversized`.
 */
export function lineSplitter(onLine, onOversized) {
  let parts = [];
  let size = 0;
  let dropping = false;
  return (chunk) => {
    let start = 0;
    while (start < chunk.length) {
      const nl = chunk.indexOf(10, start);
      const end = nl === -1 ? chunk.length : nl;
      if (!dropping) {
        size += end - start;
        if (size > MAX_MESSAGE_BYTES) {
          dropping = true;
          parts = [];
        } else {
          parts.push(chunk.subarray(start, end));
        }
      }
      if (nl === -1) break;
      if (dropping) {
        onOversized();
      } else {
        const line = Buffer.concat(parts).toString("utf8");
        if (line.trim() !== "") onLine(line);
      }
      parts = [];
      size = 0;
      dropping = false;
      start = nl + 1;
    }
  };
}

async function main() {
  // Route every write except protocol messages to stderr, so a stray
  // console.log in an extern module cannot corrupt the pipe.
  process.stdout.write = (...args) => stderrWrite(...args);
  for (const level of ["log", "info", "debug"]) {
    console[level] = (...args) => console.error(...args);
  }
  let paths;
  try {
    paths = parseArgs(process.argv.slice(2));
  } catch (err) {
    stderrWrite(`sidecar: ${err.message}\n`);
    process.exit(2);
  }
  const modules = new Map();
  for (const [name, file] of paths) {
    try {
      modules.set(name, await import(pathToFileURL(file).href));
    } catch (err) {
      stderrWrite(`sidecar: cannot load module ${name} from ${file}: ${err}\n`);
      process.exit(2);
    }
  }
  const exports = {};
  for (const [name, ns] of modules) {
    exports[name] = Object.keys(ns).filter((k) => typeof ns[k] === "function").sort();
  }
  const onLine = (line) => {
    handle(modules, line).catch((err) => {
      stderrWrite(`sidecar: internal error: ${err && err.stack}\n`);
      process.exit(70);
    });
  };
  const onOversized = () =>
    sendError(null, Codes.INVALID_REQUEST, `request exceeds ${MAX_MESSAGE_BYTES} bytes`);
  process.stdin.on("data", lineSplitter(onLine, onOversized));
  // The server owns the host. When it closes the pipe, the host stops.
  process.stdin.on("end", () => process.exit(0));
  send({ jsonrpc: "2.0", method: "ready", params: { protocol: PROTOCOL_VERSION, modules: exports } });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  await main();
}
