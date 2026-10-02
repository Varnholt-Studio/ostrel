# Identity spike: user key (ARCHITECTURE 8, D33)

Throwaway spike, never shipped or imported. It answers one question: does WebCrypto
Ed25519 in Chromium support the user key as designed (generate, store in IndexedDB,
export as text, import on another browser)? The signed device link is a separate part
under `spikes/t4-identity/link/`.

## Files

* `keys.mjs`: generate, export, import, raw public key. WebCrypto only, same code in Node and Chromium.
* `vector.mjs`: RFC 8032 section 7.1 TEST 1 as a bundle (public test vector).
* `keys.test.mjs`: Node tests, run by the gate (`node --test`).
* `check.html` and `chromium.mjs`: the same checks in headless Chromium, local only (D27):
  `node chromium.mjs <chrome binary>` prints a JSON report, exit code 0 when all checks pass.

## Bundle format

One line: `ostrel-user-key-v1.<x>.<d>`, where `x` is the public key and `d` the private
seed, both 32 bytes as unpadded base64url (the JWK members). Import accepts nothing else
and proves with one signature that `x` belongs to `d`.

## Results (2026-10-02)

| Check | Node 22.22 | Chromium 141.0.7390.37 | Chrome for Testing 148.0.7778.97 |
|---|---|---|---|
| Generate extractable Ed25519 key | pass | pass | pass |
| Export and import keep the public key and signatures | pass | pass | pass |
| RFC 8032 TEST 1 signature | pass | pass | pass |
| Mismatched `x` and `d` rejected | pass | pass | pass |
| `CryptoKey` stored in IndexedDB `ostrel-keys`, still signs and exports | not run | pass | pass |

## Findings

1. Ed25519 in WebCrypto works without flags in both Chromium versions; no library is needed in the client.
2. A `CryptoKey` is stored in IndexedDB as is (structured clone) and keeps its `extractable` flag, so the
   runtime never has to hold the private key as bytes except during export.
3. Node rejects a JWK whose `x` does not match `d` at import; the WebCrypto spec does not require that,
   so the import checks the pair with a signature on every platform.
4. WebCrypto needs a secure context; `http://127.0.0.1` and `localhost` qualify, a plain LAN address does not.
5. Chromium refuses a profile written by a newer version, so the check uses a fresh profile per run.

## Not covered

Key persistence across sign out and "forget device" (runtime work, 5.6), the device link (T4-2b), and
other browsers (evidence runs on headless Chromium, D15).
