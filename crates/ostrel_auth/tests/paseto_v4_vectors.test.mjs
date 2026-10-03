// Official PASETO v4.public test vectors, checked against an independent reference
// built only on node:crypto (Ed25519). The vectors are committed under ./vectors/
// (see vectors/SOURCE.md); this test never touches the network.
//
// The reference below is test code, not the product implementation. The Rust
// implementation in ostrel_auth runs the same vector file in its own test.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash, createPrivateKey, createPublicKey, sign, verify } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const vectorPath = join(here, 'vectors', 'v4.json');
const sourcePath = join(here, 'vectors', 'SOURCE.md');
const rawVectors = readFileSync(vectorPath);
const suite = JSON.parse(rawVectors.toString('utf8'));

const HEADER = 'v4.public.';
const SIG_LEN = 64;

// Pre-authentication encoding (PASETO spec, "PAE").
function le64(n) {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(BigInt(n) & 0x7fffffffffffffffn);
  return b;
}

function pae(pieces) {
  const parts = [le64(pieces.length)];
  for (const p of pieces) {
    parts.push(le64(p.length), p);
  }
  return Buffer.concat(parts);
}

// Strict base64url without padding: rejects '=', '+', '/' and non canonical tails.
function b64urlDecode(s) {
  if (!/^[A-Za-z0-9_-]*$/.test(s) || s.length % 4 === 1) {
    throw new Error('invalid base64url');
  }
  const out = Buffer.from(s, 'base64url');
  if (out.toString('base64url') !== s) {
    throw new Error('non canonical base64url');
  }
  return out;
}

function privateKeyFromSeed(seedHex) {
  const der = Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), Buffer.from(seedHex, 'hex')]);
  return createPrivateKey({ key: der, format: 'der', type: 'pkcs8' });
}

function publicKeyFromHex(pkHex) {
  const raw = Buffer.from(pkHex, 'hex');
  if (raw.length !== 32) throw new Error('public key must be 32 bytes');
  const der = Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), raw]);
  return createPublicKey({ key: der, format: 'der', type: 'spki' });
}

function signV4Public(seedHex, payload, footer, implicit) {
  const h = Buffer.from(HEADER, 'utf8');
  const m = Buffer.from(payload, 'utf8');
  const f = Buffer.from(footer, 'utf8');
  const i = Buffer.from(implicit, 'utf8');
  const sig = sign(null, pae([h, m, f, i]), privateKeyFromSeed(seedHex));
  const body = HEADER + Buffer.concat([m, sig]).toString('base64url');
  return f.length > 0 ? `${body}.${f.toString('base64url')}` : body;
}

// Returns { payload, footer } or throws. `expectedFooter` is compared when given.
function verifyV4Public(pkHex, token, implicit, expectedFooter) {
  if (typeof token !== 'string' || !token.startsWith(HEADER)) {
    throw new Error('wrong header');
  }
  const rest = token.slice(HEADER.length).split('.');
  if (rest.length < 1 || rest.length > 2) throw new Error('wrong number of parts');
  const body = b64urlDecode(rest[0]);
  const f = rest.length === 2 ? b64urlDecode(rest[1]) : Buffer.alloc(0);
  if (rest.length === 2 && f.length === 0) throw new Error('empty footer part');
  if (body.length < SIG_LEN) throw new Error('token too short');
  const m = body.subarray(0, body.length - SIG_LEN);
  const sig = body.subarray(body.length - SIG_LEN);
  if (expectedFooter !== undefined && !f.equals(Buffer.from(expectedFooter, 'utf8'))) {
    throw new Error('footer mismatch');
  }
  const m2 = pae([Buffer.from(HEADER, 'utf8'), m, f, Buffer.from(implicit, 'utf8')]);
  if (!verify(null, m2, publicKeyFromHex(pkHex), sig)) throw new Error('bad signature');
  return { payload: m.toString('utf8'), footer: f.toString('utf8') };
}

const publicVectors = suite.tests.filter((t) => t.name.startsWith('4-S-'));
const failVectors = suite.tests.filter((t) => t.name.startsWith('4-F-'));
const keyPk = publicVectors[0]['public-key'];
const keySeed = publicVectors[0]['secret-key-seed'];

test('vector file matches the sha256 recorded in SOURCE.md', () => {
  const recorded = readFileSync(sourcePath, 'utf8').match(/sha256 of `v4\.json` \| `([0-9a-f]{64})`/);
  assert.ok(recorded, 'SOURCE.md must record the sha256 of v4.json');
  assert.equal(createHash('sha256').update(rawVectors).digest('hex'), recorded[1]);
});

