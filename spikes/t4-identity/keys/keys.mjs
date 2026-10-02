// Spike (ARCHITECTURE 8, D33): the Ed25519 user key with WebCrypto only.
// Runs unchanged in Node 22 and in Chromium. Never shipped, never imported.

const ALG = { name: 'Ed25519' };
const PREFIX = 'ostrel-user-key-v1';
const B64URL = /^[A-Za-z0-9_-]{43}$/; // 32 bytes, unpadded base64url

/** Generates an extractable user keypair. */
export function generateUserKey() {
  return crypto.subtle.generateKey(ALG, true, ['sign', 'verify']);
}

/** Exports the keypair as one line of text: `ostrel-user-key-v1.<x>.<d>`. */
export async function exportUserKey(keyPair) {
  const jwk = await crypto.subtle.exportKey('jwk', keyPair.privateKey);
  return `${PREFIX}.${jwk.x}.${jwk.d}`;
}

/**
 * Imports a bundle made by exportUserKey. Rejects anything else, including a
 * bundle whose public half does not belong to its private half.
 */
export async function importUserKey(text) {
  const parts = typeof text === 'string' ? text.trim().split('.') : [];
  if (parts.length !== 3 || parts[0] !== PREFIX || !B64URL.test(parts[1]) || !B64URL.test(parts[2])) {
    throw new Error('not a user key bundle');
  }
  const [, x, d] = parts;
  const base = { kty: 'OKP', crv: 'Ed25519', x };
  // Node rejects a mismatched x and d at import, the WebCrypto spec does not
  // require it, so a signature proves the pair on every platform.
  const probe = new TextEncoder().encode(PREFIX);
  try {
    const privateKey = await crypto.subtle.importKey('jwk', { ...base, d }, ALG, true, ['sign']);
    const publicKey = await crypto.subtle.importKey('jwk', base, ALG, true, ['verify']);
    const sig = await crypto.subtle.sign(ALG, privateKey, probe);
    if (await crypto.subtle.verify(ALG, publicKey, sig, probe)) return { privateKey, publicKey };
  } catch {
    // Reported below with one message for every platform.
  }
  throw new Error('user key bundle is inconsistent');
}

/** Raw 32 byte public key, the user's identity on the server. */
export async function publicKeyBytes(keyPair) {
  return new Uint8Array(await crypto.subtle.exportKey('raw', keyPair.publicKey));
}
