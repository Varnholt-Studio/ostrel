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

# AC05_RULES: FILE<TAB>ERE pairs. Each FILE must hold a line, outside comments, that matches
# the anchored ERE. Together they are the AC-05 contract: the gate runs every check and is
# red when one fails, and the full scope runs fmt --check, clippy -D warnings and all tests.
AC05_RULES=(
  $'ci/gate.sh\t^[[:space:]]*for check in ci/checks/\\*\\.sh; do[[:space:]]*$'
  $'ci/gate.sh\t^[[:space:]]*if bash "\\$check"; then[[:space:]]*$'
  $'ci/gate.sh\t^[[:space:]]*failed\\+=\\("\\$check"\\)[[:space:]]*$'
  $'ci/gate.sh\t^[[:space:]]*if \\[ \\$\\{#failed\\[@\\]\\} -gt 0 \\]; then[[:space:]]*$'
  $'ci/checks/20_fmt.sh\t^[[:space:]]*cargo fmt --all -- --check[[:space:]]*$'
  $'ci/checks/25_clippy.sh\t^[[:space:]]*cargo clippy --workspace --all-targets --locked -- -D warnings( \\|\\| bad=1)?[[:space:]]*$'
  $'ci/checks/50_tests.sh\t^[[:space:]]*run cargo test --workspace --locked --no-fail-fast[[:space:]]*$'
)

