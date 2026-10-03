#!/usr/bin/env bash
# Toolchain pin and lockfile (MEASUREMENT 5.2, AC-01):
#   rust-toolchain.toml pins an exact release (X.Y.Z, never stable, beta or nightly),
#   the active rustc is that release, the workspace rust-version does not exceed it,
#   Cargo.lock exists (and is tracked in a git checkout),
#   cargo metadata --locked --offline succeeds.
# The release build itself (cargo build --locked --release) is ac_01_release_build in
# 40_build.sh.
#
# A self test (toolchain_selftest) runs first on scratch trees and proves that the pin and
# lockfile rules can fail. rustc is always asked in the repository, so the self test never
# makes rustup resolve another toolchain.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1
REPO=$PWD

# ac_01_toolchain_pin ROOT ACTIVE: checks rust-toolchain.toml and the rust-version of the
# tree at ROOT against ACTIVE, the version the active rustc reports.
ac_01_toolchain_pin() {
  local root=$1 active=$2 bad=0 file=rust-toolchain.toml channel count rv lowest
  if [ ! -f "$root/$file" ]; then
    echo "$file: missing"
    return 1
  fi
  channel=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$root/$file")
  count=$(printf '%s' "$channel" | grep -c '' || true)
  if [ "$count" != 1 ]; then
    echo "$file: expected exactly one channel entry, found $count"
    bad=1
  elif ! [[ "$channel" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "$file: channel \"$channel\" is not an exact release such as 1.95.0"
    bad=1
  else
    if [ "$active" != "$channel" ]; then
      echo "rustc reports \"$active\", but $file pins $channel"
      bad=1
    fi
    rv=$(sed -n 's/^rust-version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$root/Cargo.toml" 2>/dev/null | head -n 1)
    if [ -n "$rv" ]; then
      lowest=$(printf '%s\n%s\n' "$rv" "$channel" | sort -V | head -n 1)
      if [ "$lowest" != "$rv" ]; then
        echo "Cargo.toml: rust-version $rv is newer than the pinned toolchain $channel"
        bad=1
      fi
    fi
  fi
  return $bad
}

# ac_01_lockfile ROOT: Cargo.lock exists in ROOT and, in a git checkout, is tracked.
ac_01_lockfile() {
  local root=$1
  if [ ! -f "$root/Cargo.lock" ]; then
    echo "Cargo.lock: missing"
    return 1
  fi
  if git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1 &&
    ! git -C "$root" ls-files --error-unmatch Cargo.lock >/dev/null 2>&1; then
    echo "Cargo.lock: exists but is not committed"
    return 1
  fi
}

toolchain_selftest() {
  local scratch bad=0
  scratch=$(mktemp -d) || return 1
  mkdir -p "$scratch/clean" || return 1
  printf '[toolchain]\nchannel = "1.95.0"\n' >"$scratch/clean/rust-toolchain.toml"
  printf '[workspace]\n\n[workspace.package]\nrust-version = "1.95"\n' >"$scratch/clean/Cargo.toml"
  printf 'version = 4\n' >"$scratch/clean/Cargo.lock"
  # sel LABEL WANT ACTIVE SETUP: runs both rules on a fresh copy after SETUP.
  sel() {
    local label=$1 want=$2 active=$3 got=0
    shift 3
    rm -rf "$scratch/tree" && cp -r "$scratch/clean" "$scratch/tree" || return 1
    (cd "$scratch/tree" && eval "$*") || {
      echo "   selftest '$label': setup failed"
      return 1
    }
    {
      ac_01_toolchain_pin "$scratch/tree" "$active" || got=1
      ac_01_lockfile "$scratch/tree" || got=1
    } >"$scratch/out" 2>&1
    if [ "$got" != "$want" ]; then
      echo "   selftest '$label': want exit $want, got $got"
      sed 's/^/      /' "$scratch/out"
      return 1
    fi
  }
  sel "clean tree passes" 0 1.95.0 true || bad=1
  sel "clean git checkout passes" 0 1.95.0 "git init -q . && git add -A" || bad=1
  sel "toolchain file missing" 1 1.95.0 "rm rust-toolchain.toml" || bad=1
  sel "channel stable" 1 1.95.0 "sed -i 's/1.95.0/stable/' rust-toolchain.toml" || bad=1
  sel "channel without patch" 1 1.95.0 "sed -i 's/1.95.0/1.95/' rust-toolchain.toml" || bad=1
  sel "channel twice" 1 1.95.0 "printf 'channel = \"1.95.0\"\n' >>rust-toolchain.toml" || bad=1
  sel "active rustc differs" 1 1.94.1 true || bad=1
  sel "rust-version newer than pin" 1 1.95.0 "sed -i 's/1.95\"/1.96\"/' Cargo.toml" || bad=1
  sel "Cargo.lock missing" 1 1.95.0 "rm Cargo.lock" || bad=1
  sel "Cargo.lock not tracked" 1 1.95.0 "git init -q . && git add rust-toolchain.toml Cargo.toml" || bad=1
  rm -rf "$scratch"
  return $bad
}

if ! toolchain_selftest; then
  echo "   TOOLCHAIN: self test failed"
  exit 1
fi
echo "   toolchain_selftest ok"

cd "$REPO" || exit 1
bad=0
ac_01_toolchain_pin "$REPO" "$(rustc --version 2>/dev/null | awk '{print $2}')" || bad=1
ac_01_lockfile "$REPO" || bad=1
if ! cargo metadata --locked --offline --format-version 1 >/dev/null; then
  echo "cargo metadata --locked --offline failed (lockfile out of date or dependencies not fetched)"
  bad=1
fi
exit $bad
