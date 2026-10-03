#!/usr/bin/env bash
# Hostile input corpus in the gate, check half (AC-04, S-2, MEASUREMENT 5.2).
#
# The corpus, its generator, the limits and the runner belong to QA (tests/hostile/, QA-1).
# This check only calls the runner, so that limits and expectations live in one place
# (tests/hostile/README, A2-20):
#   1. bash tests/hostile/run.sh --self-test
#      generator builds and is deterministic, the seven named cases of AC-04 and the string
#      cases of RED-B F17 exist; needs no compiler.
#   2. bash tests/hostile/run.sh --commands check --ostrel <release binary of 40_build.sh>
#      runs `ostrel check` on every case with the CPU time and memory limits of the README
#      (D93: CPU time, wall clock only as a 60 s hang guard).
#      The `run` half is ci/checks/56_hostile_run.sh, so the corpus is not run twice (#283).
#
# Step 2 is on since the CLI implements `ostrel check` (INT-4): a missing release binary or
# any failing case makes the check red in scope full. HOSTILE_COMPILER=on|off overrides the
# switch for local runs only; gate evidence is taken without it. With the pass off, a probe
# is run and a warning is printed when the binary already answers `check` with a diagnostic.
#
# A self test (hostile_selftest) runs first in every scope. It calls this script against a
# fake runner and fake binaries in a temporary directory and proves that every rule can
# fail; it needs no compiler.
#
# Environment: HOSTILE_DIR (default tests/hostile) and OSTREL (default
# $CARGO_TARGET_DIR/release/ostrel, else target/release/ostrel), both for the self test.
#
# GATE_SCOPE=quick runs only the self test (no release build in quick scope).
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

compiler_pass=on

# hostile_selftest: runs this script with a fake runner; every case states the exit code
# and, where it matters, the arguments the runner must have received.
hostile_selftest() {
  local self tmp bad=0 n=0
  self="$(pwd)/ci/checks/45_hostile.sh"
  tmp=$(mktemp -d) || return 1
  mkdir -p "$tmp/hostile" "$tmp/bin" "$tmp/norunner" || return 1
  cat >"$tmp/hostile/run.sh" <<'EOF'
#!/usr/bin/env bash
echo "$*" >>"$(dirname "$0")/args.txt"
if [ "${1:-}" = --self-test ]; then exit "${FAKE_SELFTEST_CODE:-0}"; fi
exit "${FAKE_RUNNER_CODE:-0}"
EOF
  printf '#!/usr/bin/env bash\nexit 1\n' >"$tmp/bin/ostrel"
  chmod +x "$tmp/bin/ostrel"
  # sel LABEL WANT_CODE WANT_ARGS [ENV...]: WANT_ARGS "-" skips the argument check.
  sel() {
    local label=$1 want=$2 args=$3 got out
    shift 3
    n=$((n + 1))
    rm -f "$tmp/hostile/args.txt"
    out=$(env -u HOSTILE_COMPILER -u OSTREL GATE_SCOPE=full HOSTILE_SELFTEST_CHILD=1 \
      HOSTILE_DIR="$tmp/hostile" "$@" bash "$self" 2>&1)
    got=$?
    if [ "$got" != "$want" ]; then
      echo "   selftest '$label': want exit $want, got $got"
      echo "$out" | sed 's/^/      | /'
      return 1
    fi
    if [ "$args" != - ] && [ "$(tr '\n' '|' <"$tmp/hostile/args.txt" 2>/dev/null)" != "$args" ]; then
      echo "   selftest '$label': runner called with '$(tr '\n' '|' <"$tmp/hostile/args.txt" 2>/dev/null)', want '$args'"
      return 1
    fi
  }
  local bin="$tmp/bin/ostrel" missing="$tmp/bin/missing"
  sel "default on, green" 0 "--self-test|--commands check --ostrel $bin|" OSTREL="$bin" || bad=1
  sel "default on, missing binary" 1 "--self-test|" OSTREL="$missing" || bad=1
  sel "default on, failing case" 1 - OSTREL="$bin" FAKE_RUNNER_CODE=1 || bad=1
  sel "runner self test red" 1 "--self-test|" OSTREL="$bin" FAKE_SELFTEST_CODE=1 || bad=1
  sel "override off" 0 "--self-test|" OSTREL="$bin" HOSTILE_COMPILER=off || bad=1
  sel "override on" 0 "--self-test|--commands check --ostrel $bin|" OSTREL="$bin" HOSTILE_COMPILER=on || bad=1
  sel "override invalid" 1 - OSTREL="$bin" HOSTILE_COMPILER=yes || bad=1
  sel "dir without runner" 1 - OSTREL="$bin" HOSTILE_DIR="$tmp/norunner" || bad=1
  rm -rf "$tmp"
  [ $bad = 0 ] && echo "   hostile_selftest ok ($n cases)"
  return $bad
}

if [ -z "${HOSTILE_SELFTEST_CHILD:-}" ]; then
  if ! hostile_selftest; then
    echo "   HOSTILE: self test failed"
    exit 1
  fi
fi

if [ "${GATE_SCOPE:-full}" = quick ]; then
  echo "   hostile: compiler pass skipped (GATE_SCOPE=quick)"
  exit 0
fi

dir="${HOSTILE_DIR:-tests/hostile}"
runner="$dir/run.sh"
if [ ! -f "$runner" ]; then
  if [ -d "$dir" ]; then
    echo "   hostile: FAIL: $dir exists but has no run.sh"
    exit 1
  fi
  echo "   hostile: $dir is not in the repository yet (QA-1), nothing to run"
  exit 0
fi

mode="${HOSTILE_COMPILER:-$compiler_pass}"
case "$mode" in on | off) ;; *) echo "   hostile: HOSTILE_COMPILER must be on or off"; exit 1 ;; esac

echo "   -> bash $runner --self-test"
bash "$runner" --self-test || exit 1

ostrel="${OSTREL:-${CARGO_TARGET_DIR:-target}/release/ostrel}"
if [ "$mode" = on ]; then
  if [ ! -x "$ostrel" ]; then
    echo "   hostile: FAIL: compiler binary not found: $ostrel"
    exit 1
  fi
  echo "   -> bash $runner --commands check --ostrel $ostrel"
  bash "$runner" --commands check --ostrel "$ostrel"
  exit $?
fi

echo "   hostile: compiler pass off (AC-04 expects it on; HOSTILE_COMPILER is for local runs only)"
if [ -x "$ostrel" ]; then
  probe=$(mktemp) || exit 1
  printf ')\n' >"$probe"
  (
    ulimit -c 0
    ulimit -v 524288
    ulimit -S -t 5
    ulimit -H -t 6
    exec timeout -k 1 60 "$ostrel" check "$probe"
  ) </dev/null >/dev/null 2>&1
  code=$?
  rm -f "$probe"
  if [ "$code" = 1 ]; then
    echo "   hostile: WARNING: $ostrel already reports a diagnostic for check; switch compiler_pass on in ci/checks/45_hostile.sh"
  fi
fi
exit 0
