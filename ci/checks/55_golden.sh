#!/usr/bin/env bash
# Golden runner (MEASUREMENT 5.2, SPEC AC-02, AC-03, AC-52). Owner: QA.
#
# Suites, each run from the repository root with the path exactly as written here:
#   ac_02_examples        examples/v0_1/NAME.ostl  `ostrel run`    exit 0, stdout == NAME.expected
#                         (at least 10 cases)
#   ac_03_error_goldens   tests/errors/NAME.ostl   `ostrel check`  exit 1, stderr == NAME.expected_err
#                         (at least 15 cases)
#   ac_52_runtime_goldens tests/runtime/NAME.ostl  `ostrel run`    exit 1, stdout == NAME.expected
#                         and stderr == NAME.expected_err; one case per runtime error kind
#                         IntOverflow, DivisionByZero, CallDepth, StepLimit, TextLimit (SPEC 5.0)
#   ac_52_usage_errors    unknown flag and missing file argument exit with code 2
#
# Structure rules (always blocking): every case has its expected file(s) and every expected
# file has its case; minimum case counts; no `*.new` file anywhere in the repository; the
# bless switch (OSTREL_BLESS or BLESS, any non empty value) is not set. This runner never
# writes into the repository: expected files are written by hand (ARCHITECTURE 15.4 rule 5).
#
# Optional NAME.args next to a tests/runtime case: one CLI argument per line, passed before
# the file path (for the small `--max-steps` case of SPEC 12.4).
#
# Compiler binary: $OSTREL if set, else `cargo build --locked --release -p ostrel_cli` and
# $CARGO_TARGET_DIR/release/ostrel. Each case runs with `timeout 10`.
#
# Pending marker: while ci/checks/55_golden.pending exists, failures of the four suites are
# printed as PENDING and do not fail the gate; the structure rules and the self test still
# do. The marker names the decision that allows it. The check fails when the marker exists
# but every suite passes, so the marker cannot outlive the compiler work. A pending run is
# never evidence for AC-02, AC-03 or AC-52.
#
# A self test (golden_runner_selftest) runs first against a fake compiler in a scratch tree
# and proves that every rule above can fail.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1
REPO=$(pwd)

PENDING_FILE=ci/checks/55_golden.pending
MIN_EXAMPLES=10
MIN_ERRORS=15
RUNTIME_KINDS=(IntOverflow DivisionByZero CallDepth StepLimit TextLimit)
CASE_TIMEOUT=10

# All output of the suites goes through these two; `structure` problems always block.
structure_fail=0
suite_fail=0
structure() { echo "   GOLDEN STRUCTURE: $*"; structure_fail=1; }
failcase() { echo "   GOLDEN FAIL: $*"; suite_fail=1; }

# show_diff EXPECTED_FILE ACTUAL_FILE: first lines of a unified diff, for the log only.
show_diff() {
  diff -u --label expected --label actual "$1" "$2" | head -n 20 | sed 's/^/      /'
}

# pairs DIR CASE_EXT EXPECTED_EXT...: structure rule "case <-> expected file" for one dir.
pairs() {
  local dir=$1 f stem ext
  shift
  for f in "$dir"/*.ostl; do
    [ -e "$f" ] || continue
    stem=${f%.ostl}
    for ext in "$@"; do
      [ -f "$stem.$ext" ] || structure "$f has no ${stem##*/}.$ext"
    done
  done
  for ext in "$@" args; do
    for f in "$dir"/*."$ext"; do
      [ -e "$f" ] || continue
      stem=${f%."$ext"}
      [ -f "$stem.ostl" ] || structure "$f has no case ${stem##*/}.ostl"
    done
  done
}

