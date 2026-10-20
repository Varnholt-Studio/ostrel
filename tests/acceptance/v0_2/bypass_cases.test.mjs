// Consistency tests for the case data of the rule bypass suite (README.md in this directory).
//
// These tests check the data, not the server: every case is well formed, names only models,
// fields and functions its app declares (unless the case expects exactly that fault), uses
// only outcomes the sync protocol defines, and has an allowed control on the same app. They
// carry no `ac_NN_` title on purpose: the cases become evidence for their criteria only when
// the runner sends them to a built server (README.md, "Runner").
import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..", "..", "..");
const read = (path) => readFileSync(join(ROOT, path), "utf8");

// ---------------------------------------------------------------------------------------
// Protocol facts, read from the sources so that the data cannot drift from them.

const PROTOCOL = read("crates/ostrel_sync/src/protocol.rs");
const WIRE_JSON = read("crates/ostrel_sync/src/wire/json.rs");

// Variant names of `pub enum NAME { ... }` in protocol.rs.
function variants(name) {
  const match = PROTOCOL.match(new RegExp(`pub enum ${name} \\{([\\s\\S]*?)\\n\\}`));
  assert.ok(match, `enum ${name} not found in protocol.rs`);
  const body = match[1].replace(/\/\/[^\n]*/g, "");
  return [...body.matchAll(/^\s{4}([A-Z][A-Za-z]*)\b/gm)].map((m) => m[1]);
}

// Value of `name: EXPR,` in the LIMITS table of protocol.rs (integers and `a << b` only).
function limit(name) {
  const table = PROTOCOL.slice(PROTOCOL.indexOf("pub const LIMITS"));
  const match = table.match(new RegExp(`\\b${name}: ([0-9_]+)(?: << ([0-9]+))?,`));
  assert.ok(match, `limit ${name} not found in protocol.rs`);
  const base = Number(match[1].replaceAll("_", ""));
  return match[2] ? base * 2 ** Number(match[2]) : base;
}

const REJECT_REASONS = variants("RejectReason");
const ERROR_CODES = variants("ErrorCode");
const CALL_ERRORS = variants("CallError");
const CLIENT_MESSAGES = [...PROTOCOL.matchAll(/ClientMsg::\w+(?:\(_\)| \{ \.\. \})? => "(\w+)"/g)].map(
  (m) => m[1],
);
const FRAME_BYTES = limit("frame_bytes");
const TEXT_BYTES = limit("text_bytes");
const OPS_PER_BATCH = limit("ops_per_batch");
const TAGS_PER_REMOVE = limit("tags_per_remove");
const MAX_DEPTH = Number(WIRE_JSON.match(/pub const MAX_DEPTH: usize = (\d+);/)[1]);

// Keys of each client frame (ARCHITECTURE 5.4, wire/frame.rs); every key is required.
const FRAME_KEYS = {
  hello: ["v", "t", "token", "replica", "replica_secret", "last_server_seq"],
  subscribe: ["v", "t", "sub", "query"],
  ops: ["v", "t", "ops"],
  call: ["v", "t", "call", "function", "args"],
};
const OP_KEYS = ["id", "hlc", "model", "row", "body"];
const BODY_KEYS = {
  make: null,
  drop: [],
  set: ["field", "value"],
  add: ["field", "elem"],
  remove: ["field", "elem", "tags"],
  put: ["field", "key", "value"],
  remove_key: ["field", "key"],
};
const QUERY_KEYS = ["model", "filter", "order", "limit", "after"];

// ---------------------------------------------------------------------------------------
// Apps: a light reader for the declarations the cases refer to. It reads only what the
// fixtures use (data blocks with their fields, server functions with their parameters); the
// compiler is the authority on everything else.

const IMPLICIT_FIELDS = ["id", "made", "changed", "author", "pending", "rejected"];

function readApp(path) {
  const models = new Map();
  const functions = new Map();
  let current = null;
  for (const line of read(path).split("\n")) {
    const data = line.match(/^data (\w+)\s*$/);
    const fn = line.match(/^server fn (\w+)\(([^)]*)\)/);
    if (data) {
      current = new Map();
      models.set(data[1], current);
      continue;
    }
    if (fn) {
      const params = fn[2].trim() === "" ? [] : fn[2].split(",").map((p) => p.trim());
      functions.set(fn[1], params);
      current = null;
      continue;
    }
    if (/^\S/.test(line)) {
      current = null;
      continue;
    }
    const field = current && line.match(/^ {2}(\w+): (.+?)\s*$/);
    if (field) {
      const [, name, rest] = field;
      const type = rest.replace(/\s*=.*$/, "").replace(/\s+(fixed|serial.*|merge.*)$/, "");
      current.set(name, { type, hasDefault: rest.includes(" = ") });
    }
  }
  return { models, functions };
}

