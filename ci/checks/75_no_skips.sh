#!/usr/bin/env bash
# No skipped tests (MEASUREMENT 5.2 `75_no_skips.sh`, team rule 7). Owner: QA.
#
# A test may only be skipped, disabled or focused with an entry in ci/quarantine.txt. Found:
#   Rust    (*.rs)       #[ignore], #[cfg_attr(..., ignore)], and tests disabled by a cfg
#                        that is never true: cfg(any()), cfg(not(all())), cfg(false),
#                        cfg(FALSE)
#   JS, TS  (test files  .skip(, .only(, .todo(, xit( / xdescribe( / xtest(, fit( / fdescribe(,
#           *.test.*,    and the node:test options skip / only / todo set to true or a string
#           *.spec.*)
#   Python  (test_*.py)  unittest skip, skipIf, skipUnless, skipTest, expectedFailure, and
#                        pytest skip, skipif, xfail
# Each finding has a key `PATH NAME`: NAME is the Rust function or module after the
# attribute, the first quoted string on the JS line (the test title), or the Python function
# or class after the decorator (for a call inside a body: the enclosing function). When no
# name is found, NAME is `line:N`.
#
# ci/quarantine.txt: one entry per line, `PATH NAME TICKET OWNER`, TICKET `T<number>`, OWNER
# a team id. The check fails when a finding has no entry, an entry matches no finding (stale)
# or an entry is malformed. Quarantined Rust tests marked #[ignore] still run with
# `cargo test -- --ignored`; their result is reported and does not block. Other quarantined
# tests cannot be run by this check and are listed as not run.
#
# Generated and vendored trees (target, node_modules, .git) are not read. This file is the
# self test fixture and is not scanned (it is not a test file).
#
# A self test (no_skips_selftest) runs first on a scratch tree and proves that every rule
# above can fail.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1
REPO=$(pwd)

QUARANTINE=ci/quarantine.txt

prune_find() {
  find . \( -path ./.git -o -path ./target -o -name node_modules \) -prune \
    -o -type f \( "$@" \) -print0
}

# scan_rust FILE: prints `PATH NAME KIND` per finding.
scan_rust() {
  awk -v path="$1" '
    function emit(n) { print path, n, "rust" }
    pending != "" {
      if (match($0, /fn[[:space:]]+[A-Za-z0-9_]+/)) {
        s = substr($0, RSTART, RLENGTH); sub(/^fn[[:space:]]+/, "", s); emit(s); pending = ""; next
      }
      if (match($0, /mod[[:space:]]+[A-Za-z0-9_]+/)) {
        s = substr($0, RSTART, RLENGTH); sub(/^mod[[:space:]]+/, "", s); emit(s); pending = ""; next
      }
      if (++gap > 10) { emit(pending); pending = "" }
    }
    /^[[:space:]]*#!?\[[[:space:]]*(ignore([[:space:]]*[]=(])|cfg_attr[[:space:]]*\(.*[,(][[:space:]]*ignore([[:space:]]*[]=,)])|cfg[[:space:]]*\([[:space:]]*(any[[:space:]]*\([[:space:]]*\)|not[[:space:]]*\([[:space:]]*all[[:space:]]*\([[:space:]]*\)[[:space:]]*\)|false|FALSE)[[:space:]]*\))/ {
      if (pending != "") emit(pending)
      pending = "line:" NR; gap = 0
      if ($0 ~ /^[[:space:]]*#!\[/) { emit("line:" NR); pending = "" }
    }
    END { if (pending != "") emit(pending) }
  ' "$1"
}

