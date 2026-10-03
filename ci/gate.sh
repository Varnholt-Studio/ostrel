#!/usr/bin/env bash
# Central quality gate (MEASUREMENT 5.2). Runs locally (bash ci/gate.sh) and on the server
# for every push to dev and main. Every ci/checks/NN_name.sh runs in name order from the
# repository root; the gate is red when any of them exits non zero. All checks run, so one
# report lists every failing check.
#
# Scope (RED-A 29):
#   GATE_SCOPE unset or "full"  every check on the whole workspace. Only this is evidence.
#   GATE_SCOPE=quick            for builders: Rust checks only for the crates touched on the
#                               branch against GATE_BASE (default origin/dev); checks may
#                               skip expensive steps such as the release build. A change
#                               under tests/ or ci/ widens the set to the whole workspace,
#                               because crate tests read the shared suites under tests/.
# The last line is "GATE: GRUEN (<scope>)" or "GATE: ROT (<scope>)". The scope in that line
# is derived from GATE_SCOPE only, so a quick run never prints the full line.
#
# Exported to the checks:
#   GATE_SCOPE   "full" or "quick"
#   GATE_CRATES  "all", or a space separated list of touched package names (may be empty)
#
# Build directory (D56): a CARGO_TARGET_DIR set by the caller is kept; a relative value is
# resolved against the caller's working directory. When unset or empty, the gate builds into
# ./target of this checkout, never into a cache shared with other checkouts, so artifacts of
# parallel states cannot mix.
set -uo pipefail
caller_dir="$PWD"
cd "$(dirname "$0")/.." || exit 1

if [ -z "${CARGO_TARGET_DIR:-}" ]; then
  CARGO_TARGET_DIR="$PWD/target"
elif [[ "$CARGO_TARGET_DIR" != /* ]]; then
  CARGO_TARGET_DIR="$caller_dir/$CARGO_TARGET_DIR"
fi
export CARGO_TARGET_DIR

scope="${GATE_SCOPE:-full}"
case "$scope" in
  full | quick) ;;
  *)
    echo "GATE_SCOPE must be 'full' or 'quick'"
    echo "GATE: ROT (invalid scope)"
    exit 2
    ;;
esac
export GATE_SCOPE="$scope"

# touched_crates: prints the package names of the crates touched on this branch (committed,
# staged, unstaged and untracked changes), one per line, or the single word "all" when the
# set cannot be narrowed safely (no git checkout, unknown base, workspace, ci or shared test
# files touched). Files under tests/ are read by tests of several crates (goldens, hostile
# corpus, conformance vectors), so a change there runs the tests of every crate.
touched_crates() {
  local base="${GATE_BASE:-origin/dev}" mb path dir name
  if [[ "$base" == -* ]] || ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo all
    return
  fi
  if ! mb=$(git merge-base HEAD "$base" 2>/dev/null); then
    echo all
    return
  fi
  declare -A seen=()
  while IFS= read -r -d '' path; do
    case "$path" in
      Cargo.toml | Cargo.lock | rust-toolchain.toml | .cargo/* | ci/* | tests/*)
        echo all
        return
        ;;
      crates/*/*)
        dir="${path#crates/}"
        dir="${dir%%/*}"
        if [ ! -f "crates/$dir/Cargo.toml" ]; then
          echo all
          return
        fi
        name=$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "crates/$dir/Cargo.toml" | head -n 1)
        if ! [[ "$name" =~ ^[A-Za-z0-9_-]+$ ]]; then
          echo all
          return
        fi
        seen["$name"]=1
        ;;
    esac
  done < <(git diff --name-only -z "$mb" -- && git ls-files -z -o --exclude-standard)
  if [ ${#seen[@]} -gt 0 ]; then
    printf '%s\n' "${!seen[@]}" | sort
  fi
}

if [ "$scope" = quick ]; then
  crates=$(touched_crates | tr '\n' ' ')
  crates="${crates% }"
  case " $crates " in *" all "*) crates=all ;; esac
else
  crates=all
fi
export GATE_CRATES="$crates"
echo "GATE scope: $scope, crates: ${GATE_CRATES:-(none touched)}"

failed=()
for check in ci/checks/*.sh; do
  echo "== $check"
  if bash "$check"; then
    echo "   OK"
  else
    echo "   FAIL: $check"
    failed+=("$check")
  fi
done

if [ ${#failed[@]} -gt 0 ]; then
  echo "Failed checks: ${failed[*]}"
  echo "GATE: ROT ($scope)"
  exit 1
fi
echo "GATE: GRUEN ($scope)"