// ---------------------------------------------------------------------------------------
// Case data.

const CASE_DIR = join(HERE, "cases");
const FIXTURE_DIR = join(HERE, "fixtures");
const caseFiles = readdirSync(CASE_DIR)
  .filter((f) => f.endsWith(".json"))
  .sort();
const fixtures = new Map(
  readdirSync(FIXTURE_DIR)
    .filter((f) => f.endsWith(".json"))
    .map((f) => [f.slice(0, -5), JSON.parse(readFileSync(join(FIXTURE_DIR, f), "utf8"))]),
);
const suites = caseFiles.map((file) => ({
  file,
  data: JSON.parse(readFileSync(join(CASE_DIR, file), "utf8")),
}));

// The parts of each criterion that need at least one must deny case (task T124, SPEC 5).
const REQUIRED = {
  "AC-11": [
    "wrong_principal",
    "forged_owner",
    "unknown_field",
    "oversized_payload",
    "malformed_encoding",
    "read",
  ],
  "AC-31": ["existing_id", "tombstoned_id"],
  "AC-32": ["foreign_data", "no_call_rule", "bulk_denied"],
  "AC-33": ["a", "b", "c", "d", "e"],
  "AC-54": ["crafted_call"],
  "AC-55": ["add_other", "hijack", "shape"],
  "AC-57": ["foreign_replica", "order"],
};
// Parts that are honest behaviour and therefore need an allowed case.
const REQUIRED_ALLOW = {
  "AC-55": ["join", "leave"],
  "AC-57": ["honest_outbox"],
};

const KINDS = ["ops", "call", "subscribe", "session", "compile", "migration", "scenario"];
const SEND_KEYS = ["send", "send_raw", "send_bytes_hex", "send_generated"];
const GENERATORS = ["make_with_text", "nested_arrays", "remove_with_tags", "offline_outbox"];

const PLACEHOLDER =
  /^\$(?:op(?:[+-]\d+)?|opof:\w+|hlc|user:\w+|row:\w+|newrow:\w+|foreignrow:\w+|tag:\w+\.\w+\.\w+(?:@\w+)?|replica:\w+|secret:\w+|loggedreplica:\w+)$/;

// Every string value (and object key) inside `value`, depth first.
function* strings(value) {
  if (typeof value === "string") {
    yield value;
  } else if (Array.isArray(value)) {
    for (const v of value) yield* strings(v);
  } else if (value && typeof value === "object") {
    for (const [k, v] of Object.entries(value)) {
      yield k;
      yield* strings(v);
    }
  }
}

// Placeholders inside a raw text frame.
function rawPlaceholders(text) {
  return [...text.matchAll(/"(\$[^"]*)"/g)].map((m) => m[1]);
}

function sameKeys(object, keys) {
  return (
    object !== null &&
    typeof object === "object" &&
    !Array.isArray(object) &&
    Object.keys(object).sort().join() === [...keys].sort().join()
  );
}

const isFatal = (e) => e.fatal === true || e.close === true;
const rejects = (expect, reason) => expect.some((e) => e.reject === reason);
const isNoOp = (expect) =>
  expect.some((e) => isFatal(e) || e.reject === "Invalid" || e.reject === "Forged");

function fixtureOf(suite, c) {
  const name = c.fixture ?? suite.data.fixture;
  if (c.kind === "compile") return null;
  assert.ok(name, `${c.name}: no fixture`);
  const fixture = fixtures.get(name);
  assert.ok(fixture, `${c.name}: unknown fixture ${name}`);
  return fixture;
}

function principals(fixture) {
  const names = new Set(["anonymous", ...(fixture.users ?? []), ...(fixture.replicas ?? [])]);
  return names;
}

function knownPrincipal(fixture, who) {
  const base = who.split("@")[0];
  return principals(fixture).has(base);
}

