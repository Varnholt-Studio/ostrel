#!/usr/bin/env bash
# Repository hygiene (MEASUREMENT 5.2): conflict markers, files over 5 MB, key material,
# shell syntax, symlinks, archive attributes and committed ignored files.
#
# Only files that belong to the repository are checked: in a git checkout the tracked and
# the new not ignored files (so a local target/ is never scanned), in an exported tree (the
# server gate runs on `git archive` output) every file.
#
# Key material that must be committed on purpose (for example published test vectors) is
# listed with its exact path and a reason in ci/checks/00_sanity.allow; nothing else is
# exempt, and the exemption covers only the key material check.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

bad=0
in_git=0
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then in_git=1; fi

files=()
if [ "$in_git" = 1 ]; then
  mapfile -d '' listed < <(git ls-files -z -co --exclude-standard)
else
  mapfile -d '' listed < <(find . -path ./.git -prune -o \( -type f -o -type l \) -print0 |
    sed -z 's|^\./||')
fi
links=()
for f in "${listed[@]}"; do
  if [ -L "$f" ]; then
    links+=("$f")
  elif [ -f "$f" ]; then
    files+=("$f")
  fi
done

# grep_files ARGS...: grep over the collected files, never reading options from file names.
grep_files() {
  [ ${#files[@]} -gt 0 ] || return 1
  printf '%s\0' "${files[@]}" | xargs -0 grep -HnI "$@" --
}

# Conflict markers: seven of the same character at line start, as git writes them.
if grep_files -E '^(<{7}|>{7}|[|]{7})( |$)'; then
  echo "conflict markers found"
  bad=1
fi

# Size limit.
for f in "${files[@]}"; do
  size=$(stat -c %s -- "$f")
  if [ "$size" -gt $((5 * 1024 * 1024)) ]; then
    echo "$f: file over 5 MB ($size bytes)"
    bad=1
  fi
done

# Key material. The patterns are split so that this script does not match itself.
allow_file=ci/checks/00_sanity.allow
declare -A allowed=()
if [ -f "$allow_file" ]; then
  while IFS= read -r line; do
    case "$line" in '' | '#'*) continue ;; esac
    path="${line%%[[:space:]]*}"
    reason="${line#"$path"}"
    if [ -z "${reason//[[:space:]]/}" ]; then
      echo "$allow_file: entry without reason: $path"
      bad=1
      continue
    fi
    allowed["$path"]=1
  done <"$allow_file"
fi
dashes="-----"
key_patterns=(
  -e "${dashes}BEGIN [A-Z0-9 ]*PRIVATE ""KEY( BLOCK)?${dashes}"
  -e "AKIA""[0-9A-Z]{16}"
  -e "gh[pousr]""_[A-Za-z0-9]{36}"
  -e "github""_pat_[A-Za-z0-9_]{22,}"
  -e "xox[abprs]""-[A-Za-z0-9-]{10,}"
  -e "k[1-4]\\.""secret\\.[A-Za-z0-9_-]{20,}"
)
while IFS= read -r hit; do
  path="${hit%%:*}"
  if [ -n "${allowed[$path]:-}" ]; then continue; fi
  echo "$hit"
  echo "   possible key material (exempt it only via $allow_file with a reason)"
  bad=1
done < <(grep_files -E "${key_patterns[@]}" | cut -c1-200)

# Shell syntax.
for f in "${files[@]}"; do
  case "$f" in
    *.sh)
      if ! bash -n -- "$f"; then
        echo "$f: shell syntax error"
        bad=1
      fi
      ;;
  esac
done

# Symlinks can point outside the repository and make checks read foreign files.
for f in "${links[@]}"; do
  echo "$f: symlinks are not allowed in the repository"
  bad=1
done

# Archive attributes would hide files from or change files in the server gate, which runs
# on `git archive` output.
for f in "${files[@]}"; do
  case "$f" in
    .gitattributes | */.gitattributes)
      if grep -HnE 'export-(ignore|subst)' -- "$f"; then
        echo "$f: export-ignore and export-subst are not allowed"
        bad=1
      fi
      ;;
  esac
done

# Files that are tracked although an ignore rule matches them (for example a build tree).
if [ "$in_git" = 1 ]; then
  while IFS= read -r -d '' f; do
    echo "$f: tracked file matches an ignore rule"
    bad=1
  done < <(git ls-files -z -ci --exclude-standard)
fi

exit $bad