# count DIR: number of cases in DIR.
count() {
  local n=0 f
  for f in "$1"/*.ostl; do [ -e "$f" ] && n=$((n + 1)); done
  echo "$n"
}

# runcase OUT_PREFIX CMD...: runs CMD with a timeout, stdout and stderr into files, prints
# the exit code (124 is the timeout).
runcase() {
  local out=$1
  shift
  timeout "$CASE_TIMEOUT" "$@" >"$out.stdout" 2>"$out.stderr" </dev/null
  echo $?
}

# expect_exit NAME GOT WANT: one line per mismatch; a panic (101), a signal (> 128) and a
# timeout (124) are named, because AC-04 treats them as crashes.
expect_exit() {
  local name=$1 got=$2 want=$3 what=""
  [ "$got" = "$want" ] && return 0
  case $got in
    101) what=" (panic)" ;;
    124) what=" (timeout ${CASE_TIMEOUT}s)" ;;
    12[5-9] | 1[3-9][0-9] | 2[0-9][0-9]) what=" (signal or launch failure)" ;;
  esac
  failcase "$name: exit code $got$what, expected $want"
  return 1
}

# expect_same NAME WHAT EXPECTED_FILE ACTUAL_FILE
expect_same() {
  cmp -s "$3" "$4" && return 0
  failcase "$1: $2 differs from ${3##*/}"
  show_diff "$3" "$4"
  return 1
}

ac_02_examples() {
  local ostrel=$1 tmp=$2 dir=examples/v0_1 f stem rc
  for f in "$dir"/*.ostl; do
    [ -e "$f" ] || continue
    stem=${f%.ostl}
    [ -f "$stem.expected" ] || continue
    rc=$(runcase "$tmp/case" "$ostrel" run "$f")
    expect_exit "$f" "$rc" 0 && expect_same "$f" stdout "$stem.expected" "$tmp/case.stdout"
  done
}

ac_03_error_goldens() {
  local ostrel=$1 tmp=$2 dir=tests/errors f stem rc
  for f in "$dir"/*.ostl; do
    [ -e "$f" ] || continue
    stem=${f%.ostl}
    [ -f "$stem.expected_err" ] || continue
    rc=$(runcase "$tmp/case" "$ostrel" check "$f")
    expect_exit "$f" "$rc" 1 && expect_same "$f" stderr "$stem.expected_err" "$tmp/case.stderr"
  done
}

ac_52_runtime_goldens() {
  local ostrel=$1 tmp=$2 dir=tests/runtime f stem rc kind ok args=()
  if [ ! -d "$dir" ]; then
    failcase "$dir does not exist (AC-52 runtime goldens, SPEC 5.0)"
    return
  fi
  for kind in "${RUNTIME_KINDS[@]}"; do
    if ! grep -qsF -- "runtime error[$kind]" "$dir"/*.expected_err; then
      failcase "$dir: no golden case for runtime error kind $kind"
    fi
  done
  for f in "$dir"/*.ostl; do
    [ -e "$f" ] || continue
    stem=${f%.ostl}
    [ -f "$stem.expected" ] && [ -f "$stem.expected_err" ] || continue
    args=()
    if [ -f "$stem.args" ]; then mapfile -t args <"$stem.args"; fi
    rc=$(runcase "$tmp/case" "$ostrel" run "${args[@]}" "$f")
    expect_exit "$f" "$rc" 1 || continue
    expect_same "$f" stdout "$stem.expected" "$tmp/case.stdout"
    expect_same "$f" stderr "$stem.expected_err" "$tmp/case.stderr"
  done
}

ac_52_usage_errors() {
  local ostrel=$1 tmp=$2 rc probe=examples/v0_1/01_hello.ostl
  rc=$(runcase "$tmp/case" "$ostrel" run --no-such-flag-55 "$probe")
  expect_exit "usage: ostrel run --no-such-flag-55 $probe" "$rc" 2
  rc=$(runcase "$tmp/case" "$ostrel" run)
  expect_exit "usage: ostrel run (missing file argument)" "$rc" 2
  rc=$(runcase "$tmp/case" "$ostrel" check)
  expect_exit "usage: ostrel check (missing file argument)" "$rc" 2
}

# structure_rules ROOT: the always blocking part, relative to the current directory.
structure_rules() {
  local n f
  if [ -n "${OSTREL_BLESS:-}" ] || [ -n "${BLESS:-}" ]; then
    structure "bless switch is set (OSTREL_BLESS or BLESS); goldens are never blessed in the gate"
  fi
  pairs examples/v0_1 expected
  pairs tests/errors expected_err
  if [ -d tests/runtime ]; then pairs tests/runtime expected expected_err; fi
  n=$(count examples/v0_1)
  [ "$n" -ge "$MIN_EXAMPLES" ] || structure "examples/v0_1 has $n cases, at least $MIN_EXAMPLES needed (AC-02)"
  n=$(count tests/errors)
  [ "$n" -ge "$MIN_ERRORS" ] || structure "tests/errors has $n cases, at least $MIN_ERRORS needed (AC-03)"
  while IFS= read -r -d '' f; do
    structure "${f#./} must not exist (unreviewed golden output)"
  done < <(find . \( -path ./.git -o -path ./target -o -path '*/node_modules' \) -prune \
    -o -type f -name '*.new' -print0)
}