// Aliases bound by the fixture and by the given list of a case.
function givenAliases(entries) {
  const aliases = new Set();
  for (const g of entries ?? []) if (g.alias) aliases.add(g.alias);
  return aliases;
}

// Checks one placeholder against the names a case can resolve.
function checkPlaceholder(where, p, scope) {
  assert.match(p, PLACEHOLDER, `${where}: unknown placeholder ${p}`);
  const [kind, arg] = p.slice(1).split(":");
  if (kind === "user") assert.ok(scope.users.has(arg), `${where}: unknown user ${arg}`);
  if (kind === "row") assert.ok(scope.rows.has(arg), `${where}: row alias ${arg} is not bound yet`);
  if (kind === "newrow") scope.rows.add(arg);
  if (kind === "loggedreplica") assert.ok(scope.rows.has(arg), `${where}: unknown row ${arg}`);
  if (["opof", "foreignrow", "replica", "secret"].includes(kind)) {
    assert.ok(scope.principals.has(arg), `${where}: unknown principal ${arg}`);
  }
  if (kind === "tag") {
    const [row, field, elem] = arg.split(".");
    assert.ok(scope.rows.has(row), `${where}: tag of unknown row ${row}`);
    const handle = elem.split("@")[0];
    assert.ok(scope.users.has(handle), `${where}: tag of unknown user ${handle}`);
    assert.ok(scope.setFields.has(field), `${where}: ${field} is not a Set field`);
  }
}

function scopeFor(fixture, c) {
  const app = readApp(fixture.app);
  const setFields = new Set();
  for (const fields of app.models.values()) {
    for (const [name, f] of fields) if (f.type.startsWith("Set[")) setFields.add(name);
  }
  return {
    app,
    users: new Set(fixture.users ?? []),
    principals: principals(fixture),
    rows: new Set([...givenAliases(fixture.given), ...givenAliases(c.given)]),
    setFields,
  };
}

// Checks one op of a frame that the server must decode. A name the app does not know is only
// allowed when the case expects the op to be Invalid (D132).
function checkOp(where, op, scope, expect) {
  assert.ok(sameKeys(op, OP_KEYS), `${where}: op keys must be ${OP_KEYS}`);
  const kinds = Object.keys(op.body ?? {});
  assert.equal(kinds.length, 1, `${where}: body must have exactly one kind`);
  const [kind] = kinds;
  assert.ok(kind in BODY_KEYS, `${where}: unknown body kind ${kind}`);
  const inner = op.body[kind];
  const model = scope.app.models.get(op.model);
  if (!model) {
    assert.ok(rejects(expect, "Invalid"), `${where}: unknown model ${op.model} must be Invalid`);
    return;
  }
  if (kind === "make") {
    const declared = [...model.keys()];
    const named = Object.keys(inner);
    const exact = sameKeys(inner, declared);
    if (!exact) {
      assert.ok(
        isNoOp(expect),
        `${where}: a make must name every declared field (${declared}) or expect Invalid (D116)`,
      );
    }
    for (const n of named) {
      if (!model.has(n) && !IMPLICIT_FIELDS.includes(n)) {
        assert.ok(rejects(expect, "Invalid"), `${where}: unknown field ${n} must be Invalid`);
      }
    }
    return;
  }
  assert.ok(sameKeys(inner, BODY_KEYS[kind]), `${where}: ${kind} keys must be ${BODY_KEYS[kind]}`);
  if (inner.field !== undefined && !model.has(inner.field)) {
    assert.ok(rejects(expect, "Invalid"), `${where}: unknown field ${inner.field} must be Invalid`);
  }
}

