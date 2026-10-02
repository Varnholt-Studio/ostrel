#!/usr/bin/env bash
# Self test of 56_hostile_run.sh (AC-04, G-2). Every rule of the check must be shown red
# on a broken copy of the corpus contract, and the compiler switch must behave as written.
# Uses copies of tests/hostile/ and fake compiler binaries in a temporary directory; no
# compiler and no network needed.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

check=ci/checks/56_hostile_run.sh
tmp=$(mktemp -d) || exit 1
trap 'rm -rf "$tmp"' EXIT

fail=0
n=0
# expect NAME WANT_CODE [GREP_TEXT] -- ENV...: runs the check with the given environment.
expect() {
  local name=$1 want=$2 text=$3
  shift 3
  local out code
  n=$((n + 1))
  out=$(env "$@" bash "$check" 2>&1)
  code=$?
  if [ "$code" != "$want" ]; then
    echo "   selftest $name: exit $code, expected $want"
    echo "$out" | sed 's/^/      | /'
    fail=1
  elif [ -n "$text" ] && ! grep -qF -- "$text" <<< "$out"; then
    echo "   selftest $name: output lacks '$text'"
    echo "$out" | sed 's/^/      | /'
    fail=1
  fi
}

# fixture NAME: fresh copy of the corpus contract, path printed.
fixture() {
  local d="$tmp/$1"
  mkdir -p "$d"
  cp tests/hostile/README tests/hostile/expect.txt tests/hostile/run.sh "$d/"
  cp -r tests/hostile/cases "$d/cases"
  echo "$d"
}

# Fake compilers: a placeholder that does nothing, and one that runs the probe program.
mkdir -p "$tmp/bin"
printf '#!/usr/bin/env bash\nexit 0\n' > "$tmp/bin/stub"
printf '#!/usr/bin/env bash\n[ "$1" = run ] && grep -q hostile-run-probe "$2" && echo hostile-run-probe\nexit 0\n' > "$tmp/bin/real"
chmod +x "$tmp/bin/stub" "$tmp/bin/real"

base=(HOSTILE_DIR= GATE_SCOPE=full HOSTILE_RUN_COMPILER=off OSTREL="$tmp/bin/missing")

# The repository itself passes.
expect repo 0 "corpus contract OK" "${base[@]:1}" HOSTILE_DIR=tests/hostile

d=$(fixture clean)
expect clean 0 "corpus contract OK" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture no_files)
rm "$d/run.sh"
expect no_runner 1 "must contain" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture run_dash)
sed -i -E 's/^\| v0\.1 \| ([^|]*)\| [^|]*\|/| v0.1 | \1| - |/' "$d/README"
expect run_limit_dash 1 "README row v0.1: run limit" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture no_row)
sed -i '/^| v0\.3 |/d' "$d/README"
expect missing_row 1 "no limits row for v0.3" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture mem)
sed -i -E 's/^(\| v0\.2 \|.*\| )512 \|$/\1lots |/' "$d/README"
expect memory_limit 1 "README row v0.2: memory limit" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture steps)
sed -i '/^Small step limit/d' "$d/README"
expect small_steps 1 "small step limit" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture typo)
sed -i 's/runtime error\[DivisionByZero\]$/runtime error[DivByZero]/' "$d/expect.txt"
expect unknown_kind 1 "'DivByZero' is not a runtime error kind" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture form)
sed -i 's/^run_modulo_by_zero.ostl 0 1 runtime error\[DivisionByZero\]$/run_modulo_by_zero.ostl 0 1 DivisionByZero/' "$d/expect.txt"
expect bad_form 1 "is neither 'runtime error[Kind]' nor 'error[Exxxx]'" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture diag_code)
sed -i 's/^escape_cr.ostl 1 1 error\[E0004\]$/escape_cr.ostl 1 1 error[E9999]/' "$d/expect.txt"
expect diagnostic_unregistered 1 "diagnostic E9999 is not registered" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture diag_exit)
sed -i 's/^escape_nul.ostl 1 1 error\[E0004\]$/escape_nul.ostl 1 0 error[E0004]/' "$d/expect.txt"
expect diagnostic_run_exit 1 "escape_nul.ostl: expects error[E0004] but check/run exit '1'/'0'" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture registry)
expect registry_missing 1 "registry $tmp/none.md not found" "${base[@]}" HOSTILE_DIR="$d" ERROR_REGISTRY="$tmp/none.md"

