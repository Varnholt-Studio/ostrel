#!/usr/bin/env bash
# Hostile input under `ostrel run` (AC-04, gap G-2 of SPEC 9, D41). Owner: QA.
#
# AC-04 holds for `ostrel check` and `ostrel run` (SPEC 5.0). 45_hostile.sh covers the
# corpus as a whole; this check owns the `run` half. Corpus, limits and expectations stay in
# tests/hostile/ (QA-1) and are only read here, never repeated (A2-20).
#
# Part 1, always (no compiler needed): the contract between the corpus and `ostrel run`.
#   * every milestone row of tests/hostile/README has a numeric `run` limit (AC-04 names
#     `run` from v0.1 on, so `-` would silently drop the run half);
#   * the small step limit for `run --max-steps` is written in the README;
#   * every stderr expectation in tests/hostile/expect.txt is either `runtime error[Kind]`
#     with a Kind of ARCHITECTURE 3.4 and exit 1 under `run`, or `error[CODE]` with a code
#     registered in tests/errors/README.md and exit 1 under both `check` and `run` (a
#     diagnostic stops the program before it runs, G15);
#   * every Kind of ARCHITECTURE 3.4 is expected by at least one case, and the three run
#     cases named by AC-04 (deep recursion, overflow, long loop by recursion) are present.
# Part 2, compiler pass: tests/hostile/run.sh --commands run --ostrel <release binary>.
#   Switched on below once the CLI implements `ostrel run` (INT-4). While it is off, a probe
#   program is run; when the binary already behaves like a real `ostrel run`, the check
#   prints a warning so the switch is not forgotten. HOSTILE_RUN_COMPILER=on|off overrides
#   the switch for local runs.
#
# Environment for the self test (56_hostile_run_selftest.sh): HOSTILE_DIR (default
# tests/hostile), ERROR_REGISTRY (default tests/errors/README.md) and OSTREL (default $CARGO_TARGET_DIR/release/ostrel, else target/release/ostrel).
# GATE_SCOPE=quick skips the compiler pass (no release build in quick scope), not part 1.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

compiler_pass=off

dir="${HOSTILE_DIR:-tests/hostile}"
readme="$dir/README"
expect="$dir/expect.txt"
runner="$dir/run.sh"
registry="${ERROR_REGISTRY:-tests/errors/README.md}"

# Runtime error kinds of ARCHITECTURE 3.4 (v1.2, D44). Change only together with 3.4.
kinds="IntOverflow DivisionByZero CallDepth StepLimit HeapLimit TextLimit"
milestones="v0.1 v0.2 v0.3"
# Run cases named by AC-04 (A2-16) and the Kind each one must expect.
named_cases="run_deep_recursion.ostl:CallDepth run_overflow_add.ostl:IntOverflow
  run_long_loop_by_recursion.ostl:StepLimit"

fail=0
problem() {
  echo "   hostile run: FAIL: $*"
  fail=1
}

if [ ! -f "$runner" ] || [ ! -f "$readme" ] || [ ! -f "$expect" ]; then
  echo "   hostile run: $dir must contain README, expect.txt and run.sh (QA-1)"
  exit 1
fi

# Part 1a: limits rows of the README.
lim_mem=""
for m in $milestones; do
  row=$(grep -E "^\| *${m//./\\.} *\|" "$readme" | head -n 1)
  if [ -z "$row" ]; then
    problem "README has no limits row for $m"
    continue
  fi
  IFS='|' read -r _ _ _ lim_run _ mem _ <<< "$row"
  lim_run="${lim_run//[[:space:]]/}"
  mem="${mem//[[:space:]]/}"
  if ! [[ "$lim_run" =~ ^[1-9][0-9]*$ ]]; then
    problem "README row $m: run limit '$lim_run' is not a number of seconds (AC-04 runs 'run' from v0.1)"
  fi
  if ! [[ "$mem" =~ ^[1-9][0-9]*$ ]]; then
    problem "README row $m: memory limit '$mem' is not a number of MiB"
  fi
  if [ "$m" = v0.1 ]; then
    lim_mem="$mem"
    lim_run_v01="$lim_run"
  fi
done
if ! grep -qE '^Small step limit for `run --max-steps`: [1-9][0-9]*\.$' "$readme"; then
  problem "README does not state the small step limit for 'run --max-steps'"
fi

# Part 1b: stderr expectations of expect.txt.
if [ ! -f "$registry" ]; then
  problem "diagnostic code registry $registry not found (T1, AC-03)"
