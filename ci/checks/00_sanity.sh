#!/usr/bin/env bash
# Repository hygiene (MEASUREMENT 5.2): conflict markers, files over 5 MB, key material,
# shell syntax, symlinks, archive attributes and committed ignored files.
#
# Only files that belong to the repository are checked: in a git checkout the tracked and
# the new not ignored files (so a local target/ or node_modules/ is never scanned), in an
# exported tree (the server gate runs on `git archive` output) every file.
#
# The one exception in both modes is the gate's own build directory: when CARGO_TARGET_DIR
# (exported by ci/gate.sh, D56) lies strictly inside the repository, files below it that are
# not tracked are skipped, so a second local run after a build stays green. A build directory
# equal to or above the repository root exempts nothing. The server gate builds outside the
# exported tree, so there a committed target/ or node_modules/ is still checked in full.
# Limit: a local export run with the gate.sh default (CARGO_TARGET_DIR=$PWD/target) skips a
# shipped target/ that carries a cargo CACHEDIR.TAG completely, key material included; only
# the server gate run is evidence.
#
# Key material that must be committed on purpose (for example published test vectors) is
# listed with its exact path and a reason in ci/checks/00_sanity.allow; nothing else is
# exempt, and the exemption covers only the key material check.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

# Self test: the file selection on small trees. Each case copies this script into a fresh
# tree and runs it there; SANITY_NESTED stops the copies from testing themselves again.
sanity_selftest() {
  local dir bad=0 tag='Signature: 8a477f597d28d172789f06886806bc55'
  dir=$(mktemp -d) || return 1
  # tree NAME: a new tree with this script, a source file and a filled build directory.
  tree() {
    mkdir -p "$dir/$1/ci/checks" "$dir/$1/src" "$dir/$1/target/debug" \
      "$dir/$1/bench/ref/node_modules/engine"
    cp -- "$0" "$dir/$1/ci/checks/00_sanity.sh"
    printf 'fn main() {}\n' >"$dir/$1/src/main.rs"
    printf '/target/\nnode_modules/\n' >"$dir/$1/.gitignore"
    printf '%s\n' "$tag" >"$dir/$1/target/CACHEDIR.TAG"
    truncate -s 6M "$dir/$1/target/debug/big.bin"
  }
  # expect LABEL WANT DIR [ENV...]: runs the copy in DIR; WANT is 0 (green) or 1 (red).
  expect() {
    local label=$1 want=$2 where=$3 got
    shift 3
    env -u CARGO_TARGET_DIR GIT_CEILING_DIRECTORIES="$dir" SANITY_NESTED=1 "$@" \
      bash "$where/ci/checks/00_sanity.sh" >/dev/null 2>&1
    got=$?
    if [ "$got" != "$want" ]; then
      echo "   self test case '$label': exit $got, expected $want"
      bad=1
    fi
  }
  tree exp
  expect "export, own build dir" 0 "$dir/exp" CARGO_TARGET_DIR="$dir/exp/target"
  expect "export, relative path into build dir" 0 "$dir/exp" \
    CARGO_TARGET_DIR="$dir/exp/src/../target"
  expect "export, no build dir given" 1 "$dir/exp"
  expect "export, build dir outside" 1 "$dir/exp" CARGO_TARGET_DIR="$dir/elsewhere"
  expect "export, build dir is the root" 1 "$dir/exp" CARGO_TARGET_DIR="$dir/exp"
  tree nm
  truncate -s 6M "$dir/nm/bench/ref/node_modules/engine/e.node"
  expect "export, node_modules is checked" 1 "$dir/nm" CARGO_TARGET_DIR="$dir/nm/target"
  tree untagged
  rm -f -- "$dir/untagged/target/CACHEDIR.TAG"
  expect "export, build dir without cargo tag" 1 "$dir/untagged" \
    CARGO_TARGET_DIR="$dir/untagged/target"
  tree big
  truncate -s 6M "$dir/big/src/data.bin"
  expect "export, large file outside build dir" 1 "$dir/big" CARGO_TARGET_DIR="$dir/big/target"
  tree git
  truncate -s 6M "$dir/git/bench/ref/node_modules/engine/e.node"
  git -C "$dir/git" init -q && git -C "$dir/git" add -A >/dev/null 2>&1
  expect "git, ignored build and node_modules" 0 "$dir/git"
  expect "git, own build dir" 0 "$dir/git" CARGO_TARGET_DIR="$dir/git/target"
  git -C "$dir/git" add -f target/debug/big.bin
  expect "git, tracked file in build dir" 1 "$dir/git" CARGO_TARGET_DIR="$dir/git/target"
  rm -rf -- "$dir"
  return $bad
}
if [ -z "${SANITY_NESTED:-}" ]; then
  if ! sanity_selftest; then
    echo "sanity self test failed"
    exit 1
  fi
  echo "   sanity_selftest ok"
fi

bad=0
in_git=0
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then in_git=1; fi

# build_rel: the build directory relative to the repository root, or empty when there is
# none inside it. Both sides are resolved without following a missing tail, so "a/../b" and
# symlinked parents cannot move the exemption onto source files.
build_rel=""
if [ -n "${CARGO_TARGET_DIR:-}" ]; then
  root_abs=$(pwd -P)
  build_abs=$(realpath -m -- "$CARGO_TARGET_DIR" 2>/dev/null) || build_abs=""
  case "$build_abs" in
    "$root_abs"/?*) build_rel="${build_abs#"$root_abs"/}" ;;
  esac
  # Only a directory cargo has marked as its cache is exempt, never a source directory.
  if [ -n "$build_rel" ] && ! grep -qs '^Signature: 8a477f597d28d172789f06886806bc55' \
    -- "$build_abs/CACHEDIR.TAG"; then
    build_rel=""
  fi
fi

# glob_quote STRING: STRING with find(1) pattern characters escaped.
glob_quote() {
  local s=$1 out="" c i
  for ((i = 0; i < ${#s}; i++)); do
    c=${s:i:1}
    case "$c" in '*' | '?' | '[' | ']' | '\\') out+="\\$c" ;; *) out+="$c" ;; esac
  done
  printf '%s' "$out"
}

files=()
if [ "$in_git" = 1 ]; then
  mapfile -d '' listed < <(git ls-files -z -c)
  mapfile -d '' others < <(git ls-files -z -o --exclude-standard)
  for f in "${others[@]}"; do
    if [ -n "$build_rel" ] && [[ "$f" == "$build_rel"/* ]]; then continue; fi
    listed+=("$f")
  done
else
  prune=(-path ./.git)
  if [ -n "$build_rel" ]; then prune+=(-o -path "./$(glob_quote "$build_rel")"); fi
  mapfile -d '' listed < <(find . \( "${prune[@]}" \) -prune -o \( -type f -o -type l \) \
    -print0 | sed -z 's|^\./||')
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
