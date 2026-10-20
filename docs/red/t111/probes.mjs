// Probes for the red review of docs/types.md (T149). Run: node docs/red/t111/probes.mjs
// Each probe states a claim of docs/types.md and checks it against the JavaScript platform the
// generated client runs on. The script exits with 1 while any claim does not hold, so it is a
// reproduction, not a gate test (it is deliberately not named *.test.mjs).

const results = [];

function probe(id, claim, ok, detail) {
  results.push({ id, claim, ok, detail });
}

// F1: B-F14 in numbers. A client guard written with String.prototype.length disagrees with the
// scalar value count of the server for every astral character.
{
  const emoji = '\u{1F600}';
  const combining = 'e\u0301';
  const thousand = emoji.repeat(1000);
  probe('F1a', 'JS .length of U+1F600 equals its len 1 (types.md:106)', emoji.length === 1,
    `.length ${emoji.length}, scalar values ${[...emoji].length}`);
  probe('F1b', 'JS .length of e U+0301 equals its len 2 (types.md:107)', combining.length === 2,
    `.length ${combining.length}, scalar values ${[...combining].length}`);
  probe('F1c', '1000 emoji pass check len in 1..2000 the same way on both targets',
    thousand.length <= 2000 && [...thousand].length <= 2000 && thousand.length === [...thousand].length,
    `.length ${thousand.length}, scalar values ${[...thousand].length}`);
}

// F2: every Time in the documented range is a usable instant on the client.
{
  const edge = 8.64e15;
  for (const t of [edge, edge + 1, Number.MAX_SAFE_INTEGER, -Number.MAX_SAFE_INTEGER]) {
    const ms = new Date(t).getTime();
    probe('F2', `Time ${t} is a valid Date in JS (types.md:96, api.d.ts:68)`, !Number.isNaN(ms),
      `new Date(${t}).getTime() = ${ms}`);
  }
}

// F3: an Int on the wire is "number, safe integer" (types.md:31). JSON.parse turns these
// non integer or non canonical texts into safe integers before any guard can see them.
for (const text of ['1.0', '1e3', '-0', '9007199254740990.9', '9007199254740991.4']) {
  const value = JSON.parse(text);
  const shown = Object.is(value, -0) ? '-0' : String(value);
  probe('F3', `wire text ${text} is rejected as an Int`, !Number.isSafeInteger(value),
    `JSON.parse gives ${shown}, Number.isSafeInteger ${Number.isSafeInteger(value)}`);
}

// F4: the 256 KiB Text field limit (ARCHITECTURE 5.9) has no unit; JS units and UTF-8 differ.
{
  const cjk = '\u4E00'.repeat(256 * 1024);
  const units = cjk.length;
  const bytes = Buffer.byteLength(cjk, 'utf8');
  probe('F4', 'one Text is within 256 KiB by UTF-16 units and by UTF-8 bytes alike',
    (units <= 262144) === (bytes <= 262144), `UTF-16 units ${units}, UTF-8 bytes ${bytes}`);
}

let failed = 0;
for (const r of results) {
  if (!r.ok) failed += 1;
  console.log(`${r.ok ? 'holds  ' : 'BROKEN '} ${r.id} ${r.claim}: ${r.detail}`);
}
console.log(`${failed} of ${results.length} probes show a broken claim`);
process.exitCode = failed > 0 ? 1 : 0;
