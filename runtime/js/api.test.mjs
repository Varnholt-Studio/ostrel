// Contract tests for the error kind unions of `api.d.ts` (F2). The declaration file has no
// runtime form, so these tests read its source and check the exact members of each union.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./api.d.ts", import.meta.url), "utf8");

// Returns the string literal members of `export type NAME = | "A" | "B" ...;`.
function members(name) {
  const match = source.match(new RegExp(`export type ${name} =([^;]*);`));
  assert.ok(match, `type ${name} not found in api.d.ts`);
  const body = match[1];
  const literals = [...body.matchAll(/"([^"]*)"/g)].map((m) => m[1]);
  const rest = body.replace(/"[^"]*"/g, "").replace(/[|\s]/g, "");
  assert.equal(rest, "", `type ${name} holds more than string literals`);
  return literals;
}

test("RuntimeErrorKind lists the VM kinds of ARCHITECTURE 3.4 incl. D113 and D123", () => {
  assert.deepEqual(members("RuntimeErrorKind"), [
    "IntOverflow",
    "DivisionByZero",
    "CallDepth",
    "StepLimit",
    "HeapLimit",
    "TextLimit",
    "OutputLimit",
    "NegativeCount",
  ]);
});

test("CallErrorKind lists the call errors of ARCHITECTURE 5.7 incl. D81 and D134", () => {
  assert.deepEqual(members("CallErrorKind"), [
    "Denied",
    "NotFound",
    "Limit",
    "Invalid",
    "Offline",
    "OutcomeUnknown",
    "Unavailable",
  ]);
});

test("RejectReason and ExternErrorKind stay unchanged", () => {
  assert.deepEqual(members("RejectReason"), ["Denied", "Conflict", "Forged", "Invalid", "Limit"]);
  assert.deepEqual(members("ExternErrorKind"), [
    "Type",
    "Threw",
    "Undefined",
    "TooLarge",
    "Busy",
    "Crashed",
    "Timeout",
  ]);
});

test("ErrorKind is the union of the four kind families", () => {
  const match = source.match(/export type ErrorKind =([^;]*);/);
  assert.ok(match, "type ErrorKind not found");
  const parts = match[1].split("|").map((p) => p.trim());
  assert.deepEqual(parts, ["RuntimeErrorKind", "RejectReason", "ExternErrorKind", "CallErrorKind"]);
});

test("runtime kinds and call kinds do not share a name", () => {
  const runtime = new Set(members("RuntimeErrorKind"));
  const shared = members("CallErrorKind").filter((k) => runtime.has(k));
  assert.deepEqual(shared, []);
});