test('vector file has the expected shape', () => {
  assert.equal(suite.name, 'PASETO v4 Test Vectors');
  assert.equal(publicVectors.length, 3);
  assert.equal(failVectors.length, 5);
  for (const t of publicVectors) {
    assert.equal(t['expect-fail'], false, t.name);
    assert.equal(t['secret-key'], t['secret-key-seed'] + t['public-key'], t.name);
  }
  for (const t of failVectors) assert.equal(t['expect-fail'], true, t.name);
});

test('PAE matches the examples of the PASETO specification', () => {
  assert.equal(pae([]).toString('hex'), '0000000000000000');
  assert.equal(pae([Buffer.alloc(0)]).toString('hex'), '01000000000000000000000000000000');
  assert.equal(
    pae([Buffer.from('test')]).toString('hex'),
    '0100000000000000040000000000000074657374',
  );
});

for (const t of publicVectors) {
  test(`${t.name}: official token verifies and yields payload and footer`, () => {
    const out = verifyV4Public(t['public-key'], t.token, t['implicit-assertion'], t.footer);
    assert.equal(out.payload, t.payload);
    assert.equal(out.footer, t.footer);
  });

  test(`${t.name}: signing reproduces the official token byte for byte`, () => {
    assert.equal(signV4Public(t['secret-key-seed'], t.payload, t.footer, t['implicit-assertion']), t.token);
  });

  test(`${t.name}: wrong implicit assertion is rejected`, () => {
    assert.throws(() => verifyV4Public(t['public-key'], t.token, `${t['implicit-assertion']}x`));
  });

  test(`${t.name}: any flipped bit in the signed body is rejected`, () => {
    const [, , body, footer] = t.token.split('.');
    const raw = b64urlDecode(body);
    for (const pos of [0, raw.length - SIG_LEN - 1, raw.length - SIG_LEN, raw.length - 1]) {
      const bad = Buffer.from(raw);
      bad[pos] ^= 0x01;
      const token = HEADER + bad.toString('base64url') + (footer !== undefined ? `.${footer}` : '');
      assert.throws(() => verifyV4Public(t['public-key'], token, t['implicit-assertion']), `byte ${pos}`);
    }
  });

  test(`${t.name}: another public key is rejected`, () => {
    const otherPk = createPublicKey(privateKeyFromSeed('00'.repeat(32)))
      .export({ format: 'der', type: 'spki' })
      .subarray(12)
      .toString('hex');
    assert.throws(() => verifyV4Public(otherPk, t.token, t['implicit-assertion']), /bad signature/);
  });
}

test('4-S-2: footer swapped for the 4-S-1 form (no footer) is rejected', () => {
  const s2 = publicVectors.find((t) => t.name === '4-S-2');
  const stripped = s2.token.split('.').slice(0, 3).join('.');
  assert.throws(() => verifyV4Public(s2['public-key'], stripped, ''), /bad signature/);
});

test('padded or non base64url encodings are rejected', () => {
  const s1 = publicVectors.find((t) => t.name === '4-S-1');
  const body = s1.token.slice(HEADER.length);
  assert.throws(() => verifyV4Public(keyPk, `${s1.token}=`, ''), /base64url/);
  assert.throws(() => verifyV4Public(keyPk, HEADER + body.replace(/-/g, '+').replace(/_/g, '/'), ''), /base64url/);
  assert.throws(() => verifyV4Public(keyPk, `${s1.token}.`, ''), /empty footer/);
  assert.throws(() => verifyV4Public(keyPk, `${s1.token}.e30.e30`, ''), /number of parts/);
  assert.throws(() => verifyV4Public(keyPk, HEADER + body.slice(0, 40), ''));
});

for (const t of failVectors) {
  test(`${t.name}: rejected by a v4.public verifier with the official key`, () => {
    assert.equal(t.payload, null);
    assert.throws(() => verifyV4Public(keyPk, t.token, t['implicit-assertion'] ?? '', t.footer));
  });
}

test('the official seed signs fresh claims that verify only with the official key', () => {
  const token = signV4Public(keySeed, '{"sub":"u1","exp":"2026-10-03T00:00:00+00:00"}', '', '');
  assert.ok(token.startsWith(HEADER));
  assert.equal(verifyV4Public(keyPk, token, '').payload, '{"sub":"u1","exp":"2026-10-03T00:00:00+00:00"}');
});
