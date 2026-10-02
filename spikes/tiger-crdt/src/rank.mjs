// Fractional index keys for `Rank` (ARCHITECTURE 5.1). Digits are ASCII ascending, so
// code point order of keys equals numeric order. Generated keys never end in '0'.
const ALPHABET = '0123456789abcdefghijklmnopqrstuvwxyz';
const BASE = ALPHABET.length;

function digit(s, i) {
  return ALPHABET.indexOf(s[i]);
}

// Returns a key strictly between `lo` and `hi`. `lo` may be '' (no lower bound) and
// `hi` may be null (no upper bound). Requires lo < hi.
export function rankBetween(lo, hi) {
  // Two rows may share a key (ties are broken by row id); then any key above `lo` will do.
  if (hi !== null && lo >= hi) hi = null;
  let out = '';
  let bounded = hi !== null;
  for (let i = 0; ; i++) {
    const a = i < lo.length ? digit(lo, i) : 0;
    const b = bounded && i < hi.length ? digit(hi, i) : BASE;
    if (a === b) {
      out += ALPHABET[a];
      continue;
    }
    if (b - a > 1) return out + ALPHABET[(a + b) >> 1];
    out += ALPHABET[a];
    bounded = false;
  }
}

export function initialRank(index) {
  let s = ((index + 1) * 97).toString(BASE);
  s = '0'.repeat(Math.max(0, 5 - s.length)) + s;
  return s + 'i';
}