# all_suites OSTREL TMP: the four AC suites in a fixed order.
all_suites() {
  ac_02_examples "$1" "$2"
  ac_03_error_goldens "$1" "$2"
  ac_52_runtime_goldens "$1" "$2"
  ac_52_usage_errors "$1" "$2"
}

# selftest_expect LABEL WANT_STRUCTURE WANT_SUITE: runs both parts in the current scratch
# tree with the fake compiler and compares the outcome.
selftest_expect() {
  local label=$1 want_s=$2 want_f=$3 log
  structure_fail=0
  suite_fail=0
  log=$(structure_rules; all_suites "$fake" "$work"; echo "RESULT $structure_fail $suite_fail")
  local got
  got=$(printf '%s\n' "$log" | sed -n 's/^RESULT //p')
  if [ "$got" != "$want_s $want_f" ]; then
    echo "   self test case '$label': got structure/suite '$got', expected '$want_s $want_f'"
    printf '%s\n' "$log" | sed 's/^/     | /'
    return 1
  fi
  return 0
}

golden_runner_selftest() {
  local scratch bad=0 i
  scratch=$(mktemp -d) || return 1
  # shellcheck disable=SC2064
  trap "rm -rf '$scratch'" RETURN
  work="$scratch/work"
  fake="$scratch/fake-ostrel"
  mkdir -p "$work" "$scratch/tree/examples/v0_1" "$scratch/tree/tests/errors" \
    "$scratch/tree/tests/runtime"
  # Fake compiler: behaviour comes from FAKE_* markers inside the case file, so each self
  # test case can break exactly one thing.
  cat >"$fake" <<'FAKE'
#!/usr/bin/env bash
cmd=${1:-}; shift || true
file=""
for a in "$@"; do
  case $a in
    --max-steps) ;;
    --*) echo "unknown flag $a" >&2; exit 2 ;;
    *) file=$a ;;
  esac
