#!/usr/bin/env bash
# Self test of 30_deps.sh: builds small fixture repositories in a temporary directory and
# expects the check to pass on the clean fixture and to fail, with the right message, on
# each broken variant. No cargo, no network.
set -uo pipefail
check="$(cd "$(dirname "$0")" && pwd)/30_deps.sh"
tmp=$(mktemp -d) || exit 1
trap 'rm -rf "$tmp"' EXIT
fail=0

# fixture DIR: a clean repository with one external crate (rustc-hash) and a git history
# whose policy file commit names a DECISION.
fixture() {
  local d=$1
  mkdir -p "$d/crates/app/src" "$d/ci" "$d/runtime/js"
  cat > "$d/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/app"]

[workspace.dependencies]
rustc-hash = "2"
EOF
  cat > "$d/crates/app/Cargo.toml" <<'EOF'
[package]
name = "app"
version = "0.1.0"

[dependencies]
rustc-hash = { workspace = true }
EOF
  cat > "$d/Cargo.lock" <<'EOF'
version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
 "rustc-hash",
]

[[package]]
name = "rustc-hash"
version = "2.1.1"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc"
EOF
  printf '# comment\nrustc-hash  *   # sema\nserde derive # wire\n' > "$d/ci/allowed-crates.txt"
  printf '# header\n\nrustc-hash 2.1.1 registry+https://github.com/rust-lang/crates.io-index abc\n' \
    > "$d/ci/deps-snapshot.txt"
  printf '{\n  "name": "runtime",\n  "dependencies": {}\n}\n' > "$d/runtime/js/package.json"
  git -C "$d" init -q -b dev
  git -C "$d" -c user.name=t -c user.email=t@t add -A
  git -C "$d" -c user.name=t -c user.email=t@t commit -qm "Base"
  git -C "$d" branch -q base
}

commit() { git -C "$1" -c user.name=t -c user.email=t@t commit -qam "$2"; }

# expect NAME WANT_RC PATTERN DIR
expect() {
  local name=$1 want=$2 pattern=$3 dir=$4 out rc
  out=$(GATE_BASE=base bash "$check" "$dir" 2>&1); rc=$?
  if [ "$rc" != "$want" ]; then
    echo "   selftest $name: exit $rc, expected $want"; echo "$out" | sed 's/^/      /'; fail=1; return
  fi
  if [ -n "$pattern" ] && ! grep -qF -- "$pattern" <<< "$out"; then
    echo "   selftest $name: output lacks '$pattern'"; echo "$out" | sed 's/^/      /'; fail=1
  fi
}

case_dir() { local d="$tmp/$1"; fixture "$d"; echo "$d"; }

# export_of DIR: commits every change in DIR and prints a directory holding
# `git archive HEAD` of it, without .git, as the server gate sees it.
export_of() {
  local src=$1 dst="$1.export"
  git -C "$src" add -A
  git -C "$src" -c user.name=t -c user.email=t@t commit -qm "Change" > /dev/null 2>&1
  mkdir -p "$dst" && git -C "$src" archive HEAD | tar -x -C "$dst"
  echo "$dst"
}

d=$(case_dir clean); expect clean 0 "" "$d"

d=$(case_dir not_allowed)
sed -i '/^rustc-hash/d' "$d/ci/allowed-crates.txt"
expect not_allowed 1 "depends on rustc-hash, which is not in ci/allowed-crates.txt" "$d"

d=$(case_dir snapshot_drift)
sed -i 's/2.1.1 registry/2.1.0 registry/' "$d/ci/deps-snapshot.txt"
expect snapshot_drift 1 "ci/deps-snapshot.txt differs from the external packages of Cargo.lock" "$d"

d=$(case_dir snapshot_missing)
rm "$d/ci/deps-snapshot.txt"
expect snapshot_missing 1 "Cargo.lock has external packages" "$d"

d=$(case_dir git_source)
sed -i 's#registry+https://github.com/rust-lang/crates.io-index#git+https://example.org/x#' "$d/Cargo.lock" "$d/ci/deps-snapshot.txt"
expect git_source_lock 1 "only crates.io is allowed" "$d"

d=$(case_dir git_manifest)
sed -i 's#^rustc-hash = "2"#rustc-hash = { git = "https://example.org/x" }#' "$d/Cargo.toml"
expect git_manifest 1 "git dependency" "$d"

