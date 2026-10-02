// Node tests for the device link spike, run by the gate (node --test).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  ALG,
  LINK_TTL_MS,
  PREFIX,
  STATEMENT_LEN,
  decodeStatement,
  encodeStatement,
  fromB64url,
  parseLink,
  presentLink,
  rawPublicKey,
  signLink,
  toB64url,
} from './link.mjs';
import { CHALLENGE_TTL_MS, CLOCK_SKEW_MS, Registry, signInMessage } from './server.mjs';

const T0 = 1_790_000_000_000;

const newPair = () => crypto.subtle.generateKey(ALG, true, ['sign', 'verify']);

/** A user with one registered device (`a`) and a fresh device (`b`) that wants to join. */
async function setup() {
  const registry = new Registry();
  const user = await newPair();
  const a = await newPair();
  const b = await newPair();
  const userId = await rawPublicKey(user);
  const aKey = await rawPublicKey(a);
  const bKey = await rawPublicKey(b);
  registry.register(userId, aKey);
  return { registry, userId, a, b, aKey, bKey };
}

async function linkFor(s, { now = T0, signer = s.a, ttlMs } = {}) {
  const link = await signLink({ signerPair: signer, userId: s.userId, deviceKey: s.bKey, now, ttlMs });
  return presentLink({ link, devicePair: s.b });
}

async function rejects(promise, kind) {
  await assert.rejects(promise, (e) => e.kind === kind || assert.fail(`expected ${kind}, got ${e.kind ?? e.message}`));
}

/** Re-signs a modified statement with a key, to test checks behind the signature check. */
async function forge(statementFields, signer, device) {
  const statement = encodeStatement(statementFields);
  const ctx = new TextEncoder();
  const sig = await crypto.subtle.sign(ALG, signer.privateKey, concat(ctx.encode(`${PREFIX}/statement\0`), statement));
  const proof = await crypto.subtle.sign(ALG, device.privateKey, concat(ctx.encode(`${PREFIX}/possession\0`), statement));
  return { link: `${PREFIX}.${toB64url(statement)}.${toB64url(new Uint8Array(sig))}`, proof: toB64url(new Uint8Array(proof)) };
}

function concat(a, b) {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}

/** Same bytes, other text: sets one of the unused low bits of the last base64url character. */
function nonCanonical(text) {
  const abc = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
  const i = abc.indexOf(text.at(-1));
  return text.slice(0, -1) + abc[i ^ 1];
}

async function signIn(registry, userId, pair, now) {
  const challenge = registry.issueChallenge(now);
  const signature = new Uint8Array(await crypto.subtle.sign(ALG, pair.privateKey, signInMessage(challenge)));
  return registry.signIn({ userId, deviceKey: await rawPublicKey(pair), challenge, signature }, now);
}

test('statement layout round trips and has a fixed length', () => {
  const f = {
    userId: new Uint8Array(32).fill(1),
    deviceKey: new Uint8Array(32).fill(2),
    signerKey: new Uint8Array(32).fill(3),
    expiresAt: T0,
    nonce: new Uint8Array(16).fill(4),
  };
  const bytes = encodeStatement(f);
  assert.equal(bytes.length, STATEMENT_LEN);
  assert.deepEqual(decodeStatement(bytes), f);
});

test('a signed link adds the second device key, which can then sign in', async () => {
  const s = await setup();
  const added = await s.registry.acceptLink(await linkFor(s), T0 + 1000);
  assert.equal(added.deviceKey, toB64url(s.bKey));
  assert.equal(s.registry.activeKeys(s.userId).length, 2);
  const session = await signIn(s.registry, s.userId, s.b, T0 + 2000);
  assert.equal(session.userId, toB64url(s.userId));
});

test('link text has the documented form', async () => {
  const s = await setup();
  const { link } = await linkFor(s);
  assert.match(link, /^ostrel-device-link-v1\.[A-Za-z0-9_-]{160}\.[A-Za-z0-9_-]{86}$/);
  const { statement } = parseLink(link);
  const st = decodeStatement(statement);
  assert.equal(st.expiresAt, T0 + LINK_TTL_MS);
  assert.deepEqual(st.deviceKey, s.bKey);
});