function checkFrame(where, frame, scope, expect, c) {
  if (c.kind === "compile") {
    assert.ok(sameKeys(frame, ["command", "file"]), `${where}: compile step needs command and file`);
    assert.ok(existsSync(join(ROOT, frame.file)), `${where}: ${frame.file} missing`);
    return;
  }
  const wellFormed =
    frame.v === 0 && FRAME_KEYS[frame.t] !== undefined && sameKeys(frame, FRAME_KEYS[frame.t]);
  if (!wellFormed) {
    assert.ok(
      expect.length === 1 && isFatal(expect[0]),
      `${where}: a malformed frame must expect a fatal error`,
    );
    return;
  }
  assert.ok(CLIENT_MESSAGES.includes(frame.t), `${where}: ${frame.t} is not a client message`);
  if (frame.t === "ops") {
    assert.ok(Array.isArray(frame.ops), `${where}: ops must be an array`);
    const formOk = frame.ops.every(
      (op) =>
        sameKeys(op, OP_KEYS) &&
        Object.keys(op.body ?? {}).length === 1 &&
        Object.keys(op.body)[0] in BODY_KEYS &&
        (Object.keys(op.body)[0] === "make" ||
          sameKeys(op.body[Object.keys(op.body)[0]], BODY_KEYS[Object.keys(op.body)[0]])) &&
        typeof op.id === "string" &&
        op.id.startsWith("$"),
    );
    if (!formOk) {
      assert.ok(
        expect.length === 1 && isFatal(expect[0]),
        `${where}: an op with a form fault must expect a fatal error (D132)`,
      );
      return;
    }
    frame.ops.forEach((op, i) => checkOp(`${where} op ${i}`, op, scope, expect));
  }
  if (frame.t === "call") {
    const params = scope.app.functions.get(frame.function);
    const errors = expect.map((e) => e.return?.error);
    if (!params) {
      assert.deepEqual(errors, ["NotFound"], `${where}: unknown function must be NotFound`);
    } else if (params.length !== frame.args.length) {
      assert.deepEqual(errors, ["Invalid"], `${where}: wrong argument count must be Invalid`);
    }
  }
  if (frame.t === "subscribe") {
    assert.ok(sameKeys(frame.query, QUERY_KEYS), `${where}: query keys must be ${QUERY_KEYS}`);
    assert.ok(scope.app.models.has(frame.query.model), `${where}: unknown model in query`);
  }
}

function checkExpect(where, e, scope) {
  const keys = Object.keys(e);
  if ("reject" in e) {
    assert.ok(REJECT_REASONS.includes(e.reject), `${where}: unknown reject reason ${e.reject}`);
    assert.match(e.op, PLACEHOLDER, `${where}: reject needs the op placeholder`);
  } else if ("ack" in e) {
    assert.match(e.ack, PLACEHOLDER, `${where}: ack needs the op placeholder`);
  } else if ("error" in e) {
    assert.ok(ERROR_CODES.includes(e.error), `${where}: unknown error code ${e.error}`);
    assert.equal(e.fatal, true, `${where}: errors here are fatal`);
  } else if ("close" in e) {
    assert.equal(e.close, true);
    assert.ok(ERROR_CODES.includes(e.error_if_sent), `${where}: unknown error code`);
  } else if ("return" in e) {
    const r = e.return;
    assert.ok(sameKeys(r, ["ok"]) || sameKeys(r, ["error"]), `${where}: return is ok or error`);
    if ("error" in r) assert.ok(CALL_ERRORS.includes(r.error), `${where}: unknown call error`);
  } else if ("snapshot" in e) {
    for (const row of e.snapshot.rows) {
      assert.ok(scope.rows.has(row), `${where}: snapshot names unknown row ${row}`);
    }
  } else if ("welcome" in e) {
    assert.ok(sameKeys(e.welcome, ["replica"]), `${where}: welcome names its replica`);
  } else if ("expected_file" in e) {
    assert.ok(existsSync(join(ROOT, e.expected_file)), `${where}: ${e.expected_file} missing`);
  } else if ("all_acked" in e) {
    assert.ok(Number.isInteger(e.all_acked) && e.rejects === 0, `${where}: all_acked and rejects 0`);
  } else {
    assert.fail(`${where}: unknown expectation ${keys}`);
  }
}

// Builds a generated frame with dummy values for its placeholders, as the runner would.
function dummy(value) {
  if (typeof value === "string" && value.startsWith("$")) return "0".repeat(32);
  if (Array.isArray(value)) return value.map(dummy);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, dummy(v)]));
  }
  return value;
}

function generate(spec) {
  switch (spec.kind) {
    case "make_with_text": {
      const fields = dummy(spec.fields);
      const head = { id: "0".repeat(24), hlc: "0".repeat(32), model: spec.model, row: "0".repeat(32) };
      const frame = (text) =>
        JSON.stringify({
          v: 0,
          t: "ops",
          ops: [{ ...head, body: { make: { ...fields, [spec.text_field]: text } } }],
        });
      if (spec.frame_bytes) {
        const base = Buffer.byteLength(frame(""));
        return frame("a".repeat(Math.max(0, spec.frame_bytes - base)));
      }
      return frame("a".repeat(spec.text_bytes));
    }
    case "nested_arrays":
      return `{"v":0,"t":"ops","ops":${"[".repeat(spec.depth)}${"]".repeat(spec.depth)}}`;
    default:
      return null;
  }
}

