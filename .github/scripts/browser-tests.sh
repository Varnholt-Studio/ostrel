#!/usr/bin/env bash
# Browser job (SPEC 2 "Browser job", D15): runs every browser test of the repository in
# headless Chromium. Called by .github/workflows/browser.yml; also runs locally, but local
# runs are for development and are not evidence (D27).
#
# Contract with the test authors:
#   - A browser test is a file named *.spec.mjs or *.spec.js outside .git, target,
#     node_modules and bench (the same trees ci/checks/70_ac_coverage.sh reads). Files named
#     *.test.* belong to the offline gate (ci/checks/50_tests.sh) and never reach a browser.
#   - Each file runs under `node --test` (Node 22, built in runner, no npm install). Tests
#     that are evidence for an AC are titled `ac_NN_<topic>` (SPEC 2 "Test name").
#   - The environment provides OSTREL_CHROMIUM (absolute path of the Chromium binary) and,
#     when set by the caller, OSTREL_BIN (absolute path of the release `ostrel` binary).
#     A test starts its own browser with --headless=new and its own --user-data-dir.
#   - *.spec.ts and *.spec.mts are counted by the coverage check but cannot run without a
#     build step, so their presence fails the job instead of being skipped silently.
#
# Before the tests the job proves that the browser starts and renders a page, so a job
# without any browser test still fails when Chromium is broken.
set -uo pipefail
cd "$(dirname "$0")/../.."

fail() {
  echo "browser job: $*" >&2
  exit 1
}

find_chromium() {
  local name
  if [ -n "${OSTREL_CHROMIUM:-}" ]; then
    echo "$OSTREL_CHROMIUM"
    return
  fi
  for name in chromium chromium-browser google-chrome google-chrome-stable; do
    if command -v "$name" >/dev/null 2>&1; then
      command -v "$name"
      return
    fi
  done
}

chromium=$(find_chromium)
[ -n "$chromium" ] || fail "Chromium not found; set OSTREL_CHROMIUM"
[ -x "$chromium" ] || fail "not executable: $chromium"
version=$("$chromium" --version 2>/dev/null) || fail "cannot run $chromium --version"
echo "browser job: $version ($chromium)"
echo "browser job: $(node --version 2>/dev/null || echo 'node missing'), image ${ImageOS:-local} ${ImageVersion:-}"

profile=$(mktemp -d) || fail "cannot create a browser profile directory"
trap 'rm -rf "$profile"' EXIT

marker="ostrel-browser-job-$$"
page="data:text/html,<p id=m>$marker</p>"
dom=$(timeout 60 "$chromium" --headless=new --no-sandbox --disable-gpu \
  --user-data-dir="$profile" --dump-dom "$page" 2>/dev/null)
grep -q "<p id=\"m\">$marker</p>" <<<"$dom" || fail "headless Chromium did not render the smoke page"
echo "browser job: smoke page rendered"

mapfile -d '' typed < <(find . \( -path ./.git -o -path ./target -o -path '*/node_modules' \
  -o -path ./bench \) -prune -o -type f \( -name '*.spec.ts' -o -name '*.spec.mts' \) -print0 |
  sort -z)
if [ ${#typed[@]} -gt 0 ]; then
  printf 'browser job: cannot run without a build step: %s\n' "${typed[@]}" >&2
  exit 1
fi

mapfile -d '' specs < <(find . \( -path ./.git -o -path ./target -o -path '*/node_modules' \
  -o -path ./bench \) -prune -o -type f \( -name '*.spec.mjs' -o -name '*.spec.js' \) -print0 |
  sort -z)
if [ ${#specs[@]} -eq 0 ]; then
  echo "browser job: no browser tests (*.spec.mjs, *.spec.js) yet"
  exit 0
fi

printf 'browser job: %s\n' "${specs[@]}"
export OSTREL_CHROMIUM="$chromium"
node --test --test-reporter=spec "${specs[@]}" || fail "browser tests failed"
echo "browser job: ${#specs[@]} file(s) passed"
