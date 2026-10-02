#!/usr/bin/env bash
# Formatting (MEASUREMENT 5.2, AC-05): cargo fmt --all -- --check.
# In quick scope only the crates in GATE_CRATES are checked (see ci/gate.sh).
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

crates="${GATE_CRATES-all}"
if [ ! -f Cargo.toml ]; then
  echo "   (no Cargo workspace, nothing to format)"
  exit 0
fi

if [ "$crates" = all ]; then
  cargo fmt --all -- --check
  exit $?
fi
if [ -z "$crates" ]; then
  echo "   (quick scope: no crate touched)"
  exit 0
fi
args=()
for c in $crates; do
  if ! [[ "$c" =~ ^[A-Za-z0-9_-]+$ ]]; then
    echo "invalid crate name in GATE_CRATES: $c"
    exit 1
  fi
  args+=(-p "$c")
done
cargo fmt "${args[@]}" -- --check