# scan_js FILE: prints `PATH NAME KIND` per finding.
scan_js() {
  awk -v path="$1" '
    /(\.(skip|only|todo)[[:space:]]*\()|(^|[^A-Za-z0-9_$.])(x(it|describe|test)|f(it|describe))[[:space:]]*\(|(^|[^A-Za-z0-9_$])(skip|only|todo)[[:space:]]*:[[:space:]]*(true|["\047`])/ {
      name = "line:" NR
      if (match($0, /["\047`][^"\047`]*["\047`]/)) {
        name = substr($0, RSTART + 1, RLENGTH - 2); gsub(/[[:space:]]+/, "_", name)
        if (name == "") name = "line:" NR
      }
      print path, name, "js"
    }
  ' "$1"
}

# scan_py FILE: prints `PATH NAME KIND` per finding.
scan_py() {
  awk -v path="$1" '
    function emit(n) { print path, n, "py" }
    match($0, /^[[:space:]]*(async[[:space:]]+)?(def|class)[[:space:]]+[A-Za-z0-9_]+/) {
      s = substr($0, RSTART, RLENGTH); sub(/^.*(def|class)[[:space:]]+/, "", s)
      if (pending != "") { emit(s); pending = "" }
      current = s; next
    }
    /^[[:space:]]*@(unittest\.)?(skip|skipIf|skipUnless|expectedFailure)([^A-Za-z0-9_]|$)|^[[:space:]]*@pytest\.mark\.(skip|skipif|xfail)([^A-Za-z0-9_]|$)/ {
      pending = "line:" NR; next
    }
    /\.skipTest[[:space:]]*\(|pytest\.(skip|xfail)[[:space:]]*\(|raise[[:space:]]+(unittest\.)?SkipTest/ {
      emit(current != "" ? current : "line:" NR)
    }
    END { if (pending != "") emit(pending) }
  ' "$1"
}

# findings: every finding of the working tree, sorted, `PATH NAME KIND`.
findings() {
  local f
  {
    while IFS= read -r -d '' f; do scan_rust "${f#./}"; done < <(prune_find -name '*.rs')
    while IFS= read -r -d '' f; do scan_js "${f#./}"; done < <(prune_find \
      -name '*.test.js' -o -name '*.test.mjs' -o -name '*.test.cjs' -o -name '*.test.ts' \
      -o -name '*.test.mts' -o -name '*.spec.js' -o -name '*.spec.mjs' -o -name '*.spec.ts' \
      -o -name '*.spec.mts')
    while IFS= read -r -d '' f; do scan_py "${f#./}"; done < <(prune_find -name 'test_*.py')
  } | sort -u
}

# rules QUARANTINE FINDINGS: prints one line per violation and returns 1 if any; writes the
# quarantined Rust tests to FINDINGS.rust.
rules() {
  local q=$1 found=$2 bad=0 n=0 line path name ticket owner rest kind
  declare -A entry=() seen=()
  if [ -f "$q" ]; then
    while IFS= read -r line || [ -n "$line" ]; do
      n=$((n + 1))
      case $line in '' | '#'*) continue ;; esac
      read -r path name ticket owner rest <<<"$line"
      if [ -z "${owner:-}" ] || [ -n "${rest:-}" ] || ! [[ "$ticket" =~ ^T[0-9]+$ ]] ||
        ! [[ "$owner" =~ ^[a-z][a-z0-9_-]*$ ]]; then
        echo "   NO SKIPS: $q:$n: not of the form 'PATH NAME T<number> OWNER': $line"
        bad=1
        continue
      fi
      if [ -n "${entry["$path $name"]:-}" ]; then
        echo "   NO SKIPS: $q:$n: duplicate entry $path $name"
        bad=1
      fi
      entry["$path $name"]="$ticket $owner"
    done <"$q"
  fi
  : >"$found.rust"
  while read -r path name kind; do
    seen["$path $name"]=1
    if [ -z "${entry["$path $name"]:-}" ]; then
      echo "   NO SKIPS: $path: test $name is skipped, disabled or focused without an entry in $q"
      bad=1
    elif [ "$kind" = rust ]; then
      echo "$name" >>"$found.rust"
    fi
  done <"$found"
  for line in "${!entry[@]}"; do
    if [ -z "${seen[$line]:-}" ]; then
      echo "   NO SKIPS: $q: stale entry '$line' matches no skipped test; remove it"
      bad=1
    fi
  done
  return $bad
}

no_skips_selftest() {
  local scratch bad=0
  scratch=$(mktemp -d) || return 1
  mkdir -p "$scratch/clean/ci" "$scratch/clean/crates/a/src" "$scratch/clean/runtime" \
    "$scratch/clean/tests" "$scratch/clean/target" "$scratch/clean/x/node_modules" || return 1
  (
    cd "$scratch/clean" || exit 1
    printf '#[test]\n#[ignore = "slow"]\nfn slow_case() {}\n#[test]\nfn fine() {}\n' >crates/a/src/lib.rs
    printf "test.skip('quarantined title', () => {});\ntest('fine', () => {});\n" >runtime/a.test.mjs
    printf 'import unittest\nclass T(unittest.TestCase):\n    @unittest.skip("x")\n    def test_q(self):\n        pass\n' >tests/test_a.py
    printf '#[ignore]\nfn generated() {}\n' >target/x.rs
    printf "it.only('vendored', () => {});\n" >x/node_modules/v.test.mjs
    printf "const s = 'a'.skip;\n" >runtime/helper.mjs
    printf '# quarantine\ncrates/a/src/lib.rs slow_case T1 cem\nruntime/a.test.mjs quarantined_title T2 cem\ntests/test_a.py test_q T3 cem\n' \
      >ci/quarantine.txt
  ) || return 1
  sel() {
    local label=$1 want=$2 got
    shift 2
    rm -rf "$scratch/tree" && cp -r "$scratch/clean" "$scratch/tree" || return 1
    (
      cd "$scratch/tree" || exit 1
      eval "$*" || exit 3
      findings >found
      rules ci/quarantine.txt found >out
    )
    got=$?
    if [ "$got" != "$want" ]; then
      echo "   selftest '$label': want exit $want, got $got"
      sed 's/^/      /' "$scratch/tree/out" "$scratch/tree/found" 2>/dev/null
      return 1
    fi
  }
  sel "clean tree passes" 0 true || bad=1
  sel "rust ignore without entry" 1 "printf '#[test]\n#[ignore]\nfn other() {}\n' >>crates/a/src/lib.rs" || bad=1
  sel "rust cfg_attr ignore" 1 "printf '#[cfg_attr(unix, ignore)]\n#[test]\nfn other() {}\n' >>crates/a/src/lib.rs" || bad=1
  sel "rust cfg any()" 1 "printf '#[cfg(any())]\nmod disabled {}\n' >>crates/a/src/lib.rs" || bad=1
  sel "rust cfg not all()" 1 "printf '#[cfg(not(all()))]\n#[test]\nfn other() {}\n' >>crates/a/src/lib.rs" || bad=1
  sel "rust cfg false" 1 "printf '#[cfg(false)]\nfn other() {}\n' >>crates/a/src/lib.rs" || bad=1
  sel "rust inner cfg any()" 1 "printf '#![cfg(any())]\n' >>crates/a/src/lib.rs" || bad=1
  sel "js only" 1 "printf \"test.only('focus', () => {});\n\" >>runtime/a.test.mjs" || bad=1
  sel "js todo" 1 "printf \"it.todo('later');\n\" >>runtime/a.test.mjs" || bad=1
  sel "js describe skip" 1 "printf \"describe.skip('group', () => {});\n\" >>runtime/a.test.mjs" || bad=1
  sel "js xit" 1 "printf \"xit('old', () => {});\n\" >>runtime/a.test.mjs" || bad=1
  sel "js fdescribe" 1 "printf \"fdescribe('f', () => {});\n\" >>runtime/a.test.mjs" || bad=1
  sel "js skip option" 1 "printf \"test('opt', { skip: true }, () => {});\n\" >>runtime/a.test.mjs" || bad=1
  sel "js skip option with reason" 1 "printf \"test('opt', { skip: 'flaky' }, () => {});\n\" >>runtime/a.test.mjs" || bad=1
  sel "js t.skip in body" 1 "printf \"test('body', (t) => { t.skip(); });\n\" >>runtime/a.test.mjs" || bad=1
  sel "js spec file" 1 "printf \"it.skip('b', () => {});\n\" >runtime/b.spec.ts" || bad=1
  sel "js skip option false is fine" 0 "printf \"test('opt', { skip: false }, () => {});\n\" >>runtime/a.test.mjs" || bad=1
  sel "py skipIf" 1 "printf '    @unittest.skipIf(True, \"x\")\n    def test_r(self):\n        pass\n' >>tests/test_a.py" || bad=1
  sel "py skipTest in body" 1 "printf '    def test_s(self):\n        self.skipTest(\"x\")\n' >>tests/test_a.py" || bad=1
  sel "py pytest xfail" 1 "printf '@pytest.mark.xfail\ndef test_t():\n    pass\n' >>tests/test_a.py" || bad=1
  sel "py expectedFailure" 1 "printf '    @unittest.expectedFailure\n    def test_u(self):\n        pass\n' >>tests/test_a.py" || bad=1
  sel "stale entry" 1 "sed -i 's/#\\[ignore = \"slow\"\\]//' crates/a/src/lib.rs" || bad=1
  sel "malformed ticket" 1 "sed -i 's/ T1 / 1 /' ci/quarantine.txt" || bad=1
  sel "missing owner" 1 "sed -i 's/ T2 cem/ T2/' ci/quarantine.txt" || bad=1
  sel "duplicate entry" 1 "printf 'tests/test_a.py test_q T4 cem\n' >>ci/quarantine.txt" || bad=1
  sel "quarantine file missing" 1 "rm ci/quarantine.txt" || bad=1
  rm -rf "$scratch"
  return $bad
}

if ! no_skips_selftest; then
  echo "   NO SKIPS: self test failed"
  exit 1
fi
echo "   no_skips_selftest ok"

cd "$REPO" || exit 1
work=$(mktemp -d) || exit 1
trap 'rm -rf "$work"' EXIT
findings >"$work/found"
rules "$QUARANTINE" "$work/found"
rc=$?
echo "   skipped, disabled or focused tests: $(wc -l <"$work/found")"
if [ -s "$work/found" ]; then
  sed 's/^/      /' "$work/found"
fi
if [ -s "$work/found.rust" ] && [ -f Cargo.toml ]; then
  echo "   quarantined Rust tests (cargo test -- --ignored), reported, not blocking:"
  if cargo test --workspace --locked -q -- --ignored >"$work/ignored" 2>&1; then
    echo "      ignored tests pass"
  else
    echo "      QUARANTINE: ignored tests fail"
  fi
  grep -E '^test |test result' "$work/ignored" | sed 's/^/      /' | tail -n 40
fi
if [ -s "$work/found" ]; then
  awk '$3 != "rust" { print "      not run by this check: " $1 " " $2 }' "$work/found"
fi
exit $rc