test('a link is rejected at and after its expiry', async () => {
  const s = await setup();
  const presented = await linkFor(s);
  await rejects(s.registry.acceptLink(presented, T0 + LINK_TTL_MS), 'Expired');
  await rejects(s.registry.acceptLink(presented, T0 + LINK_TTL_MS + 1), 'Expired');
  assert.equal(s.registry.activeKeys(s.userId).length, 1);
});

test('a link one millisecond before its expiry is accepted', async () => {
  const s = await setup();
  await s.registry.acceptLink(await linkFor(s), T0 + LINK_TTL_MS - 1);
});

test('the server caps the lifetime with its own clock', async () => {
  const s = await setup();
  // A signing clock ahead by the tolerated skew still works.
  await s.registry.acceptLink(await linkFor(s, { now: T0 + CLOCK_SKEW_MS }), T0);
  const s2 = await setup();
  await rejects(s2.registry.acceptLink(await linkFor(s2, { ttlMs: 24 * 3600 * 1000 }), T0), 'LifetimeTooLong');
  const s3 = await setup();
  await rejects(s3.registry.acceptLink(await linkFor(s3, { now: T0 + CLOCK_SKEW_MS + 1 }), T0), 'LifetimeTooLong');
});

test('a link is single use', async () => {
  const s = await setup();
  const presented = await linkFor(s);
  await s.registry.acceptLink(presented, T0);
  await rejects(s.registry.acceptLink(presented, T0 + 1), 'Replayed');
});

test('a link signed by an unregistered key is rejected', async () => {
  const s = await setup();
  await rejects(s.registry.acceptLink(await linkFor(s, { signer: await newPair() }), T0), 'UnknownSigner');
});

test('a key of another user cannot sign a link for this user', async () => {
  const s = await setup();
  const other = await newPair();
  const otherDevice = await newPair();
  s.registry.register(await rawPublicKey(other), await rawPublicKey(otherDevice));
  await rejects(s.registry.acceptLink(await linkFor(s, { signer: otherDevice }), T0), 'UnknownSigner');
});

test('a link signed by a revoked key is rejected', async () => {
  const s = await setup();
  const c = await newPair();
  const cKey = await rawPublicKey(c);
  const link = await signLink({ signerPair: s.a, userId: s.userId, deviceKey: cKey, now: T0 });
  await s.registry.acceptLink(await presentLink({ link, devicePair: c }), T0);
  const fromC = await linkFor(s, { signer: c });
  s.registry.revoke(s.userId, cKey, s.aKey);
  await rejects(s.registry.acceptLink(fromC, T0), 'UnknownSigner');
});

test('a revoked device key cannot sign in and cannot be linked again', async () => {
  const s = await setup();
  await s.registry.acceptLink(await linkFor(s), T0);
  s.registry.revoke(s.userId, s.bKey, s.aKey);
  await rejects(signIn(s.registry, s.userId, s.b, T0 + 1), 'UnknownDeviceKey');
  await rejects(s.registry.acceptLink(await linkFor(s, { now: T0 + 2 }), T0 + 2), 'DeviceKeyTaken');
  await signIn(s.registry, s.userId, s.a, T0 + 3);
});

test('the last active key cannot be revoked', async () => {
  const s = await setup();
  assert.throws(() => s.registry.revoke(s.userId, s.aKey, s.aKey), (e) => e.kind === 'LastKey');
});

test('a device key already owned by a user is not added again', async () => {
  const s = await setup();
  const self = await signLink({ signerPair: s.a, userId: s.userId, deviceKey: s.aKey, now: T0 });
  await rejects(s.registry.acceptLink(await presentLink({ link: self, devicePair: s.a }), T0), 'DeviceKeyTaken');
});

test('any changed byte of the statement breaks the signature', async () => {
  const s = await setup();
  const { link, proof } = await linkFor(s);
  const [, st, sig] = link.split('.');
  const bytes = fromB64url(st);
  // Byte 103 ends the expiry, byte 110 is inside the nonce, byte 40 inside the device key.
  for (const at of [40, 103, 110]) {
    const changed = bytes.slice();
    changed[at] ^= 1;
    const tampered = `${PREFIX}.${toB64url(changed)}.${sig}`;
    await assert.rejects(s.registry.acceptLink({ link: tampered, proof }, T0), (e) =>
      ['BadSignature', 'UnknownSigner'].includes(e.kind),
    );
  }
});

