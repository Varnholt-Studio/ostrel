#!/usr/bin/env bash
# Legal files (AC-07, D1, MEASUREMENT 5.2):
#   the repository root contains LICENSE-MIT, LICENSE-APACHE and AI_DISCLOSURE.md, each not
#   empty, and the two license files carry the text of their license,
#   every Cargo.toml declares license = "MIT OR Apache-2.0", either directly in [package] or
#   [workspace.package], or as license.workspace = true in [package] when the root
#   Cargo.toml sets that license in [workspace.package]. license-file is not accepted.
# AI_DISCLOSURE.md is written only by the chief (SPEC 5.0 AC-07); this check reads it for
# existence and never changes it.
#
# Manifests read: in a git checkout the tracked and the new not ignored files, in an
# exported tree (the server gate runs on `git archive` output) every file outside target/,
# node_modules/ and .git/.
#
# A self test (legal_selftest) runs first on scratch trees and proves that every rule above
# can fail.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

LICENSE_VALUE='MIT OR Apache-2.0'

# manifests ROOT: NUL separated paths (relative to ROOT) of every Cargo.toml in ROOT.
manifests() {
  local root=$1
  if git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1 &&
    [ "$(git -C "$root" rev-parse --show-toplevel)" = "$(cd "$root" && pwd -P)" ]; then
    git -C "$root" ls-files -z -co --exclude-standard -- 'Cargo.toml' '*/Cargo.toml' |
      while IFS= read -r -d '' f; do
        [ -f "$root/$f" ] && printf '%s\0' "$f"
      done
  else
    (cd "$root" && find . \( -path ./.git -o -path ./target -o -name node_modules \) -prune \
      -o -type f -name Cargo.toml -print0) |
      while IFS= read -r -d '' f; do printf '%s\0' "${f#./}"; done
  fi
}

# license_kind FILE: prints how FILE declares its license, one word per declaration:
#   direct   license = "MIT OR Apache-2.0" in [package] or [workspace.package]
#   inherit  license.workspace = true or license = { workspace = true } in [package]
#   wrong    any other license or license-file entry in [package] or [workspace.package]
license_kind() {
  awk -v want="$LICENSE_VALUE" '
    function trim(s) { gsub(/^[[:space:]]+|[[:space:]]+$/, "", s); return s }
    /^[[:space:]]*\[/ { s = $0; gsub(/[[:space:]]/, "", s); sub(/\].*/, "]", s); cur = s; next }
    cur != "[package]" && cur != "[workspace.package]" { next }
    /^[[:space:]]*license-file[[:space:]]*=/ { print "wrong"; next }
    /^[[:space:]]*license[[:space:]]*\.[[:space:]]*workspace[[:space:]]*=[[:space:]]*true[[:space:]]*(#.*)?$/ {
      print (cur == "[package]" ? "inherit" : "wrong"); next
    }
    /^[[:space:]]*license[[:space:]]*=/ {
      v = $0; sub(/^[^=]*=/, "", v); sub(/#.*$/, "", v); v = trim(v)
      if (v == "\"" want "\"") { print "direct" }
      else if (cur == "[package]" && v ~ /^\{[[:space:]]*workspace[[:space:]]*=[[:space:]]*true[[:space:]]*\}$/) { print "inherit" }
      else { print "wrong" }
    }' "$1"
}

# ac_07_legal_files ROOT: applies every rule above to the tree at ROOT; prints one line per
# violation and returns 1 if any.
ac_07_legal_files() {
  local root=$1 bad=0 f kinds root_ok=0
  for f in LICENSE-MIT LICENSE-APACHE AI_DISCLOSURE.md; do
    if [ ! -f "$root/$f" ]; then
      echo "   LEGAL: $f missing in the repository root"
      bad=1
    elif [ ! -s "$root/$f" ]; then
      echo "   LEGAL: $f is empty"
      bad=1
    fi
  done
  if [ -s "$root/LICENSE-MIT" ] &&
    ! { grep -q 'MIT License' "$root/LICENSE-MIT" &&
      grep -q 'Permission is hereby granted, free of charge' "$root/LICENSE-MIT"; }; then
    echo "   LEGAL: LICENSE-MIT does not carry the MIT license text"
    bad=1
  fi
  if [ -s "$root/LICENSE-APACHE" ] &&
    ! { grep -q 'Apache License' "$root/LICENSE-APACHE" &&
      grep -q 'Version 2.0, January 2004' "$root/LICENSE-APACHE"; }; then
    echo "   LEGAL: LICENSE-APACHE does not carry the Apache License 2.0 text"
    bad=1
  fi

  if [ -f "$root/Cargo.toml" ] &&
    awk '/^[[:space:]]*\[/ { s = $0; gsub(/[[:space:]]/, "", s); cur = s; next }
      cur == "[workspace.package]" && /^[[:space:]]*license[[:space:]]*=/ { found = 1 }
      END { exit found ? 0 : 1 }' "$root/Cargo.toml" &&
    [ "$(license_kind "$root/Cargo.toml" | sort -u)" = direct ]; then
    root_ok=1
  fi

  local count=0
  while IFS= read -r -d '' f; do
    count=$((count + 1))
    kinds=$(license_kind "$root/$f" | sort -u | tr '\n' ' ')
    kinds="${kinds% }"
    case "$kinds" in
      direct) ;;
      inherit)
        if [ "$root_ok" != 1 ]; then
          echo "   LEGAL: $f inherits the license, but the root Cargo.toml sets no license = \"$LICENSE_VALUE\" in [workspace.package]"
          bad=1
        fi
        ;;
      "")
        echo "   LEGAL: $f declares no license (want license = \"$LICENSE_VALUE\")"
        bad=1
        ;;
      *)
        echo "   LEGAL: $f declares a license other than \"$LICENSE_VALUE\" or a license-file"
        bad=1
        ;;
    esac
  done < <(manifests "$root")
  if [ -f "$root/Cargo.toml" ] && [ "$count" -eq 0 ]; then
    echo "   LEGAL: no Cargo.toml found although the root has one (file listing failed)"
    bad=1
  fi
  return $bad
}

