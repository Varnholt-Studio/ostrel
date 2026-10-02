#!/usr/bin/env bash
# Hostile corpus runner (AC-04, S-2). Owner: QA.
#
#   bash tests/hostile/run.sh --self-test        corpus checks only, no compiler needed
#   bash tests/hostile/run.sh [--ostrel PATH]    run `ostrel check` and `ostrel run` on every case
#
# Options:
#   --ostrel PATH      compiler binary (default: $OSTREL, else $CARGO_TARGET_DIR/release/ostrel,
#                      else target/release/ostrel)
#   --milestone M      limits row of tests/hostile/README (default: $HOSTILE_MILESTONE or v0.1)
#   --out DIR          work directory for the generator and the generated corpus
#                      (default: $HOSTILE_OUT, else $CARGO_TARGET_DIR/hostile, else
#                      ~/.cache/ostrel-target/hostile as in ci/gate.sh)
#   --count N          number of seeded cases (default 200)
#   --seed S           base seed of the generator (default: built into the generator)
#
# Limits are read from the table in tests/hostile/README, nowhere else (A2-20).
# Uses bash, coreutils and rustc from the pinned toolchain; no network.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
cd "$root" || exit 1

mode=run
ostrel="${OSTREL:-}"
milestone="${HOSTILE_MILESTONE:-v0.1}"
# Same default as ci/gate.sh, outside the tree: 00_sanity.sh rejects files over 5 MB.
out="${HOSTILE_OUT:-${CARGO_TARGET_DIR:-$HOME/.cache/ostrel-target}/hostile}"
count=200
seed=""

while [ $# -gt 0 ]; do
  case "$1" in
    --self-test) mode=self; shift ;;
    --ostrel) ostrel="${2:-}"; shift 2 ;;
    --milestone) milestone="${2:-}"; shift 2 ;;
    --out) out="${2:-}"; shift 2 ;;
    --count) count="${2:-}"; shift 2 ;;
    --seed) seed="${2:-}"; shift 2 ;;
    *) echo "usage: $0 [--self-test] [--ostrel PATH] [--milestone M] [--out DIR] [--count N] [--seed S]" >&2; exit 2 ;;
  esac
done

fail=0
problem() { echo "HOSTILE FAIL: $*"; fail=1; }

# Limits row: | v0.1 | check s | run s | fmt s | memory MiB |
row=$(grep -E "^\| *${milestone//./\\.} *\|" "$here/README" | head -1)
if [ -z "$row" ]; then echo "HOSTILE FAIL: no limits row for milestone '$milestone' in tests/hostile/README"; exit 1; fi
IFS='|' read -r _ _ lim_check lim_run lim_fmt lim_mem _ <<< "$row"
lim_check=$(echo "$lim_check" | tr -d ' '); lim_run=$(echo "$lim_run" | tr -d ' ')
lim_fmt=$(echo "$lim_fmt" | tr -d ' '); lim_mem=$(echo "$lim_mem" | tr -d ' ')
echo "hostile: milestone $milestone, limits check ${lim_check}s, run ${lim_run}s, fmt ${lim_fmt}, memory ${lim_mem} MiB"
# Small step limit for the second run of StepLimit cases; value from tests/hostile/README.
small_steps=$(sed -n 's/^Small step limit for `run --max-steps`: \([0-9][0-9]*\)\.$/\1/p' "$here/README" | head -1)
if [ -z "$small_steps" ]; then echo "HOSTILE FAIL: no small step limit in tests/hostile/README"; exit 1; fi

# Build the generator with plain rustc (std only, no workspace entry).
mkdir -p "$out/bin" || exit 1
gen="$out/bin/hostile-gen"
if [ ! -x "$gen" ] || [ "$here/gen/main.rs" -nt "$gen" ]; then
  rustc --edition 2021 -O -o "$gen" "$here/gen/main.rs" || { echo "HOSTILE FAIL: generator does not build"; exit 1; }
fi
gen_args=(--count "$count")
[ -n "$seed" ] && gen_args+=(--seed "$seed")
corpus="$out/corpus"
rm -rf "$corpus"
"$gen" "$corpus" "${gen_args[@]}" || { echo "HOSTILE FAIL: generator failed"; exit 1; }

