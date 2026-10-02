// Contract tests for the default std theme: token table, stylesheet and their link.
// Run with: node --test std/theme/
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const dir = new URL('./', import.meta.url);
const tokens = JSON.parse(readFileSync(new URL('tokens.json', dir), 'utf8'));
const css = readFileSync(new URL('theme.css', dir), 'utf8');

const NAME = /^[a-z][a-zA-Z0-9]*$/;
const SCHEMES = ['light', 'dark'];

// Mapping from a token name to its CSS custom property, documented in README.md.
function cssVar(name) {
  return '--ostrel-' + name.replace(/[A-Z0-9]/g, (c) => '-' + c.toLowerCase());
}

function stripComments(text) {
  return text.replace(/\/\*[\s\S]*?\*\//g, '');
}

// Returns the declarations of the first rule whose selector list equals `selector`.
function block(selector) {
  const body = stripComments(css);
  const re = /([^{}]+)\{([^{}]*)\}/g;
  let m;
  while ((m = re.exec(body)) !== null) {
    if (m[1].trim() === selector) return m[2];
  }
  return null;
}

// Rules nested in an at rule are flattened: the regex sees only innermost blocks.
function allSelectors() {
  const body = stripComments(css).replace(/@media[^{]*\{/g, '');
  const out = [];
  const re = /([^{}]+)\{[^{}]*\}/g;
  let m;
  while ((m = re.exec(body)) !== null) {
    for (const s of m[1].split(',')) out.push(s.trim());
  }
  return out;
}

function declaredVars(decls) {
  const out = new Map();
  for (const m of decls.matchAll(/(--ostrel-[a-z0-9-]+)\s*:\s*([^;]+);/g)) out.set(m[1], m[2].trim());
  return out;
}

function hexToRgb(hex) {
  const h = hex.slice(1);
  const full = h.length === 3 ? [...h].map((c) => c + c).join('') : h;
  return [0, 2, 4].map((i) => parseInt(full.slice(i, i + 2), 16) / 255);
}

function luminance(hex) {
  const [r, g, b] = hexToRgb(hex).map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a, b) {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

const names = Object.keys(tokens.tokens);

test('token file has a version and a non empty token table', () => {
  assert.equal(tokens.version, 1);
  assert.ok(names.length > 0);
});

test('token names are lowerCamel ASCII, usable as $name in style blocks', () => {
  for (const n of names) assert.match(n, NAME, n);
});

test('tokens named in SYNTAX 4.8 and used by the example apps exist', () => {
  for (const n of ['accentSoft', 'muted', 'line']) assert.ok(names.includes(n), n);
});

test('every token has a kind and a value for every color scheme', () => {
  const kinds = new Set(['color', 'length', 'font', 'number', 'shadow']);
  for (const [n, t] of Object.entries(tokens.tokens)) {
    assert.ok(kinds.has(t.kind), `${n}: kind ${t.kind}`);
    assert.equal(typeof t.doc, 'string', `${n}: doc`);
    for (const s of SCHEMES) assert.equal(typeof t[s], 'string', `${n}: ${s}`);
  }
});

test('token values cannot break out of a declaration', () => {
  // Values are spliced into CSS by the compiler, so they must be single, inert values.
  const unsafe = /[;{}<>\\@"'`]|url\(|expression\(|\/\*/i;
  for (const [n, t] of Object.entries(tokens.tokens)) {
    for (const s of SCHEMES) assert.doesNotMatch(t[s], unsafe, `${n}.${s}`);
  }
});

test('color tokens are hex colors so contrast can be checked', () => {
  for (const [n, t] of Object.entries(tokens.tokens)) {
    if (t.kind !== 'color') continue;
    for (const s of SCHEMES) assert.match(t[s], /^#([0-9a-f]{3}|[0-9a-f]{6})$/, `${n}.${s}`);
  }
});

test('theme.css declares exactly the token table on :root, with the same values', () => {
  const root = block(':root');
  assert.ok(root, ':root rule missing');
  const vars = declaredVars(root);
  assert.deepEqual([...vars.keys()].sort(), names.map(cssVar).sort());
  for (const n of names) assert.equal(vars.get(cssVar(n)), tokens.tokens[n].light, n);
});

test('theme.css declares the dark values under prefers-color-scheme: dark', () => {
  const m = stripComments(css).match(
    /@media\s*\(prefers-color-scheme:\s*dark\)\s*\{\s*:root\s*\{([^{}]*)\}\s*\}/,
  );
  assert.ok(m, 'dark scheme block missing');
  const vars = declaredVars(m[1]);
  for (const n of names) {
    const want = tokens.tokens[n].dark;
    if (want === tokens.tokens[n].light) continue;
    assert.equal(vars.get(cssVar(n)), want, n);
  }
  for (const k of vars.keys()) assert.ok(names.map(cssVar).includes(k), `unknown ${k}`);
});

test('theme.css only reads tokens that exist', () => {
  const known = new Set(names.map(cssVar));
  for (const m of css.matchAll(/var\((--ostrel-[a-z0-9-]+)\)/g)) assert.ok(known.has(m[1]), m[1]);
});

test('theme.css never hard codes a color outside the token declarations', () => {
  const body = stripComments(css)
    .replace(/:root\s*\{[^{}]*\}/g, '')
    .replace(/@media[^{]*\{\s*\}/g, '');
  assert.doesNotMatch(body, /#[0-9a-fA-F]{3,6}\b|rgba?\(|hsla?\(/);
});

test('theme selectors cannot collide with app classes', () => {
  // Apps write their own classes (`.muted`, `.title` in examples/chat-nostd). The theme only
  // targets elements, data and aria attributes, and classes with the reserved `o-` prefix.
  const ok = /^(:root|\*|::?[a-z-]+|[a-z0-9]+|\.o-[a-z-]+|\[(data|aria)-[a-z-]+(="[a-z]+")?\])+((\s+|\s*>\s*)(\*|[a-z0-9]+|\.o-[a-z-]+|\[(data|aria)-[a-z-]+(="[a-z]+")?\]|:[a-z-]+(\([^)]*\))?)+)*$/;
  for (const s of allSelectors()) {
    if (s.startsWith('@') || s === '') continue;
    const plain = s.replace(/:(hover|focus-visible|disabled|focus)/g, '');
    assert.match(plain, ok, s);
  }
});

test('std hooks of SYNTAX 4.7 and 4.8 are styled', () => {
  const sel = allSelectors();
  for (const want of [
    '[data-pending="true"]',
    '[data-rejected="true"]',
    '[aria-current="true"]',
    '[data-look="title"]',
    '[data-look="strong"]',
    '[data-look="muted"]',
    '.o-row',
    '.o-col',
    '.o-split',
  ]) {
    assert.ok(sel.some((s) => s.includes(want)), want);
  }
});

test('theme contains no app specific names (D30)', () => {
  const text = JSON.stringify(tokens) + css;
  assert.doesNotMatch(text, /\b(chat|room|message|issue|mine|board|tracker)\b/i);
});

test('text colors meet WCAG AA contrast on the surface in both schemes', () => {
  const t = tokens.tokens;
  const pairs = [
    ['text', 'bg', 4.5],
    ['muted', 'bg', 4.5],
    ['text', 'surface', 4.5],
    ['accentText', 'accent', 4.5],
    ['text', 'accentSoft', 4.5],
    ['danger', 'bg', 4.5],
  ];
  for (const s of SCHEMES) {
    for (const [fg, bg, min] of pairs) {
      const c = contrast(t[fg][s], t[bg][s]);
      assert.ok(c >= min, `${s}: ${fg} on ${bg} is ${c.toFixed(2)}, needs ${min}`);
    }
  }
});

test('the documented name mapping is stable', () => {
  assert.equal(cssVar('accentSoft'), '--ostrel-accent-soft');
  assert.equal(cssVar('space2'), '--ostrel-space-2');
  assert.equal(cssVar('line'), '--ostrel-line');
});

// Declarations of every rule whose selector list contains `selector`, in source order.
function declsFor(selector) {
  const body = stripComments(css).replace(/@media[^{]*\{/g, '');
  const re = /([^{}]+)\{([^{}]*)\}/g;
  const out = [];
  let m;
  while ((m = re.exec(body)) !== null) {
    if (m[1].split(',').some((s) => s.trim() === selector)) out.push(m[2]);
  }
  return out.join('\n');
}

function decl(decls, prop) {
  let value;
  for (const m of decls.matchAll(/(^|[;\s])([a-z-]+)\s*:\s*([^;]+);/g)) {
    if (m[2] === prop) value = m[3].trim();
  }
  return value;
}

// The std element `pick` renders as a native select with option children (allowlist in
// runtime/js/view/safe/attrs.js). It must look and behave like the other controls.
test('pick (select) shares the control look of button and input', () => {
  const sel = declsFor('select');
  const inp = declsFor('input');
  assert.ok(sel, 'select is not styled');
  for (const prop of ['font', 'border-radius', 'border', 'padding', 'color', 'background']) {
    assert.equal(decl(sel, prop), decl(inp, prop), `select ${prop}`);
  }
  assert.equal(decl(sel, 'min-width'), '0');
  assert.equal(decl(sel, 'cursor'), 'pointer');
});

test('pick (select) has the focus ring and the disabled state of the other controls', () => {
  assert.equal(decl(declsFor('select:focus-visible'), 'outline'), decl(declsFor('input:focus-visible'), 'outline'));
  assert.equal(decl(declsFor('select:focus-visible'), 'outline-offset'), decl(declsFor('input:focus-visible'), 'outline-offset'));
  const off = declsFor('select:disabled');
  assert.equal(decl(off, 'opacity'), 'var(--ostrel-pending-opacity)');
  assert.equal(decl(off, 'cursor'), 'default');
  assert.equal(decl(declsFor('input:disabled'), 'opacity'), 'var(--ostrel-pending-opacity)');
});

test('pick (select) hover and option colors follow the tokens in both schemes', () => {
  assert.equal(decl(declsFor('select:hover'), 'border-color'), 'var(--ostrel-accent)');
  const opt = declsFor('option');
  assert.equal(decl(opt, 'background'), 'var(--ostrel-bg)');
  assert.equal(decl(opt, 'color'), 'var(--ostrel-text)');
});