function checkGenerated(where, spec, scope, c) {
  assert.ok(GENERATORS.includes(spec.kind), `${where}: unknown generator ${spec.kind}`);
  for (const s of strings(spec)) if (s.startsWith("$")) checkPlaceholder(where, s, scope);
  if (spec.kind === "make_with_text") {
    const fields = scope.app.models.get(spec.model);
    assert.ok(fields?.get(spec.text_field)?.type === "Text", `${where}: text_field must be Text`);
    const text = generate(spec);
    const bytes = Buffer.byteLength(text);
    if (spec.frame_bytes) {
      assert.ok(bytes >= spec.frame_bytes, `${where}: built ${bytes} bytes`);
      assert.ok(bytes > FRAME_BYTES, `${where}: the frame must exceed the frame limit`);
      if (c.covers === "oversized_payload") {
        assert.ok(bytes >= 10 * 1024 * 1024, `${where}: AC-11 names a payload of 10 MiB`);
      }
    } else {
      assert.ok(spec.text_bytes > TEXT_BYTES, `${where}: text must exceed the Text limit`);
      assert.ok(bytes < FRAME_BYTES, `${where}: the frame must stay under the frame limit`);
      JSON.parse(text);
    }
  }
  if (spec.kind === "nested_arrays") {
    assert.ok(spec.depth > MAX_DEPTH, `${where}: depth must exceed MAX_DEPTH ${MAX_DEPTH}`);
    assert.ok(Buffer.byteLength(generate(spec)) < FRAME_BYTES);
  }
  if (spec.kind === "remove_with_tags") {
    assert.ok(spec.tags > TAGS_PER_REMOVE, `${where}: tags must exceed ${TAGS_PER_REMOVE}`);
    assert.ok(scope.setFields.has(spec.field), `${where}: ${spec.field} is not a Set field`);
  }
  if (spec.kind === "offline_outbox") {
    assert.ok(spec.ops > OPS_PER_BATCH, `${where}: the outbox must need more than one batch`);
    assert.ok(sameKeys(spec.fields, [...scope.app.models.get(spec.model).keys()]));
  }
}

function checkCase(suite, c) {
  const where = `${suite.file} ${c.name}`;
  const nn = suite.data.ac.slice(3);
  assert.match(c.name, new RegExp(`^ac_${nn}_[a-z0-9_]+$`), `${where}: name must be ac_${nn}_<topic>`);
  assert.ok(KINDS.includes(c.kind), `${where}: unknown kind ${c.kind}`);
  assert.ok(["allow", "deny"].includes(c.verdict), `${where}: verdict is allow or deny`);
  const fixture = fixtureOf(suite, c);
  if (c.kind === "migration") {
    assert.ok(c.app_after && existsSync(join(ROOT, c.app_after)), `${where}: app_after missing`);
  }

  if (c.verdict === "deny") {
    const control = suite.data.cases.find((d) => d.name === c.control);
    if (c.kind !== "compile") {
      assert.ok(control, `${where}: control ${c.control} not in this file`);
      assert.equal(control.verdict, "allow", `${where}: control must be an allowed case`);
      assert.equal(
        control.fixture ?? suite.data.fixture,
        c.fixture ?? suite.data.fixture,
        `${where}: control must use the same app`,
      );
    }
    if (c.partial) {
      assert.equal(typeof c.db?.expect, "string", `${where}: a partial case states the db`);
    } else {
      assert.equal(c.db, "unchanged", `${where}: a must deny case leaves the database unchanged`);
    }
  } else {
    assert.ok(c.control === undefined, `${where}: an allowed case has no control`);
    assert.ok(c.db === "unchanged" || typeof c.db?.expect === "string", `${where}: db`);
  }

  assert.ok(Array.isArray(c.steps) && c.steps.length > 0, `${where}: steps`);
  if (!fixture) {
    for (const s of c.steps) checkFrame(where, s.send, null, s.expect, c);
    for (const s of c.steps) for (const e of s.expect) checkExpect(where, e, null);
    return;
  }
  assert.ok(knownPrincipal(fixture, c.as), `${where}: unknown principal ${c.as}`);
  const scope = scopeFor(fixture, c);
  checkGiven(where, c.given ?? [], fixture, scope);

  c.steps.forEach((s, i) => {
    const at = `${where} step ${i + 1}`;
    const sends = SEND_KEYS.filter((k) => k in s);
    assert.equal(sends.length, 1, `${at}: exactly one of ${SEND_KEYS}`);
    assert.ok(Array.isArray(s.expect) && s.expect.length > 0, `${at}: expect`);
    if ("send" in s) {
      for (const p of strings(s.send)) if (p.startsWith("$")) checkPlaceholder(at, p, scope);
      checkFrame(at, s.send, scope, s.expect, c);
    }
    if ("send_raw" in s) {
      for (const p of rawPlaceholders(s.send_raw)) checkPlaceholder(at, p, scope);
      assert.ok(s.expect.every(isFatal), `${at}: a raw frame here is malformed and fatal`);
    }
    if ("send_bytes_hex" in s) {
      assert.match(s.send_bytes_hex, /^(?:[0-9a-f]{2})+$/, `${at}: lowercase hex bytes`);
      const bytes = Buffer.from(s.send_bytes_hex, "hex");
      assert.throws(
        () => new TextDecoder("utf-8", { fatal: true }).decode(bytes),
        `${at}: the bytes must not be valid UTF-8`,
      );
    }
    if ("send_generated" in s) checkGenerated(at, s.send_generated, scope, c);
    for (const e of s.expect) checkExpect(at, e, scope);
  });

  if (c.verdict === "deny" && c.kind !== "subscribe") {
    const outcomes = c.steps.flatMap((s) => s.expect);
    const denied = outcomes.some(
      (e) => "reject" in e || isFatal(e) || e.return?.error !== undefined || "expected_file" in e,
    );
    assert.ok(denied, `${where}: a must deny case expects a rejection`);
  }
}