legal_selftest() {
  local scratch bad=0
  scratch=$(mktemp -d) || return 1
  mkdir -p "$scratch/clean/crates/a" "$scratch/clean/tools/x" "$scratch/clean/target/t" || return 1
  (
    cd "$scratch/clean" || exit 1
    printf 'MIT License\n\nPermission is hereby granted, free of charge, to any person\n' >LICENSE-MIT
    printf '   Apache License\n   Version 2.0, January 2004\n' >LICENSE-APACHE
    printf '# Disclosure\n' >AI_DISCLOSURE.md
    printf '[workspace]\nmembers = ["crates/*"]\n\n[workspace.package]\nlicense = "%s"\n' \
      "$LICENSE_VALUE" >Cargo.toml
    printf '[package]\nname = "a"\nlicense.workspace = true\n\n[dependencies]\nlicense = "x"\n' \
      >crates/a/Cargo.toml
    printf '[package]\nname = "x"\nlicense = "%s" # pinned\n' "$LICENSE_VALUE" >tools/x/Cargo.toml
    # A build directory is not part of the repository.
    printf '[package]\nname = "t"\nlicense = "GPL-3.0"\n' >target/t/Cargo.toml
  ) || return 1
  # sel LABEL WANT SETUP: runs ac_07_legal_files on a fresh copy after SETUP.
  sel() {
    local label=$1 want=$2 got
    shift 2
    rm -rf "$scratch/tree" && cp -r "$scratch/clean" "$scratch/tree" || return 1
    (cd "$scratch/tree" && eval "$*") || {
      echo "   selftest '$label': setup failed"
      return 1
    }
    ac_07_legal_files "$scratch/tree" >"$scratch/out" 2>&1
    got=$?
    if [ "$got" != "$want" ]; then
      echo "   selftest '$label': want exit $want, got $got"
      sed 's/^/      /' "$scratch/out"
      return 1
    fi
  }
  sel "clean tree passes" 0 true || bad=1
  sel "clean git checkout passes" 0 "git init -q . && printf '/target/\n' >.gitignore && git add -A" || bad=1
  sel "LICENSE-MIT missing" 1 "rm LICENSE-MIT" || bad=1
  sel "LICENSE-APACHE missing" 1 "rm LICENSE-APACHE" || bad=1
  sel "AI_DISCLOSURE.md missing" 1 "rm AI_DISCLOSURE.md" || bad=1
  sel "AI_DISCLOSURE.md empty" 1 ": >AI_DISCLOSURE.md" || bad=1
  sel "LICENSE-MIT with other text" 1 "printf 'All rights reserved\n' >LICENSE-MIT" || bad=1
  sel "LICENSE-APACHE with other text" 1 "printf 'Apache License\nVersion 1.1\n' >LICENSE-APACHE" || bad=1
  sel "license file only below the root" 1 "mkdir d && mv LICENSE-MIT d/" || bad=1
  sel "crate without license" 1 "sed -i '/^license.workspace/d' crates/a/Cargo.toml" || bad=1
  sel "crate with MIT only" 1 "sed -i 's/^license = .*/license = \"MIT\"/' tools/x/Cargo.toml" || bad=1
  sel "crate with license-file" 1 "printf 'license-file = \"L\"\n' >>tools/x/Cargo.toml" || bad=1
  sel "license only in dependencies table" 1 "sed -i '/^license.workspace/d' crates/a/Cargo.toml && printf '[dependencies]\nlicense = \"%s\"\n' '$LICENSE_VALUE' >>crates/a/Cargo.toml" || bad=1
  sel "root workspace license wrong" 1 "sed -i 's/^license = .*/license = \"MIT\"/' Cargo.toml" || bad=1
  sel "root workspace license missing" 1 "sed -i '/^license = /d' Cargo.toml" || bad=1
  sel "inherit form with braces passes" 0 "sed -i 's/^license.workspace = true/license = { workspace = true }/' crates/a/Cargo.toml" || bad=1
  sel "new not ignored manifest in git checkout" 1 "git init -q . && printf '/target/\n' >.gitignore && git add -A && mkdir -p crates/b && printf '[package]\nname = \"b\"\n' >crates/b/Cargo.toml" || bad=1
  sel "ignored build directory in git checkout" 0 "git init -q . && printf '/target/\n' >.gitignore && git add -A" || bad=1
  rm -rf "$scratch"
  return $bad
}

if ! legal_selftest; then
  echo "   LEGAL: self test failed"
  exit 1
fi
echo "   legal_selftest ok"

ac_07_legal_files "$PWD"
