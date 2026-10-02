#!/usr/bin/env bash
# Dependency policy (ARCHITECTURE section 11 and 11.1, decision D4, MEASUREMENT 5.2).
#
#   bash ci/checks/30_deps.sh            check the repository
#   bash ci/checks/30_deps.sh DIR        check another checkout (used by 30_deps_selftest.sh)
#   bash ci/checks/30_deps.sh --print-snapshot
#                                        print the expected ci/deps-snapshot.txt body
#
# Rules:
#   1. Every direct dependency (normal, dev and build) of every workspace crate is a
#      workspace member or is listed in ci/allowed-crates.txt.
#   2. Explicit features of a listed crate are in its feature column; "*" allows only the
#      default feature set, so no explicit features.
#   3. Only crates.io sources: no git dependencies, no out of tree paths, no [patch],
#      [replace] or source replacement.
#   4. The package set of Cargo.lock equals ci/deps-snapshot.txt exactly.
#   5. A commit that changes ci/deps-snapshot.txt or ci/allowed-crates.txt names a
#      DECISION id (D<number>) in its message. Commits checked: GATE_BASE..HEAD, where
#      GATE_BASE defaults to origin/dev when that ref exists.
#   6. No package.json outside bench/ declares dependencies (11.1). bench/package.json, if
#      present, has a committed bench/package-lock.json with integrity hashes and only
#      direct dependencies from the bench list of ARCHITECTURE section 11.
#
# Uses bash, awk, sed, grep, sort and git only; no network.
set -uo pipefail

# Bench only npm packages (ARCHITECTURE section 11, architect decision for MEASUREMENT 4.5).
BENCH_NPM="playwright @playwright/test typescript prettier express prisma @prisma/client yjs
y-websocket @automerge/automerge react react-dom vite"

CRATES_IO_SOURCES="registry+https://github.com/rust-lang/crates.io-index sparse+https://index.crates.io/"

# lock_packages LOCKFILE: "name version source checksum" per [[package]], "-" when absent.
# Same program as the regeneration command in the header of ci/deps-snapshot.txt.
lock_packages() {
  awk '/^\[\[package\]\]/{if(n)print n,v,s,c;n=v="";s=c="-"}
       /^name = /{n=$3} /^version = /{v=$3} /^source = /{s=$3}
       /^checksum = /{c=$3} END{if(n)print n,v,s,c}' "$1" | tr -d '"' | LC_ALL=C sort
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
# explicit feature list. Supports inline tables on one line and [*dependencies.NAME]
# tables. A feature array spanning several lines is reported as "dep <multiline>".
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
    sect ~ /dependencies$/ && /^[ \t]*[A-Za-z0-9_-]+ *= *\{/ {
      dep=$0; sub(/^[ \t]*/, "", dep); sub(/ *=.*/, "", dep); emit(dep, $0)
    }
  ' "$1"
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
  mapfile -t manifests < <(cd "$dir" && git ls-files -- 'Cargo.toml' '*/Cargo.toml' 2>/dev/null)
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
  if [ -f "$dir/.cargo/config.toml" ]; then
    while IFS= read -r line; do problem ".cargo/config.toml: source replacement or patch: $line"; done \
      < <(grep -nE '^\[(source|patch)' "$dir/.cargo/config.toml")
  fi

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

    # Rule 4: snapshot.
    if [ -f "$snap" ]; then
      local diffout
      if ! diffout=$(diff <(grep -vE '^(#|$)' "$snap") <(lock_packages "$lock")); then
        problem "ci/deps-snapshot.txt differs from Cargo.lock (< snapshot, > Cargo.lock):"
        echo "$diffout" | sed 's/^/      /'
      fi
    elif lock_packages "$lock" | awk '$3 != "-" {found=1} END {exit !found}'; then
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

  # Rule 5: commit messages of changes to the policy files.
  local base="${GATE_BASE:-}"
  if [ -z "$base" ] && git -C "$dir" rev-parse -q --verify origin/dev > /dev/null; then base=origin/dev; fi
  if [ -n "$base" ] && git -C "$dir" rev-parse -q --verify "$base" > /dev/null; then
    local c subj
    while read -r c; do
      if ! git -C "$dir" log -1 --format=%B "$c" | grep -qE '\bD[0-9]+\b'; then
        subj=$(git -C "$dir" log -1 --format=%s "$c")
        problem "commit ${c:0:10} ($subj) changes the dependency policy files without a DECISION id (D<number>) in its message"
      fi
    done < <(git -C "$dir" rev-list --no-merges "$base..HEAD" -- ci/deps-snapshot.txt ci/allowed-crates.txt)
  else
    echo "   deps: no base ref (GATE_BASE or origin/dev), commit message rule not checked"
  fi

  # Rule 6: npm.
  local pj flat
  while IFS= read -r pj; do
    flat=$(tr -d ' \t\r\n' < "$dir/$pj")
    case "$pj" in
      bench/package.json) ;;
      *)
        if echo "$flat" | grep -qE '"(dependencies|devDependencies|peerDependencies|optionalDependencies)":\{[^}]' \
          || echo "$flat" | grep -qE '"bundled?Dependencies":\[[^]]'; then
          problem "$pj declares npm dependencies; npm packages are allowed only under bench/"
        fi
        ;;
    esac
  done < <(cd "$dir" && git ls-files -- 'package.json' '*/package.json' 2>/dev/null)

  if [ -f "$dir/bench/package.json" ]; then
    local block="$dir/bench/package-lock.json"
    if [ ! -f "$block" ]; then
      problem "bench/package.json without a committed bench/package-lock.json"
    else
      grep -qE '"lockfileVersion": *[23]' "$block" || problem "bench/package-lock.json: lockfileVersion 2 or 3 required"
      local resolved integrity
      resolved=$(grep -cE '"resolved": *"' "$block")
      integrity=$(grep -cE '"integrity": *"' "$block")
      [ "$resolved" -eq "$integrity" ] || problem "bench/package-lock.json: $resolved resolved packages but $integrity integrity hashes"
    fi
    local pkg
    while IFS= read -r pkg; do
      [ -z "$pkg" ] && continue
      case " $(echo $BENCH_NPM) " in
        *" $pkg "*) ;;
        *) problem "bench/package.json: $pkg is not in the bench list of ARCHITECTURE section 11" ;;
      esac
    done < <(tr -d ' \t\r\n' < "$dir/bench/package.json" \
      | grep -oE '"(dependencies|devDependencies|optionalDependencies)":\{[^}]*\}' \
      | sed -E 's/^"[A-Za-z]+":\{//; s/\}$//' | tr ',' '\n' | sed -nE 's/^"([^"]+)":.*/\1/p')
  fi

  return $bad
}

if [ "${1:-}" = "--print-snapshot" ]; then
  cd "$(dirname "$0")/../.." && lock_packages Cargo.lock
  exit $?
fi

if [ $# -ge 1 ]; then
  target=$1
else
  target="$(cd "$(dirname "$0")/../.." && pwd -P)"
fi
check_repo "$target"