# Corpus checks (always): required cases, sizes, expectations, determinism.
declare -A where
for f in "$here"/cases/*.ostl "$corpus"/*.ostl; do
  [ -e "$f" ] || continue
  name=$(basename "$f")
  if [ -n "${where[$name]:-}" ]; then problem "case name used twice: $name"; fi
  where[$name]="$f"
done

# AC-04 named cases, RED-B F17 string cases, A2-16 run cases.
required="empty.ostl invalid_utf8.ostl nul_bytes.ostl random_1mib.ostl nest_paren_100k.ostl
  long_line_10mib_comment.ostl unterminated_string_eof.ostl string_literal_in_interp.ostl
  nested_interp.ostl double_brace.ostl escaped_braces.ostl run_deep_recursion.ostl
  run_overflow_add.ostl run_long_loop_by_recursion.ostl chain_below.ostl chain_above.ostl
  wide_below.ostl wide_above.ostl bidi_u202e_in_string.ostl bidi_u202e_in_comment.ostl"
for name in $required; do
  [ -n "${where[$name]:-}" ] || problem "required case missing: $name"
done
size() { wc -c < "$1" | tr -d ' '; }
if [ -n "${where[empty.ostl]:-}" ] && [ "$(size "${where[empty.ostl]}")" != 0 ]; then problem "empty.ostl is not empty"; fi
if [ -n "${where[random_1mib.ostl]:-}" ] && [ "$(size "${where[random_1mib.ostl]}")" -lt 1048576 ]; then problem "random_1mib.ostl is smaller than 1 MiB"; fi
f="${where[long_line_10mib_comment.ostl]:-}"
if [ -n "$f" ]; then
  [ "$(size "$f")" -ge 10485760 ] || problem "long_line_10mib_comment.ostl is smaller than 10 MiB"
  [ "$(tr -cd '\n' < "$f" | wc -c)" -eq 0 ] || problem "long_line_10mib_comment.ostl is not a single line"
fi
f="${where[nest_paren_100k.ostl]:-}"
if [ -n "$f" ] && [ "$(tr -cd '(' < "$f" | wc -c)" -lt 100000 ]; then problem "nest_paren_100k.ostl nests less than 100 000"; fi
f="${where[invalid_utf8.ostl]:-}"
if [ -n "$f" ] && ! LC_ALL=C grep -q "$(printf '\377')" "$f"; then problem "invalid_utf8.ostl has no 0xFF byte"; fi
f="${where[nul_bytes.ostl]:-}"
if [ -n "$f" ] && [ "$(tr -cd '\000' < "$f" | wc -c)" -eq 0 ]; then problem "nul_bytes.ostl has no NUL byte"; fi
# D54 chain cases: operands must stay on their side of the AST height limit in README.
height=$(sed -n 's/^AST height limit: \([0-9][0-9]*\)\.$/\1/p' "$here/README" | head -1)
if [ -z "$height" ]; then
  problem "no AST height limit in tests/hostile/README"
else
  f="${where[chain_below.ostl]:-}"
  if [ -n "$f" ] && [ $(( $(tr -cd '+' < "$f" | wc -c) + 1 )) -ge $((height - 16)) ]; then problem "chain_below.ostl is not clearly under the AST height limit $height"; fi
  f="${where[chain_above.ostl]:-}"
  if [ -n "$f" ] && [ $(( $(tr -cd '+' < "$f" | wc -c) + 1 )) -le "$height" ]; then problem "chain_above.ostl is not over the AST height limit $height"; fi
fi
for f in "$here"/cases/*; do
  [ "$(size "$f")" -le 65536 ] || problem "committed case over 64 KiB, generate it instead: $f"
done

declare -A exp_check exp_run exp_err
while IFS= read -r line; do
  case "$line" in ''|'#'*) continue ;; esac
  read -r name c r rest <<< "$line"
  case "$c$r" in [01*][01*]) ;; *) problem "expect.txt: bad exit codes in: $line"; continue ;; esac
  [ -n "${where[$name]:-}" ] || problem "expect.txt names a case that does not exist: $name"
  exp_check[$name]="$c"; exp_run[$name]="$r"; exp_err[$name]="$rest"
done < "$here/expect.txt"

if [ "$mode" = self ]; then
  again="$out/corpus-again"
  rm -rf "$again"
  "$gen" "$again" "${gen_args[@]}" > /dev/null || problem "generator failed on second run"
  cmp -s "$corpus/MANIFEST" "$again/MANIFEST" || problem "generator is not deterministic (MANIFEST differs)"
  rm -rf "$again"
  echo "hostile: ${#where[@]} cases, ${#exp_check[@]} with expectations"
  if [ $fail -eq 0 ]; then echo "HOSTILE SELF-TEST: OK"; else echo "HOSTILE SELF-TEST: FAILED"; fi
  exit $fail
fi

# Run the compiler on every case.
if [ -z "$ostrel" ]; then
  ostrel="${CARGO_TARGET_DIR:-$root/target}/release/ostrel"
fi
if [ ! -x "$ostrel" ]; then echo "HOSTILE FAIL: compiler binary not found: $ostrel (build it or pass --ostrel)"; exit 1; fi

commands="check run"
[ "$lim_fmt" != "-" ] && commands="check run fmt"
mem_kib=$((lim_mem * 1024))
errfile="$out/stderr.txt"
total=0
for name in $(printf '%s\n' "${!where[@]}" | LC_ALL=C sort); do
  f="${where[$name]}"
  for cmd in $commands; do
    case "$cmd" in check) lim=$lim_check ;; run) lim=$lim_run ;; *) lim=$lim_fmt ;; esac
    total=$((total + 1))
    ( ulimit -v "$mem_kib"; exec timeout -k 1 "$lim" "$ostrel" "$cmd" "$f" ) < /dev/null > /dev/null 2> "$errfile"
    code=$?
    case "$code" in
      0|1) ;;
      124|137)
        if [ "$cmd" = run ] && [ "${exp_err[$name]:-}" = "runtime error[StepLimit]" ]; then
          problem "run $name: default StepLimit not reached within ${lim}s (code $code); defect, BLOCKER to chief per SPEC 12.4, do not loosen the limit"
        else
          problem "$cmd $name: no exit within ${lim}s (code $code)"
        fi
        continue ;;
      101) problem "$cmd $name: panic (exit 101)"; continue ;;
      *) if [ "$code" -gt 128 ]; then problem "$cmd $name: killed by signal $((code - 128))"; else problem "$cmd $name: exit $code, expected 0 or 1"; fi; continue ;;
    esac
    want="*"
    case "$cmd" in check) want="${exp_check[$name]:-*}" ;; run) want="${exp_run[$name]:-*}" ;; esac
    if [ "$want" != "*" ] && [ "$want" != "$code" ]; then
      problem "$cmd $name: exit $code, expected $want; stderr: $(head -c 300 "$errfile" | tr '\n' ' ')"
      continue
    fi
    if [ "$cmd" = run ] && [ -n "${exp_err[$name]:-}" ] && ! grep -qF -- "${exp_err[$name]}" "$errfile"; then
      problem "run $name: stderr lacks '${exp_err[$name]}'; stderr: $(head -c 300 "$errfile" | tr '\n' ' ')"
    fi
  done
  # StepLimit cases run a second time with a small --max-steps (README, step limit), so the
  # flag and the StepLimit path are checked independently of machine speed.
  if [ "${exp_err[$name]:-}" = "runtime error[StepLimit]" ]; then
    total=$((total + 1))
    ( ulimit -v "$mem_kib"; exec timeout -k 1 "$lim_run" "$ostrel" run --max-steps "$small_steps" "$f" ) < /dev/null > /dev/null 2> "$errfile"
    code=$?
    if [ "$code" != 1 ]; then
      problem "run --max-steps $small_steps $name: exit $code, expected 1; stderr: $(head -c 300 "$errfile" | tr '\n' ' ')"
    elif ! grep -qF -- "runtime error[StepLimit]" "$errfile"; then
      problem "run --max-steps $small_steps $name: stderr lacks 'runtime error[StepLimit]'; stderr: $(head -c 300 "$errfile" | tr '\n' ' ')"
    fi
  fi
done
echo "hostile: $total runs over ${#where[@]} cases"
if [ $fail -eq 0 ]; then echo "HOSTILE: OK"; else echo "HOSTILE: FAILED"; fi
exit $fail
