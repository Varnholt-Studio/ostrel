import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createPublicKey } from 'node:crypto';
import { mintToken, privateKeyFromSeed, publicKeyFromRaw, readToken, signToken, verifyToken } from '../src/paseto.ts';

// Known answer cases 4-S-1 and 4-S-3 from https://github.com/paseto-standard/test-vectors
// (v4.json, ISC License, Copyright (c) 2021 Paragon Initiative Enterprises).
const SEED = Buffer.from('b4cbfb43df4ce210727d953e4a713307fa19bb7d9f85041438d9e11b942a3774', 'hex');
const PUBLIC = Buffer.from('1eb9dbbbbc047c03fd70604e0071f0987e16b28b757225c11f00415d0e20b1a2', 'hex');
const MESSAGE = Buffer.from('{"data":"this is a signed message","exp":"2022-01-01T00:00:00+00:00"}');
const FOOTER = Buffer.from('{"kid":"zVhMiPBP9fRf2snEcT7gFTioeA9COcNy9DfgL1W60haN"}');
const IMPLICIT = Buffer.from('{"test-vector":"4-S-3"}');
const TOKEN_4_S_1 =
  'v4.public.eyJkYXRhIjoidGhpcyBpcyBhIHNpZ25lZCBtZXNzYWdlIiwiZXhwIjoiMjAyMi0wMS0wMVQwMDowMDowMCswMDowMCJ9bg_XBBzds8lTZShVlwwKSgeKpLT3yukTw6JUz3W4h_ExsQV-P0V54zemZDcAxFaSeef1QlXEFtkqxT1ciiQEDA';
const TOKEN_4_S_3 =
  'v4.public.eyJkYXRhIjoidGhpcyBpcyBhIHNpZ25lZCBtZXNzYWdlIiwiZXhwIjoiMjAyMi0wMS0wMVQwMDowMDowMCswMDowMCJ9NPWciuD3d0o5eXJXG5pJy-DiVEoyPYWs1YSTwWHNJq6DZD3je5gf-0M4JR9ipdUSJbIovzmBECeaWmaqcaP0DQ.eyJraWQiOiJ6VmhNaVBCUDlmUmYyc25FY1Q3Z0ZUaW9lQTlDT2NOeTlEZmdMMVc2MGhhTiJ9';

const secret = privateKeyFromSeed(SEED);
const pub = publicKeyFromRaw(PUBLIC);

test('seed derives the vector public key', () => {
  const raw = createPublicKey(secret).export({ format: 'der', type: 'spki' }).subarray(-32);
  assert.deepEqual(raw, PUBLIC);
});

test('signs vector 4-S-1 byte for byte', () => {
  assert.equal(signToken(secret, MESSAGE), TOKEN_4_S_1);
});

test('signs vector 4-S-3 byte for byte', () => {
  assert.equal(signToken(secret, MESSAGE, FOOTER, IMPLICIT), TOKEN_4_S_3);
});

test('verifies vectors 4-S-1 and 4-S-3', () => {
  assert.deepEqual(verifyToken(pub, TOKEN_4_S_1)?.message, MESSAGE);
  const v = verifyToken(pub, TOKEN_4_S_3, IMPLICIT);
  assert.deepEqual(v?.message, MESSAGE);
  assert.deepEqual(v?.footer, FOOTER);
});

test('rejects wrong implicit assertion, tampering, other headers and non canonical encoding', () => {
  assert.equal(verifyToken(pub, TOKEN_4_S_3), null);
  const body = TOKEN_4_S_1.slice('v4.public.'.length);
  const flipped = (body[5] === 'A' ? 'B' : 'A');
  assert.equal(verifyToken(pub, 'v4.public.' + body.slice(0, 5) + flipped + body.slice(6)), null);
  assert.equal(verifyToken(pub, 'v3.public.' + body), null);
  assert.equal(verifyToken(pub, 'v4.local.' + body), null);
  assert.equal(verifyToken(pub, TOKEN_4_S_1 + '.'), null);
  assert.equal(verifyToken(pub, TOKEN_4_S_1 + '='), null);
  assert.equal(verifyToken(pub, 'v4.public.AAAA'), null);
});

test('minted tokens carry sub and kid and expire', () => {
  const claims = readToken(pub, mintToken(secret, 'user-1', 'key-1'));
  assert.equal(claims?.sub, 'user-1');
  assert.equal(claims?.kid, 'key-1');
  assert.equal(readToken(pub, mintToken(secret, 'user-1', 'key-1', -1)), null);
  const other = privateKeyFromSeed(Buffer.alloc(32, 7));
  assert.equal(readToken(pub, mintToken(other, 'user-1', 'key-1')), null);
});
