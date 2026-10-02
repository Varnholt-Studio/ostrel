#!/usr/bin/env bash
# Dependency policy (ARCHITECTURE section 11 and 11.1, decision D4, MEASUREMENT 5.2).
#
#   bash ci/checks/30_deps.sh            check the repository
#   bash ci/checks/30_deps.sh DIR        check another checkout (used by 30_deps_selftest.sh)
#   bash ci/checks/30_deps.sh --print-snapshot
#                                        print the expected ci/deps-snapshot.txt body
#                                        (external packages of Cargo.lock only)
#
# Rules:
#   1. Every direct dependency (normal, dev and build) of every workspace crate is a
#      workspace member or is listed in ci/allowed-crates.txt.
#   2. Explicit features of a listed crate are in its feature column; "*" allows only the
#      default feature set, so no explicit features.
#   3. Only crates.io sources: no git dependencies, no out of tree paths, no [patch],
#      [replace] or source replacement.
#   4. The external packages of Cargo.lock (those with a source) equal
#      ci/deps-snapshot.txt exactly. Workspace members are not in the snapshot, so a new
#      crates/ostrel_* crate needs no snapshot change (D58).
#   5. A commit that adds a crate name to ci/allowed-crates.txt names a DECISION id
#      (D<number>) in its message (D64). Feature changes, removals, comments and a
#      regenerated ci/deps-snapshot.txt need none. Commits checked: GATE_BASE..HEAD, where
#      GATE_BASE defaults to origin/dev when that ref exists. Needs the git history, so it
#      runs only in a git checkout (branch gates); the server gate checks an export without
#      .git and prints that the rule was not checked there.
#   6. No package.json outside bench/ declares dependencies (11.1). Every package.json
#      anywhere under bench/ (D75) has a committed package-lock.json next to it with
#      integrity hashes and only direct dependencies from the bench list of ARCHITECTURE
#      section 11 (D70).
#
# Rules 1 to 4 and 6 do not need git: in a checkout without .git (the server gate runs on a
# git archive export) the files are found with find(1) instead of git ls-files.
# Uses bash, awk, sed, grep, find, sort and git only; no network.
set -uo pipefail

# Bench only npm packages (ARCHITECTURE section 11, architect decision for MEASUREMENT 4.5, D70).
BENCH_NPM="playwright @playwright/test typescript prettier express prisma @prisma/client yjs
y-websocket @automerge/automerge react react-dom vite ws y-protocols lib0 @types/node
@types/express @types/ws"

CRATES_IO_SOURCES="registry+https://github.com/rust-lang/crates.io-index sparse+https://index.crates.io/"

# lock_packages LOCKFILE: "name version source checksum" per [[package]], "-" when absent.
# Same program as the regeneration command in the header of ci/deps-snapshot.txt.
lock_packages() {
  awk '/^\[\[package\]\]/{if(n)print n,v,s,c;n=v="";s=c="-"}
       /^name = /{n=$3} /^version = /{v=$3} /^source = /{s=$3}
       /^checksum = /{c=$3} END{if(n)print n,v,s,c}' "$1" | tr -d '"' | LC_ALL=C sort
}

# lock_external LOCKFILE: the lines of lock_packages for packages with a source, that is
# the body of ci/deps-snapshot.txt.
lock_external() {
  lock_packages "$1" | awk '$3 != "-"'
}

# repo_files DIR NAME: paths relative to DIR of every file named NAME in the checkout,
# sorted. git ls-files in a git checkout; otherwise find(1), skipping .git, target/ and
# node_modules/ (ignored in .gitignore, so never part of an export).
repo_files() {
  local dir=$1 name=$2 out
  if [ -e "$dir/.git" ] && out=$(cd "$dir" && git ls-files -- "$name" "*/$name" 2>/dev/null); then
    [ -n "$out" ] && printf '%s\n' "$out" | LC_ALL=C sort
    return 0
  fi
  (cd "$dir" && find . \( -name .git -o -path ./target -o -name node_modules \) -prune \
    -o -type f -name "$name" -print) | sed 's#^\./##' | LC_ALL=C sort
}

