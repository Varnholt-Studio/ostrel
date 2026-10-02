# Identity spike: signed device link (ARCHITECTURE 8, D33)

Throwaway spike, never shipped or imported. It answers one question: can a signed in device
add a second device key to the user with a signed link, using WebCrypto Ed25519 only, so that
the server can enforce expiry, single use, possession of the new key and revocation (AC-63 (d))?
The user key itself (generate, export, import) is the separate part under
`spikes/t4-identity/keys/`.

## Files

* `link.mjs`: statement layout, signing on the old device, possession proof on the new device,
  strict parsing of the text form. WebCrypto only, same code in Node and Chromium.
* `server.mjs`: in memory stand in for the server checks: register a user key, accept a link,
  revoke a key, challenge based sign in.
* `link.test.mjs`: Node tests, run by the gate (`node --test`).

## Keys of a user

* The user key (D33) is the user: the user id is its raw public key. Registration proves
  possession of it by signing a server challenge (`ostrel-register-v1\0 || challenge`), so an
  id without a matching private key cannot be registered.
* The user key is always active: it signs in and signs links like a device key, and it can
  never be revoked. After importing the exported bundle on a new browser, the user signs in with
  the user key and links the new browser's device key. This is the recovery path, so revoking
  every device key is allowed.
* Device keys are added by a link only. A key belongs to one user for ever; a user key is never
  added as a device key and a device key is never registered as a user key.

## Flow

1. Device A (signed in, key active for the user) calls `signLink` with the user id and the
   raw public key of device B. B shows its public key to A, for example as a QR code.
2. B calls `presentLink`: it checks that the link names its own key and signs the statement
   with its private key (possession proof). It sends link and proof to the server.
3. The server (`Registry.acceptLink`) checks, in this order: exact text form, signer is an
   active key of the user named in the statement, signer signature, possession proof,
   signer still active (checked again after the asynchronous signature checks, so a revocation
   that lands in between wins), expiry against the server clock, lifetime cap, unused nonce,
   the new key belongs to no user. Then it adds the key in the same synchronous block.

## Wire form

One line: `ostrel-device-link-v1.<statement>.<signature>`, both unpadded base64url, only the
canonical encoding is accepted (160 and 86 characters).

The statement has a fixed layout of 120 bytes, so there is exactly one way to read it:

| Bytes | Field |
|---|---|
| 0..32 | user id |
| 32..64 | new device public key (raw Ed25519) |
| 64..96 | signer public key (raw Ed25519) |
| 96..104 | expiry, milliseconds since the Unix epoch, u64 big endian |
| 104..120 | random nonce |

Signatures carry a context prefix: the signer signs `ostrel-device-link-v1/statement\0 || statement`,
the new device signs `ostrel-device-link-v1/possession\0 || statement`, sign in signs
`ostrel-sign-in-v1\0 || challenge`, registration signs `ostrel-register-v1\0 || challenge`.
No signature of one kind is valid as another.

## Results (2026-10-02, Node 22.22)

| Check | Result |
|---|---|
| Link adds the second device key; that key signs in | pass |
| Link rejected at and after expiry (10 minutes), accepted 1 ms before | pass |
| Expiry longer than 10 minutes plus 60 s skew (server clock) rejected | pass |
| Link used a second time rejected | pass |
| Signer unknown, of another user, or revoked: rejected | pass |
| Revoked device key cannot sign in and cannot be linked again | pass |
| Registration refuses an id without its private key, another key's or a sign in signature | pass |
| Two concurrent registrations of one user key: exactly one wins | pass |
| User key cannot be revoked; imported user key signs in and links a device after all device keys are revoked | pass |
| User key never becomes a device key; device key never becomes a user key | pass |
| Signer revoked during link check, device key revoked during sign in: refused | pass |
| Changed statement byte, foreign or mismatched possession proof: rejected | pass |
| Malformed or non canonical text, out of range expiry: rejected as `Malformed` | pass |
| Sign in challenge single use, expires after 60 s, context separated | pass |

## Findings

1. Raw 32 byte Ed25519 public keys import and verify with WebCrypto in Node 22 without any
   library; Chromium support for the same calls is shown by the keys spike (T31).
2. The expiry is written by the signing device, but the server is the only authority for time
   (ARCHITECTURE 1, goal 5). The server therefore also caps the remaining lifetime with its own
   clock; otherwise a device could mint a link that lives for days.
3. Without a possession proof any party that sees B's public key could have it added to a
   user. The proof costs one signature and closes that.
4. Every check that precedes an `await` can be outdated after it. Each state check that guards a
   write is repeated after the last `await`, in the block that writes; tests revoke a key while
   the signature check is pending.
5. Once ownership of a key is permanent (also after revocation), a replayed link is already
   refused; the nonce check stays as a second barrier and gives a clear reason (`Replayed`).
6. 64 byte signatures in base64url have 4 unused bits; without the canonical check the same
   link has 16 text forms.

## Assumptions

* ASSUMPTION: the user id is the raw 32 byte public key of the user key (D33). If the server
  assigns another id form, registration must bind that id to the user key instead.
* ASSUMPTION: a link may be signed by the user key or by any active device key of the user.
* ASSUMPTION: the user key cannot be revoked; a leaked user key means a new user (D33: the
  keypair is the user). A revoked device key never returns.
* ASSUMPTION: tolerated clock lead of the signing device is 60 s. Link lifetime (10 minutes)
  and challenge lifetime (60 s) belong in the limits table (SPEC Q16).

## Not covered

The transport from A to B (QR code or copy and paste), rate limits on link acceptance (5.9),
persistence of the registry, the CLI revocation path, and a run in headless Chromium.
