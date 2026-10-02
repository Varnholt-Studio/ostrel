#!/usr/bin/env bash
# Zentrales Qualitaets-Gate. Laeuft lokal (bash ci/gate.sh) und serverseitig bei jedem Push auf dev/main.
# Neue Pruefungen: Datei ci/checks/NN_name.sh anlegen (Besitz: QA). Reihenfolge = Dateiname.
set -uo pipefail
cd "$(dirname "$0")/.."
# Shared build cache keeps repeated builds fast on small machines.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/ostrel-target}"
for check in ci/checks/*.sh; do
  echo "== $check"
  if bash "$check"; then echo "   OK"; else echo "   FAIL: $check"; echo "GATE: ROT"; exit 1; fi
done
echo "GATE: GRUEN"