done
[ -n "$file" ] || { echo "missing file" >&2; exit 2; }
out=$(sed -n 's/^# OUT //p' "$file")
err=$(sed -n 's/^# ERR //p' "$file")
code=$(sed -n 's/^# EXIT //p' "$file")
[ -n "$out" ] && printf '%s\n' "$out"
[ -n "$err" ] && printf '%s\n' "$err" >&2
exit "${code:-0}"
FAKE
  chmod +x "$fake"
  (
    cd "$scratch/tree" || exit 1
    for i in $(seq -w 1 "$MIN_EXAMPLES"); do
      printf '# OUT ok %s\n# EXIT 0\n' "$i" >"examples/v0_1/e$i.ostl"
      printf 'ok %s\n' "$i" >"examples/v0_1/e$i.expected"
    done
    for i in $(seq -w 1 "$MIN_ERRORS"); do
      printf '# ERR tests/errors/x%s.ostl:1:1: error[E0008]: bad\n# EXIT 1\n' "$i" >"tests/errors/x$i.ostl"
      printf 'tests/errors/x%s.ostl:1:1: error[E0008]: bad\n' "$i" >"tests/errors/x$i.expected_err"
    done
    for k in "${RUNTIME_KINDS[@]}"; do
      printf '# OUT before\n# ERR tests/runtime/%s.ostl:2:3: runtime error[%s]: m\n# EXIT 1\n' "$k" "$k" >"tests/runtime/$k.ostl"
      printf 'before\n' >"tests/runtime/$k.expected"
      printf 'tests/runtime/%s.ostl:2:3: runtime error[%s]: m\n' "$k" "$k" >"tests/runtime/$k.expected_err"
    done
    printf -- '--max-steps\n1000\n' >tests/runtime/StepLimit.args
  )
  cp -r "$scratch/tree" "$scratch/clean"
  # case LABEL WANT_STRUCTURE WANT_SUITE SETUP_COMMAND
  sel() {
    local label=$1 ws=$2 wf=$3
    shift 3
    rm -rf "$scratch/tree" && cp -r "$scratch/clean" "$scratch/tree" || return 1
    (cd "$scratch/tree" && eval "$*" && selftest_expect "$label" "$ws" "$wf")
  }
  sel "clean tree passes" 0 0 true || bad=1
  sel "example stdout differs" 0 1 "printf 'ok 99\n' >examples/v0_1/e01.expected" || bad=1
  sel "example exits 1" 0 1 "printf '# OUT ok 01\n# EXIT 1\n' >examples/v0_1/e01.ostl" || bad=1
  sel "example panics" 0 1 "printf '# OUT ok 01\n# EXIT 101\n' >examples/v0_1/e01.ostl" || bad=1
  sel "error exits 0" 0 1 "printf '# ERR x\n# EXIT 0\n' >tests/errors/x01.ostl" || bad=1
  sel "error column differs" 0 1 "printf 'tests/errors/x01.ostl:1:2: error[E0008]: bad\n' >tests/errors/x01.expected_err" || bad=1
  sel "runtime stdout differs" 0 1 "printf 'other\n' >tests/runtime/CallDepth.expected" || bad=1
  sel "runtime stderr differs" 0 1 "printf 'tests/runtime/CallDepth.ostl:2:3: runtime error[CallDepth]: n\n' >tests/runtime/CallDepth.expected_err" || bad=1
  sel "runtime exits 0" 0 1 "sed -i 's/^# EXIT 1/# EXIT 0/' tests/runtime/IntOverflow.ostl" || bad=1
  sel "runtime kind missing" 0 1 "rm tests/runtime/TextLimit.*" || bad=1
  sel "runtime dir missing" 0 1 "rm -r tests/runtime" || bad=1
  sel "example without expected" 1 0 "rm examples/v0_1/e01.expected" || bad=1
  sel "expected without case" 1 0 "printf 'x\n' >tests/errors/orphan.expected_err" || bad=1
  sel "args without case" 1 0 "printf 'x\n' >tests/runtime/orphan.args" || bad=1
  sel "runtime case without stderr golden" 1 1 "rm tests/runtime/DivisionByZero.expected_err" || bad=1
  sel "too few examples" 1 0 "rm examples/v0_1/e01.*" || bad=1
  sel "too few errors" 1 0 "rm tests/errors/x01.*" || bad=1
  sel "new file present" 1 0 "printf 'x\n' >tests/errors/x01.expected_err.new" || bad=1
  sel "bless switch set" 1 0 "export OSTREL_BLESS=1" || bad=1
  sel "usage exit wrong" 0 1 "sed -i 's/>\&2; exit 2 ;;/>\&2; exit 0 ;;/' '$fake'" || bad=1
  return $bad
}

# Clean bless variables cannot leak into the self test from the caller; the real run below
# still sees them.
if ! (unset OSTREL_BLESS BLESS; golden_runner_selftest); then
  echo "golden runner self test failed"
  exit 1
fi
echo "   golden_runner_selftest ok"

cd "$REPO" || exit 1
structure_fail=0
suite_fail=0
structure_rules

ostrel="${OSTREL:-}"
if [ -z "$ostrel" ]; then
  if ! cargo build --locked --release -q -p ostrel_cli; then
    echo "   GOLDEN FAIL: ostrel_cli does not build"
    exit 1
  fi
  ostrel="${CARGO_TARGET_DIR:-target}/release/ostrel"
fi
if [ ! -x "$ostrel" ]; then
  echo "   GOLDEN FAIL: compiler binary $ostrel not found"
  exit 1
fi
tmp=$(mktemp -d) || exit 1
trap 'rm -rf "$tmp"' EXIT
all_suites "$ostrel" "$tmp"

echo "   examples $(count examples/v0_1), error goldens $(count tests/errors), runtime goldens $( [ -d tests/runtime ] && count tests/runtime || echo 0)"
if [ "$structure_fail" = 1 ]; then
  exit 1
fi
if [ -f "$PENDING_FILE" ]; then
  if [ "$suite_fail" = 0 ]; then
    echo "   GOLDEN STRUCTURE: every suite passes; remove $PENDING_FILE"
    exit 1
  fi
  echo "   GOLDEN PENDING: suite failures above are not blocking while $PENDING_FILE exists:"
  sed 's/^/      /' "$PENDING_FILE"
  echo "   NOT EVIDENCE for AC-02, AC-03, AC-52"
  exit 0
fi
exit "$suite_fail"
