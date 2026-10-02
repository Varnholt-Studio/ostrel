// Test fixture: extern module whose results cannot cross the bridge.
export function throws() {
  throw new TypeError("bad input");
}

export async function rejects() {
  throw new RangeError("async failure");
}

export function throwsString() {
  throw "plain string";
}

export function longThrow() {
  throw new Error("x".repeat(100000));
}

export function nothing() {}

export function nested() {
  return { a: [1, undefined] };
}

export function aFunction() {
  return () => 1;
}

export function notFinite() {
  return [1, Number.NaN];
}

export function big() {
  return 10n;
}

export function cyclic() {
  const o = {};
  o.self = o;
  return o;
}

export function date() {
  return new Date(0);
}

export function loneSurrogate() {
  return "a\uD800b";
}

export function sparse() {
  return [1, , 3];
}

export function tooLarge() {
  return "y".repeat(1024 * 1024);
}

export function tooDeep() {
  let v = [];
  for (let i = 0; i < 300; i++) v = [v];
  return v;
}

export function shared() {
  const leaf = { x: 1 };
  return { a: leaf, b: leaf };
}

export function hang() {
  return new Promise(() => {});
}

export function dag(levels) {
  let v = [];
  for (let i = 0; i < levels; i++) v = [v, v];
  return v;
}