# ac_05_gate_runs_fmt_clippy_tests ROOT: checks the AC05_RULES on the tree at ROOT; prints
# one line per violation and returns 1 if any. A static contract on the gate scripts: it
# proves that the commands are part of the gate, not that they pass.
ac_05_gate_runs_fmt_clippy_tests() {
  local root=$1 bad=0 rule file re
  for rule in "${AC05_RULES[@]}"; do
    file=${rule%%$'\t'*}
    re=${rule#*$'\t'}
    if [ ! -f "$root/$file" ]; then
      echo "   AC-05: $file missing"
      bad=1
    elif ! grep -vE '^[[:space:]]*#' "$root/$file" | grep -E -- "$re" >/dev/null; then
      echo "   AC-05: $file has no line matching: $re"
      bad=1
    fi
  done
  return $bad
}

# ac05_selftest: proves on scratch copies of the gate files that every rule can fail.
ac05_selftest() {
  local scratch bad=0 f
  scratch=$(mktemp -d) || return 1
  for f in ci/gate.sh ci/checks/20_fmt.sh ci/checks/25_clippy.sh ci/checks/50_tests.sh; do
    mkdir -p "$scratch/clean/$(dirname "$f")" && cp "$f" "$scratch/clean/$f" || return 1
  done
  # sel LABEL WANT SETUP: runs the rules on a fresh copy after SETUP.
  sel() {
    local label=$1 want=$2 got
    shift 2
    rm -rf "$scratch/tree" && cp -r "$scratch/clean" "$scratch/tree" || return 1
    (cd "$scratch/tree" && eval "$*") || {
      echo "   selftest '$label': setup failed"
      return 1
    }
    ac_05_gate_runs_fmt_clippy_tests "$scratch/tree" >"$scratch/out" 2>&1
    got=$?
    if [ "$got" != "$want" ]; then
      echo "   selftest '$label': want exit $want, got $got"
      sed 's/^/      /' "$scratch/out"
      return 1
    fi
  }
  sel "repository gate files pass" 0 true || bad=1
  sel "fmt without --check" 1 "sed -i 's/^\\( *cargo fmt --all\\) -- --check/\\1/' ci/checks/20_fmt.sh" || bad=1
  sel "fmt only in a comment" 1 "sed -i 's/^\\( *\\)cargo fmt --all -- --check/\\1# cargo fmt --all -- --check/' ci/checks/20_fmt.sh" || bad=1
  sel "clippy without -D warnings" 1 "sed -i 's/ -- -D warnings//' ci/checks/25_clippy.sh" || bad=1
  sel "clippy without --all-targets" 1 "sed -i 's/--workspace --all-targets/--workspace/' ci/checks/25_clippy.sh" || bad=1
  sel "tests without --workspace" 1 "sed -i 's/^\\( *run cargo test\\) --workspace/\\1/' ci/checks/50_tests.sh" || bad=1
  sel "tests check missing" 1 "rm ci/checks/50_tests.sh" || bad=1
  sel "gate runs no checks" 1 "sed -i 's|^for check in ci/checks/\\*\\.sh; do|for check in; do|' ci/gate.sh" || bad=1
  sel "gate ignores failures" 1 "sed -i '/failed+=(\"\\\$check\")/d' ci/gate.sh" || bad=1
  rm -rf "$scratch"
  return $bad
}

if ! ac05_selftest; then
  echo "   AC-05: self test failed"
  fail=1
else
  echo "   ac05_selftest ok"
fi
ac_05_gate_runs_fmt_clippy_tests "$PWD" || fail=1

# quick_scope_selftest: runs a copy of ci/gate.sh with GATE_SCOPE=quick on a scratch git
# repository whose only check prints GATE_CRATES, and proves the crate selection: a change
# to one crate selects that crate, a change under tests/, examples/ or ci/ selects the whole
# workspace (shared files there are read by several crates), moving a file out of tests/ or a
# crate counts for its old path, and a change elsewhere selects none.
quick_scope_selftest() {
  local scratch bad=0
  scratch=$(mktemp -d) || return 1
  (
    set -e
    cd "$scratch"
    git init -q -b main .
    git config user.name selftest
    git config user.email selftest@example.invalid
    mkdir -p ci/checks crates/a/src tests/errors examples docs
    cp "$OLDPWD/ci/gate.sh" ci/gate.sh
    printf 'echo "CRATES=[$GATE_CRATES]"\n' >ci/checks/00_print.sh
    printf '[package]\nname = "pkg_a"\n' >crates/a/Cargo.toml
    : >crates/a/src/lib.rs
    : >tests/errors/one.ostl
    : >examples/hello.ostl
    : >docs/notes.txt
    git add -A
    git commit -q -m base
    git branch base
  ) >/dev/null 2>&1 || {
    echo "   quick scope selftest: setup failed"
    rm -rf "$scratch"
    return 1
  }
  # qs LABEL WANT FILE: appends to FILE on a fresh branch and expects GATE_CRATES=WANT.
  qs() {
    local label=$1 want=$2 file=$3 got
    got=$(cd "$scratch" && git checkout -q -f base 2>/dev/null && git clean -qfd \
      && echo x >>"$file" && GATE_SCOPE=quick GATE_BASE=base bash ci/gate.sh 2>&1 \
      | sed -n 's/^CRATES=\[\(.*\)\]$/\1/p')
    if [ "$got" != "$want" ]; then
      echo "   quick scope selftest '$label': want crates [$want], got [$got]"
      return 1
    fi
  }
  # qm LABEL WANT FROM TO: commits 'git mv FROM TO' on a fresh branch and expects WANT.
  qm() {
    local label=$1 want=$2 from=$3 to=$4 got
    got=$(cd "$scratch" && git checkout -q -f base 2>/dev/null && git clean -qfd \
      && git checkout -q -B qm-move && git mv "$from" "$to" && git commit -q -m move \
      && GATE_SCOPE=quick GATE_BASE=base bash ci/gate.sh 2>&1 \
      | sed -n 's/^CRATES=\[\(.*\)\]$/\1/p')
    if [ "$got" != "$want" ]; then
      echo "   quick scope selftest '$label': want crates [$want], got [$got]"
      return 1
    fi
  }
  qs "crate change selects the crate" pkg_a crates/a/src/lib.rs || bad=1
  qs "tests/ change selects the workspace" all tests/errors/one.ostl || bad=1
  qs "new file under tests/ selects the workspace" all tests/errors/two.ostl || bad=1
  qs "ci/ change selects the workspace" all ci/checks/00_print.sh || bad=1
  qs "examples/ change selects the workspace" all examples/hello.ostl || bad=1
  qs "other change selects no crate" "" docs/notes.txt || bad=1
  qm "move out of tests/ selects the workspace" all tests/errors/one.ostl docs/one.ostl || bad=1
  qm "move out of examples/ selects the workspace" all examples/hello.ostl docs/hello.ostl \
    || bad=1
  qm "move out of a crate selects the crate" pkg_a crates/a/src/lib.rs docs/lib.rs || bad=1
  rm -rf "$scratch"
  return $bad
}

if ! quick_scope_selftest; then
  echo "   quick scope: self test failed"
  fail=1
else
  echo "   quick_scope_selftest ok"
fi

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
