// Spike (ARCHITECTURE 8, D33): the server side of the signed device link, in memory.
// Stands in for the checks that ostrel_auth and the generated server will make. Never shipped.
//
// Every input is hostile: the link and the proof are text from the network, `now` is the
// server clock (the only clock that counts, ARCHITECTURE 1 goal 5).

import {
  ALG,
  LINK_TTL_MS,
  decodeStatement,
  deviceMessage,
  parseLink,
  parseProof,
  signerMessage,
  toB64url,
} from './link.mjs';

export const CHALLENGE_TTL_MS = 60 * 1000; // ARCHITECTURE 8: challenges expire after 60 s
export const CLOCK_SKEW_MS = 60 * 1000; // tolerated lead of the signing device's clock
const CHALLENGE_LEN = 32;
const SIGN_IN_CONTEXT = new TextEncoder().encode('ostrel-sign-in-v1\0');
const REGISTER_CONTEXT = new TextEncoder().encode('ostrel-register-v1\0');

/** Error with a stable kind, so tests check the reason and not only the failure. */
export class LinkError extends Error {
  constructor(kind) {
    super(kind);
    this.kind = kind;
  }
}

async function verify(rawKey, signature, message) {
  try {
    const key = await crypto.subtle.importKey('raw', rawKey, ALG, false, ['verify']);
    return await crypto.subtle.verify(ALG, key, signature, message);
  } catch {
    return false;
  }
}

function withContext(context, challenge) {
  const out = new Uint8Array(context.length + challenge.length);
  out.set(context, 0);
  out.set(challenge, context.length);
  return out;
}

export function signInMessage(challenge) {
  return withContext(SIGN_IN_CONTEXT, challenge);
}

export function registerMessage(challenge) {
  return withContext(REGISTER_CONTEXT, challenge);
}

function check32(bytes) {
  if (!(bytes instanceof Uint8Array) || bytes.length !== 32) throw new LinkError('Malformed');
}

export class Registry {
  constructor() {
    this.users = new Map(); // user id (b64url) -> Map(device key b64url -> 'active' | 'revoked')
    this.owner = new Map(); // device key (b64url) -> user id (b64url), also for revoked keys
    this.usedNonces = new Map(); // nonce (b64url) -> expiry ms
    this.challenges = new Map(); // challenge (b64url) -> expiry ms
  }

  /**
   * Registration of a new user (D33: the keypair is the user). The user id is the raw public
   * user key, and the caller proves possession of it by signing a server challenge, so no id
   * without a matching private key can be registered. Registration rules (open, invite,
   * handle) are out of scope. The user key is the root of the user: it is always active, it
   * signs in and signs links like a device key, and it can never be revoked.
   */
  async register({ userId, challenge, signature }, now) {
    check32(userId);
    this.#takeChallenge(challenge, now);
    const uid = toB64url(userId);
    if (this.users.has(uid)) throw new LinkError('UserExists');
    if (this.owner.has(uid)) throw new LinkError('DeviceKeyTaken');
    if (!(await verify(userId, signature, registerMessage(challenge)))) throw new LinkError('BadSignature');
    // State may have changed during the await: check again in the same synchronous block
    // that writes.
    if (this.users.has(uid)) throw new LinkError('UserExists');
    if (this.owner.has(uid)) throw new LinkError('DeviceKeyTaken');
    this.users.set(uid, new Map());
  }

  /** True for the user key itself and for every active device key of the user. */
  isActive(userId, key) {
    const uid = toB64url(userId);
    const keys = this.users.get(uid);
    if (!keys) return false;
    const k = toB64url(key);
    return k === uid || keys.get(k) === 'active';
  }

  /** Active device keys of the user; the user key is not listed (it is always active). */
  activeKeys(userId) {
    const keys = this.users.get(toB64url(userId));
    return keys ? [...keys].filter(([, s]) => s === 'active').map(([k]) => k) : [];
  }