d=$(case_dir patch)
printf '\n[patch.crates-io]\nrustc-hash = { path = "crates/app" }\n' >> "$d/Cargo.toml"
expect patch 1 "[patch] or [replace] is not allowed" "$d"

d=$(case_dir out_of_tree)
sed -i 's#^rustc-hash = "2"#rustc-hash = "2"\nother = { path = "../other" }#' "$d/Cargo.toml"
expect out_of_tree_manifest 1 "path dependency outside the repository" "$d"

d=$(case_dir lock_unknown_member)
printf '\n[[package]]\nname = "other"\nversion = "0.1.0"\n' >> "$d/Cargo.lock"
expect out_of_tree_lock 1 "is not a crate of this repository" "$d"

d=$(case_dir source_replace)
mkdir -p "$d/.cargo"; printf '[source.crates-io]\nreplace-with = "mirror"\n' > "$d/.cargo/config.toml"
expect source_replace 1 "source replacement or patch" "$d"

d=$(case_dir features_default_only)
sed -i 's#^rustc-hash = { workspace = true }#rustc-hash = { workspace = true, features = ["nightly"] }#' "$d/crates/app/Cargo.toml"
expect features_default_only 1 "allows only its defaults" "$d"

d=$(case_dir features_listed)
sed -i 's/^rustc-hash  \*/rustc-hash std,rand/' "$d/ci/allowed-crates.txt"
printf '\n[dependencies.rustc-hash]\nworkspace = true\nfeatures = ["std", "nightly"]\n' >> "$d/crates/app/Cargo.toml"
sed -i '/^\[dependencies\]$/,/^$/d' "$d/crates/app/Cargo.toml"
expect features_listed 1 "feature nightly of rustc-hash is not in ci/allowed-crates.txt" "$d"

d=$(case_dir features_multiline)
sed -i 's#^rustc-hash = "2"#rustc-hash = { version = "2", features = [#' "$d/Cargo.toml"
printf '  "std",\n] }\n' >> "$d/Cargo.toml"
expect features_multiline 1 "write the features of rustc-hash on one line" "$d"

d=$(case_dir commit_without_decision)
echo "anyhow * # new" >> "$d/ci/allowed-crates.txt"; commit "$d" "Allow anyhow"
expect commit_without_decision 1 "adds crates to ci/allowed-crates.txt (anyhow) without a DECISION id" "$d"

d=$(case_dir commit_with_decision)
echo "anyhow * # new" >> "$d/ci/allowed-crates.txt"; commit "$d" "Allow anyhow

Per D4."
expect commit_with_decision 0 "" "$d"

# D64: features, comments and a regenerated snapshot need no DECISION id.
d=$(case_dir commit_features_only)
sed -i 's/^serde derive/serde derive,std/' "$d/ci/allowed-crates.txt"
echo "# note" >> "$d/ci/allowed-crates.txt"; commit "$d" "Widen serde features"
expect commit_features_only 0 "" "$d"

d=$(case_dir commit_snapshot_only)
echo "# regenerated" >> "$d/ci/deps-snapshot.txt"; commit "$d" "Regenerate snapshot"
expect commit_snapshot_only 0 "" "$d"

d=$(case_dir npm_outside_bench)
printf '{ "devDependencies": { "left-pad": "1.0.0" } }\n' > "$d/runtime/js/package.json"
git -C "$d" add -A
expect npm_outside_bench 1 "runtime/js/package.json declares npm dependencies" "$d"

d=$(case_dir bench_without_lock)
mkdir -p "$d/bench"; printf '{ "devDependencies": { "yjs": "13.6.0" } }\n' > "$d/bench/package.json"
git -C "$d" add -A
expect bench_without_lock 1 "without a committed bench/package-lock.json" "$d"

d=$(case_dir bench_unlisted)
mkdir -p "$d/bench"
printf '{ "devDependencies": { "yjs": "13.6.0", "lodash": "4.0.0" } }\n' > "$d/bench/package.json"
printf '{ "lockfileVersion": 3, "packages": { "node_modules/yjs": { "resolved": "x", "integrity": "y" } } }\n' \
  > "$d/bench/package-lock.json"