# lock_member_deps LOCKFILE: "member dependency" for every package without a source.
lock_member_deps() {
  awk '
    /^\[\[package\]\]/ { name=""; src=0; indeps=0; next }
    /^name = /         { name=$3; gsub(/"/, "", name); next }
    /^source = /       { src=1; next }
    /^dependencies = \[/ { indeps=1; next }
    indeps && /^\]/    { indeps=0; next }
    indeps && !src     { d=$1; gsub(/[",]/, "", d); print name, d }
  ' "$1"
}

# toml_dep_features FILE: "dep feature,feature" for every dependency entry with an
# explicit feature list. Supports inline tables on one line, [*dependencies.NAME]
# tables and dotted keys (NAME.features = [...]). A feature array spanning several lines is reported as "dep <multiline>".
toml_dep_features() {
  awk '
    function emit(dep, rest,   f) {
      if (rest ~ /features *= *\[[^]]*\]/) {
        f = rest; sub(/.*features *= *\[/, "", f); sub(/\].*/, "", f)
        gsub(/[" ]/, "", f); print dep, (f == "" ? "-" : f)
      } else if (rest ~ /features *= *\[/) {
        print dep, "<multiline>"
      }
    }
    /^[ \t]*\[/ {
      sect=$0; gsub(/[ \t\[\]]/, "", sect); table=""
      if (sect ~ /dependencies\./) { table=sect; sub(/.*dependencies\./, "", table) }
      next
    }
    table != "" && /^[ \t]*features *=/ { emit(table, $0); next }
    sect ~ /dependencies$/ && /^[ \t]*[A-Za-z0-9_-]+\.features *=/ {
      dep=$0; sub(/^[ \t]*/, "", dep); sub(/\..*/, "", dep); emit(dep, $0); next
    }
    sect ~ /dependencies$/ && /^[ \t]*[A-Za-z0-9_-]+ *= *\{/ {
      dep=$0; sub(/^[ \t]*/, "", dep); sub(/ *=.*/, "", dep); emit(dep, $0)
    }
  ' "$1"
}

# allow_names: the crate names (column 1) of a ci/allowed-crates.txt read from stdin,
# sorted and unique.
allow_names() {
  sed 's/#.*//' | awk 'NF{print $1}' | LC_ALL=C sort -u
}

# check_policy_commits DIR: rule 5. Prints problems, returns 1 on any.
check_policy_commits() {
  local dir=$1 base="${GATE_BASE:-}" c subj bad=0
  if [ ! -e "$dir/.git" ]; then
    echo "   deps: no git checkout (export without .git), commit message rule not checked"
    return 0
  fi
  if [[ "$base" == -* ]]; then
    echo "   deps: GATE_BASE must not start with '-': $base"
    return 1
  fi
  if [ -z "$base" ] && git -C "$dir" rev-parse -q --verify origin/dev > /dev/null; then base=origin/dev; fi
  if [ -z "$base" ] || ! git -C "$dir" rev-parse -q --verify "$base^{commit}" > /dev/null; then
    echo "   deps: no base ref (GATE_BASE or origin/dev), commit message rule not checked"
    return 0
  fi
  local added
  while read -r c; do
    added=$(LC_ALL=C comm -13 \
      <(git -C "$dir" show "$c^:ci/allowed-crates.txt" 2>/dev/null | allow_names) \
      <(git -C "$dir" show "$c:ci/allowed-crates.txt" 2>/dev/null | allow_names) | tr '\n' ' ')
    [ -z "$added" ] && continue
    if ! git -C "$dir" log -1 --format=%B "$c" | grep -qE '\bD[0-9]+\b'; then
      subj=$(git -C "$dir" log -1 --format=%s "$c")
      echo "   deps: commit ${c:0:10} ($subj) adds crates to ci/allowed-crates.txt (${added% }) without a DECISION id (D<number>) in its message"
      bad=1
    fi
  done < <(git -C "$dir" rev-list --no-merges "$base..HEAD" -- ci/allowed-crates.txt)
  return $bad
}

# check_repo DIR: runs every rule on the checkout in DIR. Prints problems, returns 1 on any.
check_repo() {
  local dir=$1 bad=0
  local allow="$dir/ci/allowed-crates.txt" snap="$dir/ci/deps-snapshot.txt" lock="$dir/Cargo.lock"
  problem() { echo "   deps: $*"; bad=1; }

  local -A allowed=() members=()
  if [ -f "$allow" ]; then
    while read -r name feats _; do
      [ -n "$name" ] && allowed[$name]="${feats:-*}"
    done < <(sed 's/#.*//' "$allow")
  fi

  # Cargo manifests tracked in the repository, and the crate names they define.
  local -a manifests=()
  mapfile -t manifests < <(repo_files "$dir" Cargo.toml)
  if [ -f "$dir/Cargo.toml" ] && [ ${#manifests[@]} -eq 0 ]; then
    problem "no Cargo manifests found, although Cargo.toml exists (file listing failed)"
  fi
  local m n
  for m in "${manifests[@]}"; do
    n=$(awk '/^\[package\]/{p=1;next} /^\[/{p=0} p&&/^name *=/{gsub(/[" ]/,"");sub(/name=/,"");print;exit}' "$dir/$m")
    [ -n "$n" ] && members[$n]=1
  done

  # Rule 3: sources in the manifests and in the cargo configuration.
  local line
  for m in "${manifests[@]}"; do
    while IFS= read -r line; do problem "$m: git dependency: ${line#*:}"; done \
      < <(grep -nE '(^|[{ ,])git *= *"' "$dir/$m")
    while IFS= read -r line; do problem "$m: [patch] or [replace] is not allowed: ${line#*:}"; done \
      < <(grep -nE '^\[(patch|replace)' "$dir/$m")
    while IFS=: read -r ln p; do
      local target
      target=$(cd "$dir/$(dirname "$m")" && realpath -m "$p")
      case "$target/" in
        "$(cd "$dir" && pwd -P)"/*) ;;
        *) problem "$m:$ln: path dependency outside the repository: $p" ;;
      esac
    done < <(grep -noE 'path *= *"[^"]*"' "$dir/$m" | sed -E 's/path *= *"([^"]*)"/\1/')
  done
  local cfg
  for cfg in .cargo/config.toml .cargo/config; do
    [ -f "$dir/$cfg" ] || continue
    while IFS= read -r line; do problem "$cfg: source replacement or patch: $line"; done \
      < <(grep -nE '^\[(source|patch)' "$dir/$cfg")
  done

  if [ -f "$lock" ]; then
    # Rule 3: sources in the lockfile; packages without a source must be in tree crates.
    local name ver src sum ok s
    while read -r name ver src sum; do
      if [ "$src" = "-" ]; then
        [ -n "${members[$name]:-}" ] || problem "Cargo.lock: $name $ver has no source and is not a crate of this repository (out of tree path)"
        continue
      fi
      ok=0
      for s in $CRATES_IO_SOURCES; do [ "$src" = "$s" ] && ok=1; done
      [ $ok -eq 1 ] || problem "Cargo.lock: $name $ver comes from $src, only crates.io is allowed"
    done < <(lock_packages "$lock")

    # Rule 1: direct dependencies of workspace crates.
    local member dep
    while read -r member dep; do
      [ -n "${members[$dep]:-}" ] && continue
      [ -n "${allowed[$dep]:-}" ] || problem "Cargo.lock: $member depends on $dep, which is not in ci/allowed-crates.txt"
    done < <(lock_member_deps "$lock")

    # Rule 4: snapshot of the external packages.
    if [ -f "$snap" ]; then
      local diffout
      if ! diffout=$(diff <(grep -vE '^(#|$)' "$snap") <(lock_external "$lock")); then
        problem "ci/deps-snapshot.txt differs from the external packages of Cargo.lock (< snapshot, > Cargo.lock):"
        echo "$diffout" | sed 's/^/      /'
      fi
    elif [ -n "$(lock_external "$lock")" ]; then
      problem "ci/deps-snapshot.txt is missing, but Cargo.lock has external packages"
    else
      # Phase in until INT-3 lands the file: without external packages there is nothing to pin.
      echo "   deps: ci/deps-snapshot.txt is missing; accepted because Cargo.lock has only crates of this repository"
    fi
  else
    problem "Cargo.lock is missing"
  fi

  # Rule 2: explicit features.
  local feats f
  for m in "${manifests[@]}"; do
    while read -r dep feats; do
      [ -n "${allowed[$dep]:-}" ] || continue
      if [ "$feats" = "<multiline>" ]; then
        problem "$m: write the features of $dep on one line, so this check can read them"
        continue
      fi
      [ "$feats" = "-" ] && continue
      if [ "${allowed[$dep]}" = "*" ]; then
        problem "$m: $dep enables features ($feats), ci/allowed-crates.txt allows only its defaults"
        continue
      fi
      for f in ${feats//,/ }; do
        case ",${allowed[$dep]}," in
          *",$f,"*) ;;
          *) problem "$m: feature $f of $dep is not in ci/allowed-crates.txt" ;;
        esac
      done
    done < <(toml_dep_features "$dir/$m")
  done

  # Rule 5: commit messages of commits that add crates to the allowlist (D64).
  check_policy_commits "$dir" || bad=1

  # Rule 6: npm (D51, D75). Every package.json under bench/ is a bench package; any other
  # package.json must not declare dependencies.
  local pj flat pdir block pkg
  while IFS= read -r pj; do
    flat=$(tr -d ' \t\r\n' < "$dir/$pj")
    case "$pj" in
      bench/*) ;;
      *)
        if echo "$flat" | grep -qE '"(dependencies|devDependencies|peerDependencies|optionalDependencies)":\{[^}]' \
          || echo "$flat" | grep -qE '"bundled?Dependencies":\[[^]]'; then
          problem "$pj declares npm dependencies; npm packages are allowed only under bench/"
        fi
        continue
        ;;
    esac
    # A bench package: committed lockfile next to it, integrity for every resolved package,
    # direct dependencies only from the bench list of ARCHITECTURE section 11.
    pdir=$(dirname "$pj")
    block="$pdir/package-lock.json"
    if [ ! -f "$dir/$block" ]; then
      problem "$pj without a committed $block"
    else
      grep -qE '"lockfileVersion": *[23]' "$dir/$block" || problem "$block: lockfileVersion 2 or 3 required"
      local resolved integrity
      resolved=$(grep -cE '"resolved": *"' "$dir/$block")
      integrity=$(grep -cE '"integrity": *"' "$dir/$block")
      [ "$resolved" -eq "$integrity" ] || problem "$block: $resolved resolved packages but $integrity integrity hashes"
    fi
    if echo "$flat" | grep -qE '"(peerDependencies|bundled?Dependencies)":[[{][^]}]'; then
      problem "$pj: peer and bundled dependencies are not allowed in bench packages"
    fi
    while IFS= read -r pkg; do
      [ -z "$pkg" ] && continue
      case " $(echo $BENCH_NPM) " in
        *" $pkg "*) ;;
        *) problem "$pj: $pkg is not in the bench list of ARCHITECTURE section 11" ;;
      esac
    done < <(echo "$flat" \
      | grep -oE '"(dependencies|devDependencies|optionalDependencies)":\{[^}]*\}' \
      | sed -E 's/^"[A-Za-z]+":\{//; s/\}$//' | tr ',' '\n' | sed -nE 's/^"([^"]+)":.*/\1/p')
  done < <(repo_files "$dir" package.json)

  return $bad
}

if [ "${1:-}" = "--print-snapshot" ]; then
  cd "$(dirname "$0")/../.." && lock_external Cargo.lock
  exit $?
fi

if [ $# -ge 1 ]; then
  target=$1
else
  target="$(cd "$(dirname "$0")/../.." && pwd -P)"
fi
check_repo "$target"