  /**
   * Checks a presented link and adds the new device key to the user.
   * Order: shape, signer, signatures, lifetime, single use, key ownership.
   */
  async acceptLink({ link, proof }, now) {
    if (!Number.isSafeInteger(now)) throw new LinkError('BadClock');
    let parsed;
    let proofBytes;
    let st;
    try {
      parsed = parseLink(link);
      proofBytes = parseProof(proof);
      st = decodeStatement(parsed.statement);
    } catch {
      throw new LinkError('Malformed');
    }
    // The signer must be an active key of the user named in the statement. A key of another
    // user, an unknown key and a revoked key are all refused the same way.
    if (!this.isActive(st.userId, st.signerKey)) throw new LinkError('UnknownSigner');
    if (!(await verify(st.signerKey, parsed.signature, signerMessage(parsed.statement)))) {
      throw new LinkError('BadSignature');
    }
    if (!(await verify(st.deviceKey, proofBytes, deviceMessage(parsed.statement)))) {
      throw new LinkError('BadProof');
    }
    // The signer may have been revoked while the signatures were checked (TOCTOU): check again
    // here, after the last await, so the check and the write below happen in one synchronous block.
    if (!this.isActive(st.userId, st.signerKey)) throw new LinkError('UnknownSigner');
    if (now >= st.expiresAt) throw new LinkError('Expired');
    // The expiry comes from the signing device. The server caps it with its own clock, so a
    // device cannot mint a link that lives longer than the documented lifetime.
    if (st.expiresAt - now > LINK_TTL_MS + CLOCK_SKEW_MS) throw new LinkError('LifetimeTooLong');
    this.#prune(now);
    const nonce = toB64url(st.nonce);
    if (this.usedNonces.has(nonce)) throw new LinkError('Replayed');
    const dk = toB64url(st.deviceKey);
    const uid = toB64url(st.userId);
    // A key belongs to one user for ever; a revoked key never comes back through a link, and a
    // user key (own or of another user) is never added as a device key.
    if (this.owner.has(dk) || this.users.has(dk)) throw new LinkError('DeviceKeyTaken');
    this.usedNonces.set(nonce, st.expiresAt);
    this.users.get(uid).set(dk, 'active');
    this.owner.set(dk, uid);
    return { userId: uid, deviceKey: dk };
  }

  /**
   * Revocation of a device key from a signed in device (`actingKey`, a device key or the user
   * key) of the same user. The user key cannot be revoked: it is the user (D33) and stays the
   * recovery path, so revoking every device key is allowed.
   */
  revoke(userId, keyToRevoke, actingKey) {
    if (!this.isActive(userId, actingKey)) throw new LinkError('UnknownSigner');
    if (toB64url(keyToRevoke) === toB64url(userId)) throw new LinkError('UserKeyNotRevocable');
    if (!this.isActive(userId, keyToRevoke)) throw new LinkError('UnknownDeviceKey');
    this.users.get(toB64url(userId)).set(toB64url(keyToRevoke), 'revoked');
  }

  /** Sign in, step 1: a random single use challenge. */
  issueChallenge(now) {
    this.#prune(now);
    const challenge = crypto.getRandomValues(new Uint8Array(CHALLENGE_LEN));
    this.challenges.set(toB64url(challenge), now + CHALLENGE_TTL_MS);
    return challenge;
  }

  /** Sign in, step 2: the device signs the challenge with a registered key. */
  /** `deviceKey` may also be the user key itself, for example after importing the bundle. */
  async signIn({ userId, deviceKey, challenge, signature }, now) {
    this.#takeChallenge(challenge, now);
    if (!this.isActive(userId, deviceKey)) throw new LinkError('UnknownDeviceKey');
    if (!(await verify(deviceKey, signature, signInMessage(challenge)))) throw new LinkError('BadSignature');
    // Revoked during the await: no session.
    if (!this.isActive(userId, deviceKey)) throw new LinkError('UnknownDeviceKey');
    return { userId: toB64url(userId), deviceKey: toB64url(deviceKey) };
  }

  /** Consumes a challenge: single use, also when the rest of the request fails. */
  #takeChallenge(challenge, now) {
    const c = challenge instanceof Uint8Array ? toB64url(challenge) : '';
    const exp = this.challenges.get(c);
    if (exp === undefined) throw new LinkError('UnknownChallenge');
    this.challenges.delete(c);
    if (now >= exp) throw new LinkError('Expired');
  }

  #prune(now) {
    for (const [k, exp] of this.usedNonces) if (now >= exp) this.usedNonces.delete(k);
    for (const [k, exp] of this.challenges) if (now >= exp) this.challenges.delete(k);
  }
}

