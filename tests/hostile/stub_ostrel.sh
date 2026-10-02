#!/usr/bin/env bash
# Stand-in for the compiler in the runner self test of run.sh (QA-1). Not a compiler.
#
#   stub_ostrel.sh COMMAND [--max-steps N] FILE
#
# Logs "COMMAND [--max-steps N] CASE" to $HOSTILE_STUB_LOG and answers as
# $HOSTILE_STUB_EXPECT (tests/hostile/expect.txt) says: the expected exit code (0 for `*`,
# for fmt and for unlisted cases) and, with exit 1, the expected stderr text.
# $HOSTILE_STUB_BREAK="COMMAND CASE" flips the exit code of that one call;
# "stderr CASE" drops the stderr text of that case.
set -u
log="${HOSTILE_STUB_LOG:?}"
expect="${HOSTILE_STUB_EXPECT:?}"
cmd="${1:-}"
shift
steps=""
if [ "${1:-}" = --max-steps ]; then
  steps="${2:-}"
  shift 2
fi
file="${1:-}"
name=$(basename "$file")
echo "$cmd${steps:+ --max-steps $steps} $name" >> "$log"
line=$(grep -m 1 -E -- "^${name//./\\.} " "$expect" || true)
c="" r="" rest=""
[ -n "$line" ] && read -r _ c r rest <<< "$line"
case "$cmd" in
  check) code="${c:-*}" ;;
  run) code="${r:-*}" ;;
  *) code=0 ;;
esac
[ "$code" = "*" ] && code=0
if [ "${HOSTILE_STUB_BREAK:-}" = "$cmd $name" ]; then code=$((1 - code)); fi
[ "${HOSTILE_STUB_BREAK:-}" = "stderr $name" ] && rest=""
if [ "$code" = 1 ] && [ -n "$rest" ]; then echo "$file:1:1: $rest: stub answer" >&2; fi
exit "$code"
