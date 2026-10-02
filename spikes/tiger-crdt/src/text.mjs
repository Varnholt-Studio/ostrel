// Unicode code point order for JS strings (D50). JS `<` compares UTF-16 code units,
// which puts U+FF01 after U+1F600. Shifting the unit values below restores code point
// order: surrogates move above every other BMP unit, the rest keeps its relative order.
function unit(u) {
  if (u < 0xd800) return u;
  return u >= 0xe000 ? u - 0x800 : u + 0x2000;
}

export function compareText(a, b) {
  if (a === b) return 0;
  const n = a.length < b.length ? a.length : b.length;
  for (let i = 0; i < n; i++) {
    const x = a.charCodeAt(i);
    const y = b.charCodeAt(i);
    if (x !== y) return unit(x) < unit(y) ? -1 : 1;
  }
  return a.length < b.length ? -1 : 1;
}