fi
declare -A seen_kind=() case_kind=()
while IFS= read -r line; do
  case "$line" in '' | '#'*) continue ;; esac
  read -r name check_code run_code rest <<< "$line"
  [ -n "$rest" ] || continue
  if [[ "$rest" =~ ^error\[(E[0-9]{4})\]$ ]]; then
    code="${BASH_REMATCH[1]}"
    if [ -f "$registry" ] && ! grep -qE "^\| *$code *\|" "$registry"; then
      problem "expect.txt $name: diagnostic $code is not registered in $registry"
    fi
    if [ "$check_code" != 1 ] || [ "$run_code" != 1 ]; then
      problem "expect.txt $name: expects error[$code] but check/run exit '$check_code'/'$run_code' (a diagnostic exits 1 for both)"
    fi
    continue
  fi
  if ! [[ "$rest" =~ ^runtime\ error\[([A-Za-z]+)\]$ ]]; then
    problem "expect.txt $name: stderr text '$rest' is neither 'runtime error[Kind]' nor 'error[Exxxx]' (G15, ARCHITECTURE 3.4)"
    continue
  fi
  kind="${BASH_REMATCH[1]}"
  case " $kinds " in
    *" $kind "*) ;;
    *)
      problem "expect.txt $name: '$kind' is not a runtime error kind of ARCHITECTURE 3.4 ($kinds)"
      continue
      ;;
  esac
  if [ "$run_code" != 1 ]; then
    problem "expect.txt $name: expects runtime error[$kind] but run exit '$run_code' (runtime errors exit 1)"
  fi
  seen_kind[$kind]=1
  case_kind[$name]="$kind"
done < "$expect"
for kind in $kinds; do
  [ -n "${seen_kind[$kind]:-}" ] || problem "no case expects runtime error[$kind] (ARCHITECTURE 3.4, AC-52)"
done
for entry in $named_cases; do
  name="${entry%%:*}"
  kind="${entry#*:}"
  [ -f "$dir/cases/$name" ] || problem "AC-04 run case missing: $dir/cases/$name"
  if [ "${case_kind[$name]:-}" != "$kind" ]; then
    problem "expect.txt $name: must expect runtime error[$kind], has '${case_kind[$name]:-none}'"
  fi
done
if [ $fail -ne 0 ]; then
  echo "   hostile run: corpus contract for 'ostrel run' broken"
  exit 1
fi
echo "   hostile run: corpus contract OK (${#case_kind[@]} runtime error cases, kinds: $kinds)"

if [ "${GATE_SCOPE:-full}" = quick ]; then
  echo "   hostile run: compiler pass skipped (GATE_SCOPE=quick)"
  exit 0
fi

ostrel="${OSTREL:-${CARGO_TARGET_DIR:-target}/release/ostrel}"
mode="${HOSTILE_RUN_COMPILER:-$compiler_pass}"
case "$mode" in on | off) ;; *) echo "   hostile run: HOSTILE_RUN_COMPILER must be on or off"; exit 1 ;; esac

if [ "$mode" = on ]; then
  # The runner must be able to run the `run` half alone; otherwise the corpus would run
  # `check` twice (45_hostile.sh, #283) and this check would mix both halves.
  if ! grep -qE -- '--commands\)' "$runner"; then
    echo "   hostile run: FAIL: $runner has no --commands option, cannot run the 'run' half alone (#283)"
    exit 1
  fi
  if [ ! -x "$ostrel" ]; then
    echo "   hostile run: FAIL: compiler binary not found: $ostrel"
    exit 1
  fi
  echo "   -> bash $runner --commands run --ostrel $ostrel"
  bash "$runner" --commands run --ostrel "$ostrel"
  exit $?
fi

echo "   hostile run: compiler pass off until the CLI implements run (INT-4)"
if [ -x "$ostrel" ]; then
  probe_dir=$(mktemp -d) || exit 1
  printf 'fn main()\n  print("hostile-run-probe")\n' > "$probe_dir/probe.ostl"
  out=$( (ulimit -v $((lim_mem * 1024)); exec timeout -k 1 "$lim_run_v01" "$ostrel" run "$probe_dir/probe.ostl") < /dev/null 2> /dev/null)
  code=$?
  rm -rf "$probe_dir"
  if [ "$code" = 0 ] && [ "$out" = "hostile-run-probe" ]; then
    echo "   hostile run: WARNING: $ostrel already runs programs; switch compiler_pass on in ci/checks/56_hostile_run.sh (G-2)"
  fi
fi
exit 0