git -C "$d" add -A
expect bench_unlisted 1 "lodash is not in the bench list" "$d"

d=$(case_dir bench_no_integrity)
mkdir -p "$d/bench"
printf '{ "devDependencies": { "yjs": "13.6.0" } }\n' > "$d/bench/package.json"
printf '{ "lockfileVersion": 3, "packages": { "node_modules/yjs": { "resolved": "x" } } }\n' \
  > "$d/bench/package-lock.json"
git -C "$d" add -A
expect bench_no_integrity 1 "1 resolved packages but 0 integrity hashes" "$d"

d=$(case_dir bench_ok)
mkdir -p "$d/bench"
printf '{ "devDependencies": { "yjs": "13.6.0", "@automerge/automerge": "2.0.0" } }\n' > "$d/bench/package.json"
printf '{ "lockfileVersion": 3, "packages": { "node_modules/yjs": { "resolved": "x", "integrity": "y" } } }\n' \
  > "$d/bench/package-lock.json"
git -C "$d" add -A
expect bench_ok 0 "" "$d"

# A new workspace member needs no snapshot change (D58).
d=$(case_dir new_member)
mkdir -p "$d/crates/two"; printf '[package]\nname = "two"\nversion = "0.1.0"\n' > "$d/crates/two/Cargo.toml"
sed -i 's#members = \["crates/app"\]#members = ["crates/*"]#' "$d/Cargo.toml"
printf '\n[[package]]\nname = "two"\nversion = "0.1.0"\n' >> "$d/Cargo.lock"
git -C "$d" add -A
expect new_member 0 "" "$d"

d=$(case_dir snapshot_lists_member)
printf 'app 0.1.0 - -\n' >> "$d/ci/deps-snapshot.txt"
expect snapshot_lists_member 1 "< app 0.1.0 - -" "$d"

d=$(case_dir features_dotted)
sed -i 's#^rustc-hash = { workspace = true }#rustc-hash.workspace = true\nrustc-hash.features = ["nightly"]#' "$d/crates/app/Cargo.toml"
expect features_dotted 1 "allows only its defaults" "$d"

d=$(case_dir source_replace_legacy)
mkdir -p "$d/.cargo"; printf '[source.crates-io]\nreplace-with = "mirror"\n' > "$d/.cargo/config"
expect source_replace_legacy 1 ".cargo/config: source replacement or patch" "$d"

# Exports without .git, as the server gate runs them (gatekeeper on git archive).
d=$(case_dir export_clean); e=$(export_of "$d")
[ -e "$e/.git" ] && { echo "   selftest export_clean: export has .git"; fail=1; }
mkdir -p "$e/target/package/x" "$e/runtime/js/node_modules/y"
printf '[package]\nname = "x"\n[dependencies]\nleft-pad = "1"\n' > "$e/target/package/x/Cargo.toml"
printf '{ "dependencies": { "left-pad": "1" } }\n' > "$e/runtime/js/node_modules/y/package.json"
expect export_clean 0 "commit message rule not checked" "$e"

d=$(case_dir export_not_allowed)
sed -i '/^rustc-hash/d' "$d/ci/allowed-crates.txt"; e=$(export_of "$d")
expect export_not_allowed 1 "depends on rustc-hash, which is not in ci/allowed-crates.txt" "$e"

d=$(case_dir export_npm)
printf '{ "devDependencies": { "left-pad": "1.0.0" } }\n' > "$d/runtime/js/package.json"; e=$(export_of "$d")
expect export_npm 1 "runtime/js/package.json declares npm dependencies" "$e"

d=$(case_dir export_features)
sed -i 's#^rustc-hash = { workspace = true }#rustc-hash = { workspace = true, features = ["nightly"] }#' "$d/crates/app/Cargo.toml"
e=$(export_of "$d")
expect export_features 1 "allows only its defaults" "$e"

d=$(case_dir export_snapshot_drift)
sed -i 's/2.1.1 registry/2.1.0 registry/' "$d/ci/deps-snapshot.txt"; e=$(export_of "$d")
expect export_snapshot_drift 1 "ci/deps-snapshot.txt differs" "$e"

if [ $fail -eq 0 ]; then echo "   30_deps self test: OK"; fi
exit $fail
