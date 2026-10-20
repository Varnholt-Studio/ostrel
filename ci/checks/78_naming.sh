#!/usr/bin/env bash
# Public naming check (AC-53).
#   1. README.md names the project "Ostrel programming language" in its first heading or
#      first sentence.
#   2. No tracked file names a web address for the project. Until the domain is confirmed
#      (DOMAIN_CONFIRMED=1), this includes ostrel-lang.org; the project address is the GitHub
#      repository. Afterwards only ostrel-lang.org and its subdomains are allowed.
#   3. No tracked file uses "Ostrel" as the name of a company or organisation.
# A self test (ac_53_naming_selftest) runs first, in both domain modes: every forbidden phrase
# (also with other separators, case or a line break) and address in the fixture below must be
# caught, and the clean fixture must pass. This file is the fixture and is therefore excluded
# from the repository scan.
# The file list comes from git when the repository root is a git work tree, otherwise from
# the file system (for example an exported tree without .git). The check fails closed: a file
# list that is empty or misses a required file (REQUIRED below) is a failure, never a pass.
set -uo pipefail
cd "$(dirname "$0")/../.."

DOMAIN_CONFIRMED=0
SELF=ci/checks/78_naming.sh
REQUIRED=(README.md Cargo.toml)

PHRASES=(
  "Ostrel Inc" "Ostrel GmbH" "Ostrel Studio" "Ostrel team" "Ostrel company"
  "Ostrel Ltd" "Ostrel LLC" "Ostrel AG" "Ostrel Labs" "Ostrel Foundation"
  "Ostrel organisation" "Ostrel organization"
)

# Hosts that mention the project, from URLs and from bare domain names. Bare names are only
# matched for the TLDs below; .rs, .sh and similar are left out on purpose because they are
# file extensions. A host under any other TLD is caught only when written with a scheme.
hosts() {
  grep -oiE 'https?://[^/[:space:]"'"'"'<>)`]+' "$1" | sed -E 's#^[a-zA-Z]+://##; s#^.*@##; s#:[0-9]+$##'
  grep -oiE '[a-z0-9.-]*ostrel[a-z0-9.-]*\.(org|com|net|dev|io|app|ai|co|de|eu|info|page|site|xyz|tech|cloud)\b' "$1"
}

# scan FILE: prints one line per violation, returns 1 if any.
scan() {
  local f=$1 bad=0 p h words
  # Phrases match whole words, ignoring case and any run of spaces, punctuation or line breaks.
  words=" $(tr -cs '[:alnum:]' ' ' < "$f" | tr '[:upper:]' '[:lower:]') "
  for p in "${PHRASES[@]}"; do
    case $words in *" ${p,,} "*) echo "$f: forbidden phrase \"$p\""; bad=1 ;; esac
  done
  while IFS= read -r h; do
    h=${h,,}
    case $h in *ostrel*) ;; *) continue ;; esac
    if [ "$DOMAIN_CONFIRMED" = 1 ]; then
      case $h in ostrel-lang.org | *.ostrel-lang.org) continue ;; esac
    fi
    echo "$f: forbidden project address \"$h\""; bad=1
  done < <(hosts "$f" | sort -u)
  return $bad
}

# readme_ok FILE: first heading or first sentence contains the full name.
readme_ok() {
  local heading sentence
  heading=$(grep -m1 -E '^#' "$1")
  sentence=$(grep -vE '^(#|[[:space:]]*$)' "$1" | tr '\n' ' ' | sed -E 's/\. .*//')
  case "$heading" in *"Ostrel programming language"*) return 0 ;; esac
  case "$sentence" in *"Ostrel programming language"*) return 0 ;; esac
  return 1
}

# list_files: NUL separated paths, relative to the current directory, of the files to scan.
list_files() {
  if git rev-parse --is-inside-work-tree >/dev/null 2>&1 &&
    [ "$(git rev-parse --show-toplevel 2>/dev/null)" = "$(pwd -P)" ]; then
    git ls-files -z
  else
    find . \( -path ./.git -o -path ./target -o -name node_modules \) -prune -o -type f -print0 |
      sed -z 's|^\./||'
  fi
}

