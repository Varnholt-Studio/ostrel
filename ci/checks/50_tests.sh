#!/usr/bin/env bash
# Erkennt Testsuites und fuehrt sie aus. Code ohne Tests faellt durch.
set -uo pipefail
ran=0; fail=0
run() { echo "   -> $*"; ran=1; "$@" || fail=1; }
if ls -d tests test 2>/dev/null | grep -q . || find . -path ./.git -prune -o \( -name 'test_*.py' -o -name '*_test.py' \) -print | grep -q .; then
  if python3 -c 'import pytest' 2>/dev/null; then run python3 -m pytest -q
  elif [ -d tests ]; then run python3 -m unittest discover -s tests -p 'test*.py'
  else run python3 -m unittest discover -p 'test*.py'; fi
fi
if [ -f package.json ] && grep -q '"test"' package.json; then run npm test --silent; fi
if [ -f Makefile ] && grep -qE '^test:' Makefile; then run make -s test; fi
if [ -f go.mod ]; then run go test ./...; fi
if [ -f Cargo.toml ]; then run cargo test -q; fi
code=$(find . -path ./.git -prune -o -path ./ci -prune -o -path ./node_modules -prune -o -type f \( -name '*.py' -o -name '*.js' -o -name '*.ts' -o -name '*.go' -o -name '*.rs' -o -name '*.java' \) -print | head -1)
if [ $ran -eq 0 ]; then
  if [ -n "$code" ]; then echo "Code vorhanden ($code), aber keine Testsuite gefunden"; exit 1; fi
  echo "   (noch kein Code, nichts zu testen)"
fi
exit $fail