// Given entries are honest writes: they name only declared models and fields, and a make
// carries every declared field, as the generated client sends it.
function checkGiven(where, entries, fixture, scope) {
  for (const g of entries) {
    assert.ok(knownPrincipal(fixture, g.as), `${where}: given by unknown principal ${g.as}`);
    for (const p of strings(g)) if (p.startsWith("$")) checkPlaceholder(`${where} given`, p, scope);
    if (g.make) {
      const fields = scope.app.models.get(g.make);
      assert.ok(fields, `${where}: given makes unknown model ${g.make}`);
      assert.ok(sameKeys(g.fields, [...fields.keys()]), `${where}: given make ${g.alias} fields`);
    } else if (g.put || g.add) {
      const w = g.put ?? g.add;
      assert.ok(scope.rows.has(w.row), `${where}: given writes unknown row ${w.row}`);
    } else {
      assert.ok(typeof g.drop === "string" && scope.rows.has(g.drop), `${where}: given entry`);
    }
  }
}

// ---------------------------------------------------------------------------------------

test("protocol facts are found in the sources", () => {
  assert.deepEqual(REJECT_REASONS, ["Denied", "Conflict", "Forged", "Invalid", "Limit"]);
  assert.ok(ERROR_CODES.includes("Protocol") && ERROR_CODES.includes("Unauthorized"));
  assert.ok(CALL_ERRORS.includes("Denied") && CALL_ERRORS.includes("NotFound"));
  assert.deepEqual(CLIENT_MESSAGES.sort(), Object.keys(FRAME_KEYS).concat("held", "unsubscribe").sort());
  assert.equal(FRAME_BYTES, 1048576);
  assert.ok(MAX_DEPTH > 0 && TEXT_BYTES > 0 && TAGS_PER_REMOVE > 0 && OPS_PER_BATCH > 0);
});

test("the app reader finds the declarations of the fixture apps", () => {
  const teams = readApp("tests/acceptance/v0_2/apps/teams.ostl");
  assert.deepEqual([...teams.models.keys()], ["Team", "Task"]);
  assert.deepEqual([...teams.models.get("Task").keys()], ["team", "title", "owner", "locked"]);
  assert.equal(teams.models.get("Task").get("team").type, "Team");
  assert.equal(teams.models.get("Task").get("locked").hasDefault, true);
  assert.deepEqual([...teams.functions.keys()], ["clearTasks", "purgeTasks", "archiveTeam"]);
  const chat = readApp("examples/v0_2/chat/membership.ostl");
  assert.equal(chat.models.get("Room").get("members").type, "Set[User]");
});

