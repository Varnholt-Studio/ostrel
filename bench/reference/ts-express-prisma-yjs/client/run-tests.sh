#!/usr/bin/env bash
# Runs the dependency free unit tests of the REF-A client core with Node 22 (type stripping).
# Not part of the offline gate: everything under bench/ runs in the bench job only.
set -euo pipefail
cd "$(dirname "$0")"
exec node --test test/*.test.ts
