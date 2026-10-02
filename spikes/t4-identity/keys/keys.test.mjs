import { test } from 'node:test';
import assert from 'node:assert/strict';
import { generateUserKey, exportUserKey, importUserKey, publicKeyBytes } from './keys.mjs';
import { RFC8032_TEST1 } from './vector.mjs';

const ALG = { name: 'Ed25519' };
const msg = new TextEncoder().encode('device link statement');

test('generated user key is extractable and exports to one line', async () => {
  const kp = await generateUserKey();
  assert.equal(kp.privateKey.extractable, true);
  const text = await exportUserKey(kp);
  assert.match(text, /^ostrel-user-key-v1\.[A-Za-z0-9_-]{43}\.[A-Za-z0-9_-]{43}$/);
  assert.equal((await publicKeyBytes(kp)).length, 32);
});

test('import restores the same user', async () => {
  const kp = await generateUserKey();
  const text = await exportUserKey(kp);
  const back = await importUserKey(`  ${text}\n`);
  assert.deepEqual(await publicKeyBytes(back), await publicKeyBytes(kp));
  assert.equal(await exportUserKey(back), text);
  // Ed25519 is deterministic: both halves sign identically and cross verify.
  const a = new Uint8Array(await crypto.subtle.sign(ALG, kp.privateKey, msg));
  const b = new Uint8Array(await crypto.subtle.sign(ALG, back.privateKey, msg));
  assert.deepEqual(a, b);
  assert.equal(await crypto.subtle.verify(ALG, kp.publicKey, b, msg), true);
});

test('import rejects malformed bundles', async () => {
  const text = await exportUserKey(await generateUserKey());
  const [, x, d] = text.split('.');
  const bad = [
    undefined, 42, '', text.replace('v1', 'v2'), `${text}.x`, `ostrel-user-key-v1.${x}`,
    `ostrel-user-key-v1.${x}.${d.slice(1)}`, `ostrel-user-key-v1.${x}.${d.slice(1)}=`,
    `ostrel-user-key-v1.${x}.${d.slice(1)}+`,
  ];
  for (const b of bad) {
    await assert.rejects(importUserKey(b), /not a user key bundle/, String(b));
  }
});

test('imported RFC 8032 key signs the RFC signature', async () => {
  const kp = await importUserKey(RFC8032_TEST1.bundle);
  const sig = Buffer.from(await crypto.subtle.sign(ALG, kp.privateKey, new Uint8Array()));
  assert.equal(sig.toString('hex'), RFC8032_TEST1.signature);
});

test('import rejects a public half that belongs to another key', async () => {
  const [, x] = (await exportUserKey(await generateUserKey())).split('.');
  const [, , d] = (await exportUserKey(await generateUserKey())).split('.');
  await assert.rejects(importUserKey(`ostrel-user-key-v1.${x}.${d}`), /inconsistent/);
});