d=$(fixture code)
sed -i 's/^run_overflow_mul.ostl 0 1 /run_overflow_mul.ostl 0 0 /' "$d/expect.txt"
expect run_exit_not_1 1 "run_overflow_mul.ostl: expects runtime error[IntOverflow] but run exit '0'" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture kind_missing)
sed -i '/runtime error\[HeapLimit\]$/d' "$d/expect.txt"
expect kind_uncovered 1 "no case expects runtime error[HeapLimit]" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture named_missing)
rm "$d/cases/run_overflow_add.ostl"
expect named_case_missing 1 "AC-04 run case missing" "${base[@]}" HOSTILE_DIR="$d"

d=$(fixture named_kind)
sed -i 's/^run_long_loop_by_recursion.ostl 0 1 runtime error\[StepLimit\]$/run_long_loop_by_recursion.ostl 0 1 runtime error[CallDepth]/' "$d/expect.txt"
expect named_case_kind 1 "run_long_loop_by_recursion.ostl: must expect runtime error[StepLimit]" "${base[@]}" HOSTILE_DIR="$d"

# Compiler switch.
d=$(fixture switch)
expect off_no_binary 0 "compiler pass off" "${base[@]}" HOSTILE_DIR="$d"
expect off_stub 0 "compiler pass off" "${base[@]}" HOSTILE_DIR="$d" OSTREL="$tmp/bin/stub"
out=$(env "${base[@]}" HOSTILE_DIR="$d" OSTREL="$tmp/bin/stub" bash "$check" 2>&1)
n=$((n + 1))
if grep -q WARNING <<< "$out"; then
  echo "   selftest off_stub_quiet: placeholder binary must not trigger the warning"
  fail=1
fi
expect off_real_warns 0 "WARNING" "${base[@]}" HOSTILE_DIR="$d" OSTREL="$tmp/bin/real"
expect bad_switch 1 "must be on or off" "${base[@]}" HOSTILE_DIR="$d" HOSTILE_RUN_COMPILER=maybe
expect on_without_commands 1 "no --commands option" "${base[@]}" HOSTILE_DIR="$d" HOSTILE_RUN_COMPILER=on OSTREL="$tmp/bin/real"
expect quick_skips 0 "skipped (GATE_SCOPE=quick)" "${base[@]}" HOSTILE_DIR="$d" GATE_SCOPE=quick HOSTILE_RUN_COMPILER=on

# A runner with --commands: the check passes its arguments on and returns its exit code.
d=$(fixture runner)
cat > "$d/run.sh" << 'EOF'
#!/usr/bin/env bash
# fake runner: --commands) is parsed by the real runner
echo "args: $*" > "$(dirname "$0")/args.txt"
exit "${FAKE_RUNNER_CODE:-0}"
EOF
expect on_missing_binary 1 "compiler binary not found" "${base[@]}" HOSTILE_DIR="$d" HOSTILE_RUN_COMPILER=on
expect on_green 0 "" "${base[@]}" HOSTILE_DIR="$d" HOSTILE_RUN_COMPILER=on OSTREL="$tmp/bin/real"
n=$((n + 1))
if [ "$(cat "$d/args.txt" 2>/dev/null)" != "args: --commands run --ostrel $tmp/bin/real" ]; then
  echo "   selftest on_args: runner called with '$(cat "$d/args.txt" 2>/dev/null)'"
  fail=1
fi
expect on_red 1 "" "${base[@]}" HOSTILE_DIR="$d" HOSTILE_RUN_COMPILER=on OSTREL="$tmp/bin/real" FAKE_RUNNER_CODE=1

if [ $fail -ne 0 ]; then
  echo "   56_hostile_run selftest: FAILED"
  exit 1
fi
echo "   56_hostile_run selftest: $n cases OK"
