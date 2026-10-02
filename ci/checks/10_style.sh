#!/usr/bin/env bash
# Repository style rules that the server side style check also enforces (MEASUREMENT 5.2),
# so a builder sees them before pushing:
#   no long or short dash characters (U+2014, U+2013) in files, file names and commit messages,
#   no attribution lines in files and commit messages.
# AI_DISCLOSURE.md and verbatim legal or generated files are exempt, as on the server.
# Binary files and files over 2 MB are skipped, as on the server.
# Patterns are built from bytes and fragments so that this script does not match itself.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1
export LC_ALL=C

bad=0
in_git=0
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then in_git=1; fi

if [ "$in_git" = 1 ]; then
  mapfile -d '' listed < <(git ls-files -z -co --exclude-standard)
else
  mapfile -d '' listed < <(find . -path ./.git -prune -o -type f -print0 | sed -z 's|^\./||')
fi

files=()
for f in "${listed[@]}"; do
  case "$f" in
    AI_DISCLOSURE.md | LICENSE-APACHE | LICENSE-MIT | Cargo.lock | package-lock.json) continue ;;
  esac
  [ -f "$f" ] && [ ! -L "$f" ] || continue
  [ "$(stat -c %s -- "$f")" -le 2000000 ] || continue
  files+=("$f")
done

en_dash=$(printf '\342\200\223')
em_dash=$(printf '\342\200\224')
attribution="co""-authored-by|gene""rated (by|with)|machine[- ]gene""rated"

if [ ${#files[@]} -gt 0 ]; then
  if printf '%s\0' "${files[@]}" | xargs -0 grep -HnIF -e "$en_dash" -e "$em_dash" --; then
    echo "long or short dash characters found (use a comma, colon or parentheses)"
    bad=1
  fi
  if printf '%s\0' "${files[@]}" | xargs -0 grep -HnIiE -e "$attribution" --; then
    echo "attribution lines are not allowed"
    bad=1
  fi
  if printf '%s\n' "${files[@]}" | grep -nF -e "$en_dash" -e "$em_dash"; then
    echo "file names with dash characters found"
    bad=1
  fi
fi

# Commit messages of the branch, when a base is known.
if [ "$in_git" = 1 ]; then
  base="${GATE_BASE:-origin/dev}"
  if [[ "$base" != -* ]] && mb=$(git merge-base HEAD "$base" 2>/dev/null); then
    while IFS= read -r sha; do
      msg=$(git log -1 --format=%B "$sha")
      if printf '%s\n' "$msg" | grep -qF -e "$en_dash" -e "$em_dash"; then
        echo "commit ${sha:0:10}: message contains dash characters"
        bad=1
      fi
      if printf '%s\n' "$msg" | grep -qiE -e "$attribution"; then
        echo "commit ${sha:0:10}: message contains an attribution line"
        bad=1
      fi
    done < <(git rev-list "$mb..HEAD")
  fi
fi

exit $bad
