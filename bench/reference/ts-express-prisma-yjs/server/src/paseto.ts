// PASETO v4.public (Ed25519) on node:crypto, see https://github.com/paseto-standard/paseto-spec
import { createPrivateKey, createPublicKey, sign, verify, type KeyObject } from 'node:crypto';

const HEADER = 'v4.public.';
const PKCS8_PREFIX = Buffer.from('302e020100300506032b657004220420', 'hex');
const SPKI_PREFIX = Buffer.from('302a300506032b6570032100', 'hex');

export function privateKeyFromSeed(seed: Uint8Array): KeyObject {
  return createPrivateKey({ key: Buffer.concat([PKCS8_PREFIX, seed]), format: 'der', type: 'pkcs8' });
}

export function publicKeyFromRaw(raw: Uint8Array): KeyObject {
  if (raw.length !== 32) throw new Error('Ed25519 public key must be 32 bytes');
  return createPublicKey({ key: Buffer.concat([SPKI_PREFIX, raw]), format: 'der', type: 'spki' });
}

// Pre-authentication encoding (PASETO spec, section "PAE").
export function pae(...pieces: Uint8Array[]): Buffer {
  const le64 = (n: number) => {
    const b = Buffer.alloc(8);
    b.writeBigUInt64LE(BigInt(n) & 0x7fffffffffffffffn);
    return b;
  };
  return Buffer.concat([le64(pieces.length), ...pieces.flatMap((p) => [le64(p.length), p])]);
}

export function signToken(key: KeyObject, message: Uint8Array, footer = Buffer.alloc(0), implicit = Buffer.alloc(0)): string {
  const sig = sign(null, pae(Buffer.from(HEADER), message, footer, implicit), key);
  const body = Buffer.concat([message, sig]).toString('base64url');
  return footer.length ? `${HEADER}${body}.${Buffer.from(footer).toString('base64url')}` : HEADER + body;
}

export function verifyToken(key: KeyObject, token: string, implicit = Buffer.alloc(0)): { message: Buffer; footer: Buffer } | null {
  if (!token.startsWith(HEADER)) return null;
  const parts = token.slice(HEADER.length).split('.');
  if (parts.length > 2 || parts[1] === '') return null;
  const body = Buffer.from(parts[0], 'base64url');
  const footer = Buffer.from(parts[1] ?? '', 'base64url');
  // Buffer decoding is lenient, so reject anything that is not canonical base64url.
  if (body.toString('base64url') !== parts[0] || footer.toString('base64url') !== (parts[1] ?? '')) return null;
  if (body.length < 64) return null;
  const message = body.subarray(0, body.length - 64);
  const sig = body.subarray(body.length - 64);
  const ok = verify(null, pae(Buffer.from(HEADER), message, footer, implicit), key, sig);
  return ok ? { message, footer } : null;
}

export type Claims = { sub: string; kid: string; exp: string };

export function mintToken(key: KeyObject, sub: string, kid: string, ttlMs = 24 * 3600 * 1000): string {
  const claims: Claims = { sub, kid, exp: new Date(Date.now() + ttlMs).toISOString() };
  return signToken(key, Buffer.from(JSON.stringify(claims)));
}

export function readToken(key: KeyObject, token: string): Claims | null {
  const verified = verifyToken(key, token);
  if (!verified) return null;
  try {
    const claims = JSON.parse(verified.message.toString('utf8'));
    if (typeof claims.sub !== 'string' || typeof claims.exp !== 'string') return null;
    const exp = Date.parse(claims.exp);
    return Number.isFinite(exp) && exp > Date.now() ? claims : null;
  } catch {
    return null;
  }
}
