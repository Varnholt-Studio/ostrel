#!/usr/bin/env bash
# Hostile input corpus in the gate (AC-04, S-2, MEASUREMENT 5.2).
#
# The corpus, its generator, the limits and the runner belong to QA (tests/hostile/, QA-1).
# This check only calls the runner, so that limits and expectations live in one place
# (tests/hostile/README, A2-20):
#   1. bash tests/hostile/run.sh --self-test
#      generator builds and is deterministic, the seven named cases of AC-04 and the string
#      cases of RED-B F17 exist; needs no compiler.
#   2. bash tests/hostile/run.sh --ostrel <release binary of 40_build.sh>
#      runs the compiler on every case with the time and memory limits of the README.
#
# Step 2 is switched on below once the CLI implements `ostrel check` (INT-4). Until then
# the check prints a warning when the binary already answers `check` with a diagnostic, so
# the switch is not forgotten. HOSTILE_COMPILER=on|off overrides the switch for local runs.
#
# GATE_SCOPE=quick skips this check (no release build in quick scope).
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

compiler_pass=off

if [ "${GATE_SCOPE:-full}" = quick ]; then
  echo "   hostile: skipped (GATE_SCOPE=quick)"
  exit 0
fi
runner=tests/hostile/run.sh
if [ ! -f "$runner" ]; then
  if [ -d tests/hostile ]; then
    echo "   hostile: tests/hostile/ exists but has no run.sh"
    exit 1
  fi
  echo "   hostile: tests/hostile/ is not in the repository yet (QA-1), nothing to run"
  exit 0
fi

echo "   -> bash $runner --self-test"
bash "$runner" --self-test || exit 1

ostrel="${CARGO_TARGET_DIR:-target}/release/ostrel"
mode="${HOSTILE_COMPILER:-$compiler_pass}"
if [ "$mode" = on ]; then
  echo "   -> bash $runner --ostrel $ostrel"
  bash "$runner" --ostrel "$ostrel"
  exit $?
fi

echo "   hostile: compiler pass off until the CLI implements check (INT-4)"
if [ -x "$ostrel" ]; then
  probe=$(mktemp) || exit 1
  printf ')\n' > "$probe"
  ( ulimit -v 524288; exec timeout -k 1 5 "$ostrel" check "$probe" ) < /dev/null > /dev/null 2>&1
  code=$?
  rm -f "$probe"
  if [ "$code" = 1 ]; then
    echo "   hostile: WARNING: $ostrel already reports a diagnostic for check; switch compiler_pass on in ci/checks/45_hostile.sh"
  fi
fi
exit 0
