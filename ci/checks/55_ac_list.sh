#!/usr/bin/env bash
# Shape of ci/ac-list.txt (SPEC 3 and gap G-5). Owner: QA.
#   1. Every non comment line is `AC-NN vX.Y` with a known milestone (v0.1, v0.2, v0.3).
#   2. Numbers are unique and sorted ascending.
#   3. No withdrawn criterion is listed (SPEC 5: AC-08, AC-15, AC-19, AC-25, AC-26, AC-45,
#      AC-49, AC-50).
#   4. The v0.1 set is exactly AC-01 to AC-07, AC-52 and AC-53 (G-5).
# Whether every listed criterion has a test is the job of 70_ac_coverage.sh.
# A self test (ac_list_selftest) runs first on broken copies of a valid list.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

WITHDRAWN="AC-08 AC-15 AC-19 AC-25 AC-26 AC-45 AC-49 AC-50"
V01="AC-01 AC-02 AC-03 AC-04 AC-05 AC-06 AC-07 AC-52 AC-53"

# ac_list_rules FILE: prints one line per violation, returns 1 if any.
ac_list_rules() {
  local file=$1 bad=0 n=0 line id ms prev=-1 num v01=""
  if [ ! -f "$file" ]; then
    echo "$file: missing"
    return 1
  fi
  while IFS= read -r line || [ -n "$line" ]; do
    n=$((n + 1))
    case $line in '' | '#'*) continue ;; esac
    if ! [[ "$line" =~ ^(AC-[0-9][0-9])\ (v0\.[123])$ ]]; then
      echo "$file:$n: not of the form 'AC-NN vX.Y' with a known milestone: $line"
      bad=1
      continue
    fi
    id=${BASH_REMATCH[1]}
    ms=${BASH_REMATCH[2]}
    num=$((10#${id#AC-}))
    if [ "$num" -le "$prev" ]; then
      echo "$file:$n: $id is not in ascending order or listed twice"
      bad=1
    fi
    prev=$num
    case " $WITHDRAWN " in *" $id "*)
      echo "$file:$n: $id is withdrawn and must not be listed"
      bad=1
      ;;
    esac
    if [ "$ms" = v0.1 ]; then v01="$v01 $id"; fi
  done <"$file"
  if [ "${v01# }" != "$V01" ]; then
    echo "$file: v0.1 set is '${v01# }', expected '$V01'"
    bad=1
  fi
  return $bad
}

ac_list_selftest() {
  local dir good bad=0
  dir=$(mktemp -d) || return 1
  good="$dir/good"
  {
    echo "# comment"
    echo
    for id in $V01; do echo "$id v0.1"; done
    echo "AC-54 v0.2"
    echo "AC-65 v0.3"
  } >"$good"
  if ! ac_list_rules "$good" >/dev/null; then
    echo "   self test: valid list rejected"
    ac_list_rules "$good"
    bad=1
  fi
  expect_bad() {
    local label=$1
    shift
    "$@" <"$good" >"$dir/case"
    if ac_list_rules "$dir/case" >/dev/null; then
      echo "   self test case '$label' was not caught"
      bad=1
    fi
  }
  expect_bad "withdrawn listed" sed 's/^AC-54 v0.2$/AC-45 v0.3/'
  expect_bad "duplicate" sed 's/^AC-02 v0.1$/AC-01 v0.1/'
  expect_bad "unsorted" sed 's/^AC-65 v0.3$/AC-10 v0.2/'
  expect_bad "unknown milestone" sed 's/^AC-65 v0.3$/AC-65 v0.4/'
  expect_bad "bad format" sed 's/^AC-65 v0.3$/AC-65  v0.3/'
  expect_bad "v0.1 criterion missing" sed '/^AC-53 /d'
  expect_bad "extra v0.1 criterion" sed 's/^AC-54 v0.2$/AC-54 v0.1/'
  rm -rf "$dir"
  return $bad
}

if ! ac_list_selftest; then
  echo "ac-list self test failed"
  exit 1
fi
echo "   ac_list_selftest ok"
ac_list_rules ci/ac-list.txt || exit 1
echo "   ci/ac-list.txt: $(grep -c '^AC-' ci/ac-list.txt) criteria"
