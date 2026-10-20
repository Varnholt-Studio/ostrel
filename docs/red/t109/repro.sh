#!/usr/bin/env bash
# Reproduces the findings of docs/red/t109/README.md with a release build.
# Usage: bash docs/red/t109/repro.sh [path/to/ostrel]
# Timings are CPU seconds (user) as in D93; compare them on the same machine.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
ostrel="${1:-${CARGO_TARGET_DIR:-$root/target}/release/ostrel}"
probes="$here/probes"
err="$(mktemp)"
trap 'rm -f "$err"' EXIT

cpu() {
    TIMEFORMAT="%U"
    { time "$ostrel" run "$1" >/dev/null 2>"$2"; } 2>&1
}

echo "F1: CPU seconds until StepLimit (default 100 000 000 steps)"
for f in "$root/tests/runtime/step_limit_default.ostl" \
    "$probes/f1_interpolate_text.ostl" "$probes/f1_interpolate_int.ostl"; do
    t=$(cpu "$f" "$err")
    printf '  %-28s %6s s  %s\n' "$(basename "$f")" "$t" \
        "$(grep -o 'error\[[A-Za-z]*\]' "$err")"
done

echo "F2: bytes written to stdout within the default step limit"
bytes=$("$ostrel" run "$probes/f2_print_volume.ostl" 2>"$err" | wc -c)
echo "  $bytes bytes, $(grep -o 'error\[[A-Za-z]*\]' "$err")"

echo "F3: kind reported for the Concat refused by HeapLimit"
for x in 4194986 4326057 4326058; do
    kind=$("$ostrel" run --max-steps "$x" "$probes/f3_heap_limit_edge.ostl" 2>&1 >/dev/null \
        | grep -o 'error\[[A-Za-z]*\]')
    echo "  --max-steps $x: $kind"
done
