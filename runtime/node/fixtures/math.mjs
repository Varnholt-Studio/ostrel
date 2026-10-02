// Test fixture: well behaved extern module.
export function add(a, b) {
  return a + b;
}

export async function slowEcho(value, ms) {
  await new Promise((resolve) => setTimeout(resolve, ms));
  return value;
}

export function record(name, tags) {
  return { name, tags, nested: { ok: true, none: null } };
}

export function chatty(text) {
  console.log("log line from module");
  process.stdout.write("raw stdout from module\n");
  return text;
}

export const notAFunction = 42;
