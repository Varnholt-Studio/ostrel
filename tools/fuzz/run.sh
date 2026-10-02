#!/usr/bin/env bash
# Runs every campaign of tools/fuzz/seeds.txt against the ostrel CLI (AC-36).
#
#   bash tools/fuzz/run.sh [DURATION]     DURATION per campaign, default 30m (AC-36 minimum)
#
# Mutation seeds are the corpora that exist in the checkout: tests/hostile, tests/errors,
# examples. Findings go to tools/fuzz/findings/ (ignored by git). Exit code 0 when every
# campaign is clean, 1 when any campaign has findings, 2 on usage or build errors.
# Not part of the offline gate: this is a long run for the nightly job.
set -uo pipefail
cd "$(dirname "$0")/../.."

duration="${1:-30m}"
# One target directory for both builds; tools/fuzz is its own workspace (README.md).
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
target_dir="$CARGO_TARGET_DIR"

cargo build --locked --release -p ostrel_cli || exit 2
# No --locked: the fuzzer has no dependencies and no committed lock file.
cargo build --release --manifest-path tools/fuzz/Cargo.toml || exit 2
ostrel="$target_dir/release/ostrel"
fuzz="$target_dir/release/ostrel-fuzz"

corpus=()
for dir in tests/hostile tests/errors examples; do
  if [ -d "$dir" ]; then corpus+=(--corpus "$dir"); fi
done

status=0
while read -r target seed; do
  case "$target" in ''|'#'*) continue ;; esac
  echo "== campaign: $target seed $seed for $duration"
  "$fuzz" run --bin "$ostrel" --target "$target" --seed "$seed" --duration "$duration" \
    --out tools/fuzz/findings "${corpus[@]}"
  rc=$?
  if [ $rc -eq 1 ]; then status=1; elif [ $rc -ne 0 ]; then exit 2; fi
done < tools/fuzz/seeds.txt
exit $status