test('the new device must hold the key the link names', async () => {
  const s = await setup();
  const { link } = await linkFor(s);
  await assert.rejects(presentLink({ link, devicePair: await newPair() }), /another device key/);
  // A proof made with another key, or for another statement, is refused by the server.
  const intruder = await newPair();
  const forged = await forge(
    { ...decodeStatement(parseLink(link).statement) },
    s.a,
    intruder,
  );
  await rejects(s.registry.acceptLink(forged, T0), 'BadProof');
  const other = await linkFor(s, { now: T0 + 5 });
  await rejects(s.registry.acceptLink({ link, proof: other.proof }, T0), 'BadProof');
});

test('the signer signature is not accepted as the possession proof', async () => {
  const s = await setup();
  const self = await signLink({ signerPair: s.a, userId: s.userId, deviceKey: s.aKey, now: T0 });
  const sig = self.split('.')[2];
  await rejects(s.registry.acceptLink({ link: self, proof: sig }, T0), 'BadProof');
});

test('malformed links and proofs are rejected without throwing anything else', async () => {
  const s = await setup();
  const { link, proof } = await linkFor(s);
  const [, st, sig] = link.split('.');
  const bad = [
    undefined,
    null,
    42,
    '',
    link.toUpperCase(),
    `${link}.`,
    `${link} `,
    `ostrel-device-link-v2.${st}.${sig}`,
    `${PREFIX}.${st}`,
    `${PREFIX}.${st.slice(1)}.${sig}`,
    `${PREFIX}.${st}.${nonCanonical(sig)}`,
    `${PREFIX}.${st.replace(/^./, '+')}.${sig}`,
    'x'.repeat(1 << 20),
  ];
  for (const b of bad) {
    await rejects(s.registry.acceptLink({ link: b, proof }, T0), 'Malformed');
  }
  assert.deepEqual(fromB64url(nonCanonical(sig)), fromB64url(sig));
  for (const p of [undefined, '', proof.slice(1), `${proof}A`, proof.replace(/^./, '/'), nonCanonical(proof)]) {
    await rejects(s.registry.acceptLink({ link, proof: p }, T0), 'Malformed');
  }
});

test('an expiry beyond the safe integer range is malformed', async () => {
  const s = await setup();
  const { link, proof } = await linkFor(s);
  const [, st, sig] = link.split('.');
  const bytes = fromB64url(st);
  bytes.fill(0xff, 96, 104);
  await rejects(s.registry.acceptLink({ link: `${PREFIX}.${toB64url(bytes)}.${sig}`, proof }, T0), 'Malformed');
});

test('sign in challenges are single use and expire after 60 s', async () => {
  const s = await setup();
  const challenge = s.registry.issueChallenge(T0);
  const signature = new Uint8Array(await crypto.subtle.sign(ALG, s.a.privateKey, signInMessage(challenge)));
  const req = { userId: s.userId, deviceKey: s.aKey, challenge, signature };
  await s.registry.signIn(req, T0 + 1);
  await rejects(s.registry.signIn(req, T0 + 2), 'UnknownChallenge');

  const late = s.registry.issueChallenge(T0);
  const lateSig = new Uint8Array(await crypto.subtle.sign(ALG, s.a.privateKey, signInMessage(late)));
  await rejects(
    s.registry.signIn({ ...req, challenge: late, signature: lateSig }, T0 + CHALLENGE_TTL_MS),
    'Expired',
  );
});

test('a link signature cannot be used to sign in', async () => {
  const s = await setup();
  const challenge = s.registry.issueChallenge(T0);
  // Signing the bare challenge (no sign in context) is refused.
  const bare = new Uint8Array(await crypto.subtle.sign(ALG, s.a.privateKey, challenge));
  await rejects(
    s.registry.signIn({ userId: s.userId, deviceKey: s.aKey, challenge, signature: bare }, T0),
    'BadSignature',
  );
});