# check_tree: runs the whole check on the tree in the current directory, returns 1 on any
# violation, including an empty file list or a missing required file.
check_tree() {
  local bad=0 f r found n=0 files=()
  mapfile -d '' files < <(list_files)
  for r in "${REQUIRED[@]}"; do
    found=0
    for f in "${files[@]}"; do [ "$f" = "$r" ] && { found=1; break; }; done
    [ "$found" = 1 ] || { echo "file list misses required file $r (${#files[@]} files listed)"; bad=1; }
  done
  if [ -f README.md ]; then
    readme_ok README.md || { echo "README.md: first heading or sentence must name \"Ostrel programming language\""; bad=1; }
  fi
  for f in "${files[@]}"; do
    [ "$f" = "$SELF" ] && continue
    [ -f "$f" ] || continue
    grep -qI '' "$f" 2>/dev/null || continue
    n=$((n + 1))
    scan "$f" || bad=1
  done
  [ "$n" -gt 0 ] || { echo "no file scanned"; bad=1; }
  return $bad
}

ac_53_naming_selftest() {
  local tmp ok=0 p a v DOMAIN_CONFIRMED
  tmp=$(mktemp -d) || return 1
  trap 'rm -rf "$tmp"' RETURN
  for p in "${PHRASES[@]}"; do
    for v in "$p" "${p/ /, }." "${p/ /  }" "${p/ /-}" "${p/ /$'\n'}" "${p^^}"; do
      printf 'Made by the %s.\n' "$v" > "$tmp/f"
      scan "$tmp/f" > /dev/null && { echo "selftest: phrase not caught: $v"; ok=1; }
    done
  done
  for DOMAIN_CONFIRMED in 0 1; do
    for a in "https://ostrel-lang.org/docs" "docs.ostrel-lang.org" "www.ostrel.dev" \
      "https://Ostrel.io" "see ostrel-lang.com for more" "http://user@ostrel.app:8080/x"; do
      printf 'Visit %s now.\n' "$a" > "$tmp/f"
      case $DOMAIN_CONFIRMED:$a in
        1:*ostrel-lang.org*)
          scan "$tmp/f" > /dev/null || { echo "selftest: confirmed domain rejected: $a"; ok=1; } ;;
        *) scan "$tmp/f" > /dev/null && { echo "selftest: address not caught: $a"; ok=1; } ;;
      esac
    done
  done
  cat > "$tmp/clean" <<'EOF'
# Ostrel programming language
Source: https://github.com/varnholt/ostrel-lang and ostrel_core::canon.
Run `ostrel run main.ostl`; the Ostrel compiler includes a formatter (ostrel-lang).
EOF
  scan "$tmp/clean" || { echo "selftest: clean fixture rejected"; ok=1; }
  readme_ok "$tmp/clean" || { echo "selftest: README rule rejects a good heading"; ok=1; }
  printf '# Project\n\nThe Ostrel programming language is new. More.\n' > "$tmp/f"
  readme_ok "$tmp/f" || { echo "selftest: README rule rejects a good first sentence"; ok=1; }
  printf '# Ostrel\n\nA language. The Ostrel programming language.\n' > "$tmp/f"
  readme_ok "$tmp/f" && { echo "selftest: README rule accepts a missing name"; ok=1; }
  # Fail closed: an empty tree and a tree without required files must fail.
  mkdir "$tmp/empty" "$tmp/partial" "$tmp/export"
  (cd "$tmp/empty" && check_tree > /dev/null) && { echo "selftest: empty file list passes"; ok=1; }
  printf '# Ostrel programming language\n' > "$tmp/partial/README.md"
  (cd "$tmp/partial" && check_tree > /dev/null) &&
    { echo "selftest: file list without Cargo.toml passes"; ok=1; }
  # An exported tree without .git is scanned from the file system.
  printf '# Ostrel programming language\n' > "$tmp/export/README.md"
  printf '[workspace]\n' > "$tmp/export/Cargo.toml"
  mkdir "$tmp/export/docs"
  printf 'Plain text.\n' > "$tmp/export/docs/probe.md"
  (cd "$tmp/export" && check_tree > /dev/null) || { echo "selftest: clean export rejected"; ok=1; }
  printf 'Made by the Ostrel GmbH.\n' > "$tmp/export/docs/probe.md"
  (cd "$tmp/export" && check_tree > /dev/null) &&
    { echo "selftest: exported tree without .git not scanned"; ok=1; }
  return $ok
}

ac_53_naming_selftest || { echo "naming check self test failed"; exit 1; }
echo "   ac_53_naming_selftest ok"

check_tree
