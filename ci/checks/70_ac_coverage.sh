#!/usr/bin/env bash
# AC coverage (MEASUREMENT 5.2 `70_ac_coverage.sh`, SPEC 2 "Test name", A2-14). Owner: QA.
#
# Every criterion in ci/ac-list.txt needs at least one automated test named `ac_NN_<topic>`
# (NN = the two digit number of AC-NN). Test names are collected from:
#   Rust     `cargo test --workspace --locked -- --list`, last path segment; tests marked
#            #[ignore] (`-- --list --ignored`) do not count
#   JS / TS  test files *.test.* and *.spec.* (node tests and browser job tests): the title
#            of test(), it(), describe() or suite() starts with `ac_NN_`; .skip and .todo
#            calls, calls inside // or /* */ comments and titles quoted outside such a
#            call do not count
#   Python   test_*.py: a function `test_ac_NN_<topic>` (unittest needs the `test_` prefix)
#   Shell    ci/checks/*.sh and tests/**/*.sh: a function `ac_NN_<topic>` that is defined and
#            also called in the same file
# Generated, vendored and benchmark trees (target, node_modules, bench, .git) are not read.
#
# Pending list: ci/checks/70_ac_coverage.pending names, one `AC-NN` per line, the criteria
# that have no test yet. The check fails when
#   - a criterion has no test and is not pending,
#   - a pending criterion has a test (remove it from the list, so coverage only grows),
#   - a pending entry is not in ci/ac-list.txt or is malformed,
#   - a test name refers to a criterion that is not in ci/ac-list.txt (withdrawn or
#     mistyped numbers are not evidence).
# The report prints, per milestone, how many criteria are covered and which are pending. A
# milestone can only be signed off when none of its criteria is pending (SPEC 3). Coverage
# is a name check: it never proves that the test checks what the criterion says.
#
# A self test (coverage_selftest) runs first on a scratch tree and proves that every
# rule above can fail.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1
REPO=$(pwd)

AC_LIST=ci/ac-list.txt
PENDING=ci/checks/70_ac_coverage.pending

# prune_find EXPR...: NUL separated files below . outside generated and vendored trees.
prune_find() {
  find . \( -path ./.git -o -path ./target -o -name node_modules -o -path ./bench \) -prune \
    -o -type f \( "$@" \) -print0
}

