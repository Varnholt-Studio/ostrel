#!/usr/bin/env bash
# Toolchain pin and lockfile (MEASUREMENT 5.2, AC-01):
#   rust-toolchain.toml pins an exact release (X.Y.Z, never stable, beta or nightly),
#   the active rustc is that release, the workspace rust-version does not exceed it,
#   Cargo.lock exists (and is tracked in a git checkout),
#   cargo metadata --locked --offline succeeds.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

bad=0
file=rust-toolchain.toml
if [ ! -f "$file" ]; then
  echo "$file: missing"
  exit 1
fi

channel=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$file")
count=$(printf '%s' "$channel" | grep -c '' || true)
if [ "$count" != 1 ]; then
  echo "$file: expected exactly one channel entry, found $count"
  bad=1
elif ! [[ "$channel" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "$file: channel \"$channel\" is not an exact release such as 1.95.0"
  bad=1
else
  active=$(rustc --version 2>/dev/null | awk '{print $2}')
  if [ "$active" != "$channel" ]; then
    echo "rustc reports \"$active\", but $file pins $channel"
    bad=1
  fi
  rv=$(sed -n 's/^rust-version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -n 1)
  if [ -n "$rv" ]; then
    lowest=$(printf '%s\n%s\n' "$rv" "$channel" | sort -V | head -n 1)
    if [ "$lowest" != "$rv" ]; then
      echo "Cargo.toml: rust-version $rv is newer than the pinned toolchain $channel"
      bad=1
    fi
  fi
fi

if [ ! -f Cargo.lock ]; then
  echo "Cargo.lock: missing"
  bad=1
elif git rev-parse --is-inside-work-tree >/dev/null 2>&1 &&
  ! git ls-files --error-unmatch Cargo.lock >/dev/null 2>&1; then
  echo "Cargo.lock: exists but is not committed"
  bad=1
fi

if ! cargo metadata --locked --offline --format-version 1 >/dev/null; then
  echo "cargo metadata --locked --offline failed (lockfile out of date or dependencies not fetched)"
  bad=1
fi

exit $bad
