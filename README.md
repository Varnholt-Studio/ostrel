# Ostrel programming language

Ostrel is a full-stack programming language for web applications. Data models, permissions,
the boundary between browser and server, and offline synchronization are part of the language,
so the glue code that usually connects them (REST or GraphQL layers, state managers, ORMs) does
not have to be written by hand.

> **Status: early development.** Nothing here is ready for use yet. The first milestones are
> tracked in [CHANGELOG.md](CHANGELOG.md).

## Goals

- A new, consistent syntax that is quick to learn.
- A compiler written in Rust, with self-hosting as the long-term goal.
- Built-in conflict-free replicated data for offline-first, real-time apps.
- A small, clean database interface. PostgreSQL and SQLite first, MySQL and MongoDB later.
- Bridges to Rust, TypeScript/JavaScript and Go.
- Optional, keypair-based authentication (PASETO).
- Honest, reproducible benchmarks, including the cases where Ostrel is slower.

## Branches

| Branch | Purpose |
|---|---|
| `main` | Released versions only. Every commit on `main` is tagged. |
| `dev` | Integration branch. Always green, never pushed to directly. |
| `gh-pages` | Published documentation site, built from a release. |
| `packaging` | Distribution packages, built from a release. |
| `<team>/<name>/<task>-<topic>` | Work in progress, one branch per task. |

See [CONTRIBUTING.md](CONTRIBUTING.md) for the workflow.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in Ostrel shall be dual licensed as above, without any additional terms or conditions.

## Contact

| Topic | Address |
|---|---|
| General | contact@elchi.dev |
| Licensing and legal | legal@elchi.dev |
| This GitHub organization | github@elchi.dev |

Ostrel is developed by Varnholt Studio. It is not affiliated with any other company of a similar name.