# JS_TITLES: perl program for js_names. It reads one JS or TS file and prints the `ac_NN_...`
# titles of test calls. Line and block comments are dropped (a commented out test is a skip,
# not coverage), and string and template literals are blanked unless they are the first
# argument of test(), it(), describe() or suite() (optionally `.only`), so a title quoted
# elsewhere does not count. A call may span lines. Known limit: regular expression literals
# are not recognised; a quote inside one can hide later titles (the check then fails, it
# never invents coverage).
JS_TITLES='
  local $/; my $s = <>; my $o = ""; my $n = length $s; my $i = 0;
  my $call = qr/\b(?:test|it|describe|suite)(?:\.only)?\s*\(\s*\z/;
  while ($i < $n) {
    my $c = substr($s, $i, 1); my $d = substr($s, $i, 2);
    if ($d eq "//") { my $e = index($s, "\n", $i); $i = $e < 0 ? $n : $e; next }
    if ($d eq "/*") { my $e = index($s, "*/", $i + 2); $i = $e < 0 ? $n : $e + 2; $o .= " "; next }
    if ($c eq "\x27" || $c eq "\"" || $c eq "`") {
      my $j = $i + 1;
      while ($j < $n) {
        my $x = substr($s, $j, 1);
        if ($x eq "\\") { $j += 2; next }
        last if $x eq $c;
        last if $x eq "\n" && $c ne "`";
        $j++;
      }
      my $body = substr($s, $i + 1, $j - $i - 1);
      $o .= (substr($o, -80) =~ $call) ? $c . $body . $c : $c . $c;
      $i = $j + 1; next;
    }
    $o .= $c; $i++;
  }
  while ($o =~ /\b(?:test|it|describe|suite)(?:\.only)?\s*\(\s*[\x27"`](ac_[0-9]{2}_[A-Za-z0-9_]*)/g) { print "$1\n" }
'

# js_names: `ac_NN_...` titles of running JS and TS tests, one per line.
js_names() {
  local f
  while IFS= read -r -d '' f; do
    perl -e "$JS_TITLES" "$f"
  done < <(prune_find -name '*.test.js' -o -name '*.test.mjs' -o -name '*.test.cjs' \
    -o -name '*.test.ts' -o -name '*.test.mts' -o -name '*.spec.js' -o -name '*.spec.mjs' \
    -o -name '*.spec.ts' -o -name '*.spec.mts')
}

# py_names: `test_ac_NN_...` functions in Python test files, printed without `test_`.
py_names() {
  local f
  while IFS= read -r -d '' f; do
    grep -oE '^[[:space:]]*(async[[:space:]]+)?def[[:space:]]+test_ac_[0-9]{2}_[A-Za-z0-9_]*' "$f" |
      grep -oE 'ac_[0-9]{2}_[A-Za-z0-9_]*$'
  done < <(prune_find -name 'test_*.py')
}

# sh_names: shell functions `ac_NN_...` that are defined and called in the same file.
sh_names() {
  local f name
  while IFS= read -r -d '' f; do
    while IFS= read -r name; do
      # A call is any non comment line that names the function without defining it.
      if grep -vE '^[[:space:]]*#' "$f" | grep -wF -- "$name" |
        grep -qvE "^[[:space:]]*(function[[:space:]]+)?$name[[:space:]]*\(\)"; then
        echo "$name"
      fi
    done < <(grep -oE '^[[:space:]]*(function[[:space:]]+)?ac_[0-9]{2}_[A-Za-z0-9_]*[[:space:]]*\(\)' "$f" |
      grep -oE 'ac_[0-9]{2}_[A-Za-z0-9_]*')
  done < <(find ci/checks tests -type f -name '*.sh' -print0 2>/dev/null)
}

# rust_names LIST_ALL LIST_IGNORED: test names from two `cargo test -- --list` outputs.
rust_names() {
  local all=$1 ignored=$2
  sed -nE 's/^(.*::)?(ac_[0-9]{2}_[A-Za-z0-9_]*): test$/\2/p' "$all" | sort -u >"$all.names"
  sed -nE 's/^(.*::)?(ac_[0-9]{2}_[A-Za-z0-9_]*): test$/\2/p' "$ignored" | sort -u >"$ignored.names"
  comm -23 "$all.names" "$ignored.names"
}

# coverage AC_LIST PENDING NAMES_FILE: applies the rules, prints the report and one line per
# violation, returns 1 if any.
coverage() {
  local list=$1 pending=$2 names=$3 bad=0 n=0 line id ms num
  declare -A in_list=() milestone=() covered=() is_pending=()
  local -a order=()
  if [ ! -f "$list" ]; then
    echo "   AC COVERAGE: $list missing"
    return 1
  fi
  while IFS= read -r line || [ -n "$line" ]; do
    n=$((n + 1))
    case $line in '' | '#'*) continue ;; esac
    if ! [[ "$line" =~ ^AC-([0-9]{2})\ (v[0-9]+\.[0-9]+)$ ]]; then
      echo "   AC COVERAGE: $list:$n: not of the form 'AC-NN vX.Y': $line"
      bad=1
      continue
    fi
    num=${BASH_REMATCH[1]}
    in_list[$num]=1
    milestone[$num]=${BASH_REMATCH[2]}
    order+=("$num")
  done <"$list"
  if [ ${#order[@]} -eq 0 ]; then
    echo "   AC COVERAGE: $list lists no criterion"
    return 1
  fi
  if [ -f "$pending" ]; then
    n=0
    while IFS= read -r line || [ -n "$line" ]; do
      n=$((n + 1))
      case $line in '' | '#'*) continue ;; esac
      if ! [[ "$line" =~ ^AC-([0-9]{2})$ ]]; then
        echo "   AC COVERAGE: $pending:$n: not of the form 'AC-NN': $line"
        bad=1
        continue
      fi
      num=${BASH_REMATCH[1]}
      if [ -z "${in_list[$num]:-}" ]; then
        echo "   AC COVERAGE: $pending:$n: AC-$num is not in $list"
        bad=1
      fi
      if [ -n "${is_pending[$num]:-}" ]; then
        echo "   AC COVERAGE: $pending:$n: AC-$num listed twice"
        bad=1
      fi
      is_pending[$num]=1
    done <"$pending"
  fi
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    num=${line:3:2}
    if [ -z "${in_list[$num]:-}" ]; then
      echo "   AC COVERAGE: test $line names AC-$num, which is not in $list"
      bad=1
      continue
    fi
    covered[$num]="${covered[$num]:+${covered[$num]} }$line"
  done < <(sort -u "$names")
  for num in "${order[@]}"; do
    if [ -n "${covered[$num]:-}" ] && [ -n "${is_pending[$num]:-}" ]; then
      echo "   AC COVERAGE: AC-$num has a test (${covered[$num]%% *}); remove it from $pending"
      bad=1
    elif [ -z "${covered[$num]:-}" ] && [ -z "${is_pending[$num]:-}" ]; then
      echo "   AC COVERAGE: AC-$num (${milestone[$num]}) has no test named ac_${num}_<topic>"
      bad=1
    fi
  done
  # Report per milestone.
  for ms in $(printf '%s\n' "${milestone[@]}" | sort -uV); do
    local total=0 have=0 open=""
    for num in "${order[@]}"; do
      [ "${milestone[$num]}" = "$ms" ] || continue
      total=$((total + 1))
      if [ -n "${covered[$num]:-}" ]; then
        have=$((have + 1))
      else
        open="$open AC-$num"
      fi
    done
    echo "   $ms: $have of $total criteria have a test${open:+; without test:$open}"
  done
  return $bad
}

# collect_names OUT: all test names of the working tree into OUT (Rust through cargo).
collect_names() {
  local out=$1 tmp
  tmp=$(mktemp -d) || return 1
  {
    js_names
    py_names
    sh_names
  } >"$out"
  if [ -f Cargo.toml ]; then
    if ! cargo test --workspace --locked -q -- --list >"$tmp/all" 2>"$tmp/err" ||
      ! cargo test --workspace --locked -q -- --list --ignored >"$tmp/ignored" 2>>"$tmp/err"; then
      sed 's/^/      /' "$tmp/err" | tail -n 20
      echo "   AC COVERAGE: cargo test -- --list failed"
      rm -rf "$tmp"
      return 1
    fi
    rust_names "$tmp/all" "$tmp/ignored" >>"$out"
  fi
  rm -rf "$tmp"
}

coverage_selftest() {
  local scratch bad=0
  scratch=$(mktemp -d) || return 1
  mkdir -p "$scratch/clean/ci/checks" "$scratch/clean/tests/a" "$scratch/clean/runtime/x" \
    "$scratch/clean/bench" "$scratch/clean/tests/b" || return 1
  (
    cd "$scratch/clean" || exit 1
    printf '# list\nAC-01 v0.1\nAC-02 v0.1\nAC-03 v0.1\nAC-04 v0.2\nAC-05 v0.2\nAC-06 v0.3\n' >ci/ac-list.txt
    printf '# pending\nAC-06\n' >ci/checks/70_ac_coverage.pending
    printf "test('ac_01_js_title', () => {});\nit(\`ac_01_other \${x}\`, () => {});\n" >runtime/x/a.test.mjs
    printf 'def test_ac_02_py():\n    pass\n' >tests/a/test_x.py
    printf 'ac_03_sh_case() {\n  true\n}\nac_03_sh_case || exit 1\n' >tests/b/c.sh
    printf 'tests::ac_04_rust_case: test\nac_05_top: test\nsrc/lib.rs - f (line 3): test\n' >rust.all
    printf 'tests::ac_99_ignored: test\n' >rust.ign
    printf 'tests::ac_99_ignored: test\n' >>rust.all
    printf "test('ac_06_in_bench', () => {});\n" >bench/z.test.mjs
  ) || return 1
  # sel LABEL WANT_FAIL SETUP: runs the rules on a fresh copy after SETUP.
  sel() {
    local label=$1 want=$2 got=0
    shift 2
    rm -rf "$scratch/tree" && cp -r "$scratch/clean" "$scratch/tree" || return 1
    (
      cd "$scratch/tree" || exit 1
      eval "$*" || exit 3
      { js_names; py_names; sh_names; rust_names rust.all rust.ign; } >names
      coverage ci/ac-list.txt ci/checks/70_ac_coverage.pending names >out
    )
    got=$?
    if [ "$got" = 3 ] || [ "$got" != "$want" ]; then
      echo "   selftest '$label': want exit $want, got $got"
      sed 's/^/      /' "$scratch/tree/out" 2>/dev/null
      return 1
    fi
  }
  sel "clean tree passes" 0 true || bad=1
  sel "JS title missing" 1 "sed -i 's/ac_01_js_title/js_title/; s/ac_01_other/other/' runtime/x/a.test.mjs" || bad=1
  sel "JS skipped test does not count" 1 "sed -i 's/test(/test.skip(/; s/^it(/it.todo(/' runtime/x/a.test.mjs" || bad=1
  sel "JS test in line comment does not count" 1 "printf \"// test('ac_01_js_title', () => {});\n// it(\\\`ac_01_other\\\`, () => {});\n\" >runtime/x/a.test.mjs" || bad=1
  sel "JS test in block comment does not count" 1 "printf \"/*\ntest('ac_01_js_title', () => {});\n*/ /* it('ac_01_other') */\n\" >runtime/x/a.test.mjs" || bad=1
  sel "JS title quoted outside a call does not count" 1 "printf \"const t = \\\"test('ac_01_js_title'\\\";\nconst u = 'it(\\\"ac_01_other';\n\" >runtime/x/a.test.mjs" || bad=1
  sel "JS comment marker inside a string is no comment" 0 "printf \"const u = 'http://x/*';\ntest('ac_01_js_title', () => {}); // */\n\" >runtime/x/a.test.mjs" || bad=1
  sel "JS call over two lines counts" 0 "printf \"test(\n  'ac_01_js_title',\n  () => {},\n);\n\" >runtime/x/a.test.mjs" || bad=1
  sel "Python test without test_ prefix" 1 "sed -i 's/test_ac_02/ac_02/' tests/a/test_x.py" || bad=1
  sel "shell function never called" 1 "sed -i '\$d' tests/b/c.sh" || bad=1
  sel "shell call only in a comment" 1 "sed -i 's/^ac_03_sh_case ||/# ac_03_sh_case ||/' tests/b/c.sh" || bad=1
  sel "Rust ignored test does not count" 1 "printf 'tests::ac_04_rust_case: test\n' >>rust.ign" || bad=1
  sel "Rust doc test name does not count" 1 "sed -i 's/^ac_05_top: test/src\/lib.rs - ac_05_x (line 1): test/' rust.all" || bad=1
  sel "bench tree is not read" 1 "sed -i '/AC-06/d' ci/checks/70_ac_coverage.pending" || bad=1
  sel "pending but covered" 1 "printf 'AC-01\n' >>ci/checks/70_ac_coverage.pending" || bad=1
  sel "pending not in list" 1 "printf 'AC-07\n' >>ci/checks/70_ac_coverage.pending" || bad=1
  sel "pending malformed" 1 "printf 'AC-6\n' >>ci/checks/70_ac_coverage.pending" || bad=1
  sel "pending twice" 1 "printf 'AC-06\n' >>ci/checks/70_ac_coverage.pending" || bad=1
  sel "test names unknown AC" 1 "printf 'x::ac_42_extra: test\n' >>rust.all" || bad=1
  sel "list malformed" 1 "printf 'AC-07 0.3\n' >>ci/ac-list.txt" || bad=1
  sel "list missing" 1 "rm ci/ac-list.txt" || bad=1
  sel "list empty" 1 "printf '# only comments\n' >ci/ac-list.txt" || bad=1
  sel "pending file absent and all covered" 0 "sed -i '/AC-06/d' ci/ac-list.txt; rm ci/checks/70_ac_coverage.pending" || bad=1
  rm -rf "$scratch"
  return $bad
}

if ! coverage_selftest; then
  echo "   AC COVERAGE: self test failed"
  exit 1
fi
echo "   coverage_selftest ok"

cd "$REPO" || exit 1
work=$(mktemp -d) || exit 1
trap 'rm -rf "$work"' EXIT
collect_names "$work/names" || exit 1
coverage "$AC_LIST" "$PENDING" "$work/names"
