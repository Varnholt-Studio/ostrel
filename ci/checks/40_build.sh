#!/usr/bin/env bash
# Release build of the whole workspace (AC-01, MEASUREMENT 5.2).
# GATE_SCOPE=quick skips it: builders run no release build (MEASUREMENT 5.2, RED-A 29).
# 45_hostile.sh uses the binary built here.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1
if [ "${GATE_SCOPE:-full}" = quick ]; then
  echo "   build: skipped (GATE_SCOPE=quick, no release build)"
  exit 0
fi
[ -f Cargo.toml ] || { echo "   build: no Cargo.toml"; exit 1; }
echo "   -> cargo build --workspace --locked --release"
cargo build --workspace --locked --release
