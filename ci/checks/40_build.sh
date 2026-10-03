#!/usr/bin/env bash
# Release build of the whole workspace (AC-01, MEASUREMENT 5.2).
# GATE_SCOPE=quick skips it: builders run no release build (MEASUREMENT 5.2, RED-A 29).
# 45_hostile.sh uses the binary built here.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

# ac_01_release_build: builds the workspace exactly as AC-01 states, with --locked, so a
# Cargo.lock that does not match the manifests fails the build instead of being rewritten.
ac_01_release_build() {
  [ -f Cargo.toml ] || { echo "   build: no Cargo.toml"; return 1; }
  echo "   -> cargo build --workspace --locked --release"
  cargo build --workspace --locked --release
}

if [ "${GATE_SCOPE:-full}" = quick ]; then
  echo "   build: skipped (GATE_SCOPE=quick, no release build)"
  exit 0
fi
ac_01_release_build
