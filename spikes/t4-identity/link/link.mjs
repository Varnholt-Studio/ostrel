// Spike (ARCHITECTURE 8, D33): the signed device link, client side.
// WebCrypto only, runs unchanged in Node 22 and in Chromium. Never shipped, never imported.
//
// A signed in device signs a link statement that names the user, the new device's public
// key, the signing key, an expiry and a nonce. The new device adds a signature of its own
// over the same statement (proof that it holds the private key) and presents both to the
// server, which checks them in server.mjs.

export const ALG = { name: 'Ed25519' };
export const PREFIX = 'ostrel-device-link-v1';
export const LINK_TTL_MS = 10 * 60 * 1000; // ARCHITECTURE 8: expiry 10 minutes
export const NONCE_LEN = 16;

// Fixed layout, no variable fields, so there is exactly one way to read a statement:
//   user id (32) | new device key (32) | signer key (32) | expiry ms, u64 big endian (8) | nonce (16)
const OFF_USER = 0;
const OFF_DEVICE = 32;
const OFF_SIGNER = 64;
const OFF_EXP = 96;
const OFF_NONCE = 104;
export const STATEMENT_LEN = 120;

// Domain separation: a link signature can never be mistaken for a sign in signature or any
// other message signed with the same key, and the new device's proof differs from the
// signer's signature.
const enc = new TextEncoder();
const SIGNER_CONTEXT = enc.encode(`${PREFIX}/statement\0`);
const DEVICE_CONTEXT = enc.encode(`${PREFIX}/possession\0`);

const B64URL_STATEMENT = /^[A-Za-z0-9_-]{160}$/; // 120 bytes
const B64URL_SIG = /^[A-Za-z0-9_-]{86}$/; // 64 bytes

export function toB64url(bytes) {
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

export function fromB64url(text) {
  const bin = atob(text.replace(/-/g, '+').replace(/_/g, '/'));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function concat(...parts) {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

function check32(name, bytes) {
  if (!(bytes instanceof Uint8Array) || bytes.length !== 32) throw new Error(`${name} must be 32 bytes`);
}

/** Raw 32 byte public key of a WebCrypto keypair. */
export async function rawPublicKey(keyPair) {
  return new Uint8Array(await crypto.subtle.exportKey('raw', keyPair.publicKey));
}

/** Encodes a statement into its fixed 120 byte layout. */
export function encodeStatement({ userId, deviceKey, signerKey, expiresAt, nonce }) {
  check32('userId', userId);
  check32('deviceKey', deviceKey);
  check32('signerKey', signerKey);
  if (!(nonce instanceof Uint8Array) || nonce.length !== NONCE_LEN) throw new Error('nonce must be 16 bytes');
  if (!Number.isSafeInteger(expiresAt) || expiresAt < 0) throw new Error('expiresAt must be a safe integer');
  const exp = new Uint8Array(8);
  new DataView(exp.buffer).setBigUint64(0, BigInt(expiresAt));
  return concat(userId, deviceKey, signerKey, exp, nonce);
}

/** Decodes a 120 byte statement. */
export function decodeStatement(bytes) {
  if (!(bytes instanceof Uint8Array) || bytes.length !== STATEMENT_LEN) throw new Error('bad statement length');
  const exp = new DataView(bytes.buffer, bytes.byteOffset + OFF_EXP, 8).getBigUint64(0);
  if (exp > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error('expiry out of range');
  return {
    userId: bytes.slice(OFF_USER, OFF_DEVICE),
    deviceKey: bytes.slice(OFF_DEVICE, OFF_SIGNER),
    signerKey: bytes.slice(OFF_SIGNER, OFF_EXP),
    expiresAt: Number(exp),
    nonce: bytes.slice(OFF_NONCE, STATEMENT_LEN),
  };
}

export function signerMessage(statement) {
  return concat(SIGNER_CONTEXT, statement);
}

export function deviceMessage(statement) {
  return concat(DEVICE_CONTEXT, statement);
}

/**
 * Step 1, on the signed in device: signs a link for the new device's public key.
 * `now` is the signing device's clock; the server enforces the lifetime with its own clock.
 * Returns the line `ostrel-device-link-v1.<statement>.<signature>`.
 */
export async function signLink({ signerPair, userId, deviceKey, now, ttlMs = LINK_TTL_MS }) {
  const nonce = crypto.getRandomValues(new Uint8Array(NONCE_LEN));
  const signerKey = await rawPublicKey(signerPair);
  const statement = encodeStatement({ userId, deviceKey, signerKey, expiresAt: now + ttlMs, nonce });
  const sig = new Uint8Array(await crypto.subtle.sign(ALG, signerPair.privateKey, signerMessage(statement)));
  return `${PREFIX}.${toB64url(statement)}.${toB64url(sig)}`;
}

/**
 * Step 2, on the new device: proves possession of the key named in the link.
 * Returns `{ link, proof }`, which the new device sends to the server.
 */
export async function presentLink({ link, devicePair }) {
  const { statement } = parseLink(link);
  const own = await rawPublicKey(devicePair);
  const named = decodeStatement(statement).deviceKey;
  if (!equalBytes(own, named)) throw new Error('link names another device key');
  const proof = new Uint8Array(await crypto.subtle.sign(ALG, devicePair.privateKey, deviceMessage(statement)));
  return { link, proof: toB64url(proof) };
}

/** Splits and validates the text form. Rejects everything but the exact canonical form. */
export function parseLink(text) {
  const parts = typeof text === 'string' ? text.split('.') : [];
  if (parts.length !== 3 || parts[0] !== PREFIX || !B64URL_STATEMENT.test(parts[1]) || !B64URL_SIG.test(parts[2])) {
    throw new Error('not a device link');
  }
  const statement = fromB64url(parts[1]);
  const signature = fromB64url(parts[2]);
  // Non canonical base64url (unused trailing bits set) would give two texts for one link.
  if (toB64url(statement) !== parts[1] || toB64url(signature) !== parts[2]) throw new Error('not a device link');
  return { statement, signature };
}

export function parseProof(text) {
  if (typeof text !== 'string' || !B64URL_SIG.test(text)) throw new Error('bad proof');
  const proof = fromB64url(text);
  if (toB64url(proof) !== text) throw new Error('bad proof');
  return proof;
}

export function equalBytes(a, b) {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}
