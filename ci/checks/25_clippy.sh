#!/usr/bin/env bash
# Lints (MEASUREMENT 5.2, AC-05, S-2):
#   the workspace forbids unsafe code in [workspace.lints.rust],
#   every workspace member opts in with [lints] workspace = true,
#   crates that read untrusted input deny unwrap_used, expect_used, panic and
#   indexing_slicing at their crate root and never allow them again inside the crate,
#   cargo clippy --workspace --all-targets --locked -- -D warnings passes.
# In quick scope clippy runs only for the crates in GATE_CRATES; the configuration checks
# always cover the whole workspace because they are cheap.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

if [ ! -f Cargo.toml ]; then
  echo "   (no Cargo workspace, nothing to lint)"
  exit 0
fi

# Crates that read untrusted input. Extend this list when a crate's owners add the crate
# root attribute (architect review of INT-1, F1: ostrel_sema, ostrel_ir, ostrel_vm).
untrusted_input_crates=(ostrel_syntax ostrel_cli)
strict_lints=(unwrap_used expect_used panic indexing_slicing)

bad=0

# section_has FILE SECTION REGEX: true when REGEX matches a line inside [SECTION].
section_has() {
  awk -v want="[$2]" -v re="$3" '
    /^[[:space:]]*\[/ { gsub(/[[:space:]]/, ""); cur = $0; next }
    cur == want && $0 ~ re { found = 1 }
    END { exit found ? 0 : 1 }' "$1"
}

if ! section_has Cargo.toml workspace.lints.rust '^[[:space:]]*unsafe_code[[:space:]]*=[[:space:]]*"forbid"'; then
  echo "Cargo.toml: [workspace.lints.rust] must set unsafe_code = \"forbid\""
  bad=1
fi

for manifest in crates/*/Cargo.toml; do
  [ -f "$manifest" ] || continue
  if ! section_has "$manifest" lints '^[[:space:]]*workspace[[:space:]]*=[[:space:]]*true'; then
    echo "$manifest: missing [lints] workspace = true"
    bad=1
  fi
done

for crate in "${untrusted_input_crates[@]}"; do
  dir="crates/$crate"
  roots=()
  for r in "$dir/src/lib.rs" "$dir/src/main.rs"; do [ -f "$r" ] && roots+=("$r"); done
  if [ ${#roots[@]} -eq 0 ]; then
    echo "$dir: no crate root found"
    bad=1
    continue
  fi
  for root in "${roots[@]}"; do
    # Text of the first #![deny(...)] attribute, joined to one line.
    attr=$(awk '/#!\[deny\(/ { on = 1 } on { printf "%s ", $0 } on && /\)\]/ { exit }' "$root")
    for lint in "${strict_lints[@]}"; do
      if ! [[ "$attr" =~ clippy::${lint}[^a-z_] ]]; then
        echo "$root: crate root must deny clippy::$lint"
        bad=1
      fi
    done
  done
  # The strict lints must not be switched off again anywhere in the crate.
  re="(allow|expect|warn)\\([^)]*clippy::($(IFS='|'; echo "${strict_lints[*]}"))"
  if grep -rnE --include='*.rs' "$re" "$dir"; then
    echo "$dir: strict lints must not be relaxed inside the crate"
    bad=1
  fi
done

crates="${GATE_CRATES-all}"
if [ "$crates" = all ]; then
  cargo clippy --workspace --all-targets --locked -- -D warnings || bad=1
elif [ -z "$crates" ]; then
  echo "   (quick scope: no crate touched, clippy skipped)"
else
  args=()
  for c in $crates; do
    if ! [[ "$c" =~ ^[A-Za-z0-9_-]+$ ]]; then
      echo "invalid crate name in GATE_CRATES: $c"
      exit 1
    fi
    args+=(-p "$c")
  done
  cargo clippy "${args[@]}" --all-targets --locked -- -D warnings || bad=1
fi

exit $bad