test("every fixture names an existing app and honest given writes", () => {
  for (const [name, fixture] of fixtures) {
    assert.ok(existsSync(join(ROOT, fixture.app)), `fixture ${name}: ${fixture.app} missing`);
    const scope = scopeFor(fixture, {});
    scope.rows = new Set();
    for (const g of fixture.given) {
      checkGiven(`fixture ${name}`, [g], fixture, scope);
      if (g.alias) scope.rows.add(g.alias);
    }
  }
});

test("case files are named after their criterion", () => {
  assert.ok(suites.length > 0, "no case files");
  for (const { file, data } of suites) {
    assert.match(data.ac, /^AC-\d\d$/);
    assert.equal(file, `ac_${data.ac.slice(3)}.json`);
    assert.ok(Array.isArray(data.cases) && data.cases.length > 0, `${file}: cases`);
  }
});

test("case names are unique across the suite", () => {
  const names = suites.flatMap((s) => s.data.cases.map((c) => c.name));
  assert.deepEqual(names.filter((n, i) => names.indexOf(n) !== i), []);
});

test("every case is well formed and consistent with its app and the protocol", () => {
  for (const suite of suites) for (const c of suite.data.cases) checkCase(suite, c);
});

test("every required part of each criterion has a case", () => {
  const byAc = new Map(suites.map((s) => [s.data.ac, s.data.cases]));
  for (const [ac, parts] of Object.entries(REQUIRED)) {
    const cases = byAc.get(ac) ?? [];
    for (const part of parts) {
      assert.ok(
        cases.some((c) => c.covers === part && c.verdict === "deny"),
        `${ac}: no must deny case for ${part}`,
      );
    }
  }
  for (const [ac, parts] of Object.entries(REQUIRED_ALLOW)) {
    const cases = byAc.get(ac) ?? [];
    for (const part of parts) {
      assert.ok(
        cases.some((c) => c.covers === part && c.verdict === "allow"),
        `${ac}: no allowed case for ${part}`,
      );
    }
  }
});

test("the checks reject broken cases", () => {
  const suite = suites.find((s) => s.data.ac === "AC-11");
  const base = suite.data.cases.find((c) => c.name === "ac_11_wrong_principal_renames_foreign_task");
  const broken = (mutate) => {
    const c = structuredClone(base);
    mutate(c);
    return () => checkCase(suite, c);
  };
  assert.doesNotThrow(broken(() => {}));
  assert.throws(broken((c) => (c.name = "wrong_principal")), /name must be/);
  assert.throws(broken((c) => (c.db = { expect: "changed" })), /unchanged/);
  assert.throws(broken((c) => (c.control = "ac_11_missing")), /control/);
  assert.throws(broken((c) => (c.steps[0].expect = [{ reject: "Nope", op: "$op" }])), /reason/);
  assert.throws(broken((c) => (c.steps[0].send.ops[0].row = "$row:nowhere")), /not bound/);
  assert.throws(broken((c) => (c.steps[0].send.ops[0].body.set.field = "secret")), /Invalid/);
  assert.throws(broken((c) => (c.steps[0].send.ops[0].model = "Secret")), /Invalid/);
  assert.throws(broken((c) => (c.steps[0].send.v = 1)), /fatal/);
  assert.throws(broken((c) => (c.steps[0].send.ops[0].hlc = "$clock")), /placeholder/);
  assert.throws(broken((c) => (c.as = "mallory")), /principal/);
  assert.throws(
    broken((c) => (c.steps[0].send = { v: 0, t: "call", call: 1, function: "nope", args: [] })),
    /NotFound/,
  );
  assert.throws(
    broken((c) => (c.steps[0].send.ops[0].body = { make: { title: "x" } })),
    /every declared field/,
  );
  assert.throws(
    broken((c) => {
      c.steps[0] = {
        send_generated: { kind: "nested_arrays", depth: MAX_DEPTH, in: "ops" },
        expect: [{ error: "Protocol", fatal: true }],
      };
    }),
    /MAX_DEPTH/,
  );
  assert.throws(
    broken((c) => {
      c.steps[0] = { send_bytes_hex: "7b7d", expect: [{ error: "Protocol", fatal: true }] };
    }),
    /UTF-8/,
  );
});
