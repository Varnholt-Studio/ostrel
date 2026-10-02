#!/usr/bin/env bash
# Runs every test suite found in the repository, offline and without package installs.
#   Rust:       cargo test --workspace --locked --no-fail-fast (when Cargo.toml exists);
#               with GATE_SCOPE=quick only the crates in GATE_CRATES (set by ci/gate.sh)
#   JavaScript: node --test on *.test.js and *.test.mjs (built in runner, no npm)
#   Python:     unittest (or pytest when available) on test_*.py
#   Go:         go test ./... (when go.mod exists)
# A language that has code but no tests fails the check. Type declarations (.d.ts),
# examples, fixtures, test data, benchmarks and the ci/ directory are not counted as code.
set -uo pipefail
cd "$(dirname "$0")/../.."

fail=0
run() {
  echo "   -> $*"
  "$@" || fail=1
}

# find(1) over the repository without generated, vendored or non shipped trees.
# Prints NUL separated paths.
repo_find() {
  find . \( -path ./.git -o -path ./target -o -path '*/node_modules' -o -path ./bench \
    -o -path ./ci \) -prune -o -type f \( "$@" \) -print0
}

# Removes data and example locations from a NUL separated path list.
not_data() {
  grep -zvE '/(examples|fixtures|testdata|golden|goldens)/' || true
}

# list NAME FIND_EXPRESSION...: stores the matching paths in the array NAME.
list() {
  local -n out=$1
  shift
  mapfile -d '' out < <(repo_find "$@" | not_data | sort -z)
}

# rust_picks: the cargo test arguments for GATE_SCOPE=quick. The crate selection is the
# one of ci/gate.sh (GATE_CRATES), so fmt, clippy and tests see the same set: "all" or
# unset gives "--workspace"; a space separated list of package names gives "-p NAME" per
# name; an empty list gives nothing (no crate touched). A malformed name falls back to
# "--workspace", never to a test filter.
rust_picks() {
  local name
  local -a names=() out=()
  read -ra names <<< "${GATE_CRATES-all}"
  for name in "${names[@]}"; do
    if [ "$name" = all ] || ! [[ "$name" =~ ^[A-Za-z0-9_-]+$ ]]; then
      echo --workspace
      return
    fi
    out+=(-p "$name")
  done
  [ ${#out[@]} -gt 0 ] && printf '%s\n' "${out[@]}"
  return 0
}

# Rust
list rs_code -name '*.rs'
if [ -f Cargo.toml ] && [ "${GATE_SCOPE:-full}" = quick ]; then
  mapfile -t picks < <(rust_picks)
  if [ ${#picks[@]} -gt 0 ]; then
    run cargo test --locked --no-fail-fast "${picks[@]}"
  else
    echo "   (GATE_SCOPE=quick: no crate changed, Rust tests skipped)"
  fi
elif [ -f Cargo.toml ]; then
  run cargo test --workspace --locked --no-fail-fast
elif [ ${#rs_code[@]} -gt 0 ]; then
  echo "Rust code without a Cargo workspace: ${rs_code[0]}"
  fail=1
fi

# JavaScript
list js_tests -name '*.test.js' -o -name '*.test.mjs'
list js_code \( -name '*.js' -o -name '*.mjs' -o -name '*.cjs' -o -name '*.ts' \) -a ! -name '*.d.ts'
if [ ${#js_tests[@]} -gt 0 ]; then
  if command -v node >/dev/null 2>&1; then
    run node --test "${js_tests[@]}"
  else
    echo "JavaScript tests found but node is not installed"
    fail=1
  fi
elif [ ${#js_code[@]} -gt 0 ]; then
  echo "JavaScript code without tests: ${js_code[0]}"
  fail=1
fi

# Python
list py_tests -name 'test_*.py'
list py_code -name '*.py'
if [ ${#py_tests[@]} -gt 0 ]; then
  rc=0
  if python3 -c 'import pytest' 2>/dev/null; then
    echo "   -> python3 -m pytest -q (${#py_tests[@]} files)"
    python3 -m pytest -q "${py_tests[@]}"
    rc=$?
  else
    mapfile -d '' py_dirs < <(for f in "${py_tests[@]}"; do printf '%s\0' "$(dirname "$f")"; done | sort -zu)
    for d in "${py_dirs[@]}"; do
      echo "   -> python3 -m unittest discover -s $d"
      python3 -m unittest discover -s "$d" -t "$d" -p 'test_*.py'
      r=$?
      if [ $r -ne 0 ] && [ $r -ne 5 ]; then rc=$r; fi
    done
  fi
  # Exit code 5 means that no tests were collected, which is not a failure.
  if [ $rc -ne 0 ] && [ $rc -ne 5 ]; then fail=1; fi
elif [ ${#py_code[@]} -gt 0 ]; then
  echo "Python code without tests: ${py_code[0]}"
  fail=1
fi

# Go
if [ -f go.mod ]; then
  run go test ./...
fi

if [ ${#rs_code[@]} -eq 0 ] && [ ${#js_code[@]} -eq 0 ] && [ ${#py_code[@]} -eq 0 ] && [ ! -f go.mod ]; then
  echo "   (no code yet, nothing to test)"
fi
exit $fail
