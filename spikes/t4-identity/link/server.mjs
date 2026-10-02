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

export function signInMessage(challenge) {
  const out = new Uint8Array(SIGN_IN_CONTEXT.length + challenge.length);
  out.set(SIGN_IN_CONTEXT, 0);
  out.set(challenge, SIGN_IN_CONTEXT.length);
  return out;
}

export class Registry {
  constructor() {
    this.users = new Map(); // user id (b64url) -> Map(device key b64url -> 'active' | 'revoked')
    this.owner = new Map(); // device key (b64url) -> user id (b64url), also for revoked keys
    this.usedNonces = new Map(); // nonce (b64url) -> expiry ms
    this.challenges = new Map(); // challenge (b64url) -> expiry ms
  }

  /** Registration of a new user with its first device key (registration rules are out of scope). */
  register(userId, deviceKey) {
    const uid = toB64url(userId);
    const dk = toB64url(deviceKey);
    if (this.users.has(uid)) throw new LinkError('UserExists');
    if (this.owner.has(dk)) throw new LinkError('DeviceKeyTaken');
    this.users.set(uid, new Map([[dk, 'active']]));
    this.owner.set(dk, uid);
  }

  isActive(userId, deviceKey) {
    return this.users.get(toB64url(userId))?.get(toB64url(deviceKey)) === 'active';
  }

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
    if (now >= st.expiresAt) throw new LinkError('Expired');
    // The expiry comes from the signing device. The server caps it with its own clock, so a
    // device cannot mint a link that lives longer than the documented lifetime.
    if (st.expiresAt - now > LINK_TTL_MS + CLOCK_SKEW_MS) throw new LinkError('LifetimeTooLong');
    this.#prune(now);
    const nonce = toB64url(st.nonce);
    if (this.usedNonces.has(nonce)) throw new LinkError('Replayed');
    const dk = toB64url(st.deviceKey);
    const uid = toB64url(st.userId);
    // A key belongs to one user for ever; a revoked key never comes back through a link.
    if (this.owner.has(dk)) throw new LinkError('DeviceKeyTaken');
    this.usedNonces.set(nonce, st.expiresAt);
    this.users.get(uid).set(dk, 'active');
    this.owner.set(dk, uid);
    return { userId: uid, deviceKey: dk };
  }

  /** Revocation from a signed in device (`actingKey`) of the same user. */
  revoke(userId, keyToRevoke, actingKey) {
    if (!this.isActive(userId, actingKey)) throw new LinkError('UnknownSigner');
    if (!this.isActive(userId, keyToRevoke)) throw new LinkError('UnknownDeviceKey');
    if (this.activeKeys(userId).length === 1) throw new LinkError('LastKey');
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
  async signIn({ userId, deviceKey, challenge, signature }, now) {
    const c = toB64url(challenge);
    const exp = this.challenges.get(c);
    if (exp === undefined) throw new LinkError('UnknownChallenge');
    this.challenges.delete(c); // single use, also when the rest fails
    if (now >= exp) throw new LinkError('Expired');
    if (!this.isActive(userId, deviceKey)) throw new LinkError('UnknownDeviceKey');
    if (!(await verify(deviceKey, signature, signInMessage(challenge)))) throw new LinkError('BadSignature');
    return { userId: toB64url(userId), deviceKey: toB64url(deviceKey) };
  }

  #prune(now) {
    for (const [k, exp] of this.usedNonces) if (now >= exp) this.usedNonces.delete(k);
    for (const [k, exp] of this.challenges) if (now >= exp) this.challenges.delete(k);
  }
}

