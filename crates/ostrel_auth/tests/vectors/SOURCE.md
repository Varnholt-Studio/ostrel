# PASETO v4 test vectors: source

| Field | Value |
|---|---|
| Upstream repository | https://github.com/paseto-standard/test-vectors |
| Upstream file | `v4.json` |
| Upstream URL | https://github.com/paseto-standard/test-vectors/blob/32d7406591eb022f9eff88abb84106dd9d42c0f2/v4.json |
| Upstream commit | `32d7406591eb022f9eff88abb84106dd9d42c0f2` (2024-04-28, "Fix v3.public") |
| sha256 of `v4.json` | `0b72948b65d1f73f574c9ad2aa3481ec27bf8c632f5f6e1596cd41f5b9703387` |
| Upstream license | ISC, copyright 2021 Paragon Initiative Enterprises, full text in `LICENSE` next to this file |
| Fetched | 2026-10-02, once, under the network policy of ARCHITECTURE 11.1 |

`v4.json` is committed byte for byte as published upstream. The gate never downloads it.
The ISC license permits copying and redistribution provided the copyright notice and the
permission notice are kept, which `LICENSE` in this directory does.

The test `../paseto_v4_vectors.test.mjs` checks the sha256 above against the committed file,
so any edit to `v4.json` fails the gate until this table is updated in the same commit.

## Refresh

A refresh is a new commit of the same form: replace `v4.json` and `LICENSE` with the upstream
files at a newer commit and update every row of the table above.

## Scope

Only the `v4.public` vectors (`4-S-*`) and the failure vectors (`4-F-*`) concern
`ostrel_auth`. The `v4.local` vectors (`4-E-*`) are kept because the file is committed
unchanged; Ostrel does not use `v4.local`.
