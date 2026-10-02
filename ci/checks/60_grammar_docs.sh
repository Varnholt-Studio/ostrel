#!/usr/bin/env bash
# Grammar documentation check (MEASUREMENT 5.2 `60_grammar_docs.sh`, SPEC AC-06). Owner: QA.
#
# Structure rules (always blocking):
#   - docs/grammar.md exists and has at least one block tagged `ostl` and one tagged
#     `ostl-error` (an `ostl-error` block is what makes a do nothing compiler fail here);
#   - every fenced block in docs/**/*.md is closed.
# Suite ac_06_grammar_doc_blocks (blocking unless pending): every fenced block whose info
# string starts with the word `ostl` in docs/**/*.md is written to a scratch file and checked
# with `ostrel check FILE`:
#   ostl        exit 0
#   ostl-error  exit 1 and a diagnostic `FILE:LINE:COLUMN: ...` on stderr
# Any other exit code (101 panic, 124 timeout, signals) fails. Each block runs with
# `timeout 10`. A failure names the markdown file and the line of the opening fence.
#
# Compiler binary: $OSTREL if set, else `cargo build --locked --release -p ostrel_cli` and
# $CARGO_TARGET_DIR/release/ostrel.
#
# Pending marker: while ci/checks/60_grammar_docs.pending exists, suite failures are printed
# as PENDING and do not fail the gate; the structure rules and the self test still do. The
# check fails when the marker exists but every block passes. A pending run is never evidence
# for AC-06.
#
# A self test (grammar_docs_selftest) runs first against a fake compiler in a scratch tree
# and proves that every rule above can fail.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1
REPO=$(pwd)

PENDING_FILE=ci/checks/60_grammar_docs.pending
BLOCK_TIMEOUT=10

structure_fail=0
suite_fail=0
structure() { echo "   GRAMMAR DOCS STRUCTURE: $*"; structure_fail=1; }
failblock() { echo "   GRAMMAR DOCS FAIL: $*"; suite_fail=1; }

# extract MD OUTDIR: writes every ostl and ostl-error block of MD to OUTDIR as
# N.ostl with a line "N KIND LINE" in OUTDIR/index; reports unclosed fences.
extract() {
  local md=$1 out=$2
  awk -v out="$out" -v md="$md" '
    function close_block() { if (file != "") close(file); file = "" }
    /^[[:space:]]*(```|~~~)/ {
      match($0, /^[[:space:]]*(```+|~~~+)/)
      fence = substr($0, RSTART, RLENGTH); sub(/^[[:space:]]*/, "", fence)
      info = substr($0, RSTART + RLENGTH); sub(/^[[:space:]]+/, "", info)
      if (open == "") {
        open = fence; start = NR
        split(info, w, /[[:space:]]+/); tag = w[1]
        if (tag == "ostl" || tag == "ostl-error") {
          n++; file = out "/" n ".ostl"; printf "" > file
          print n, tag, NR >> (out "/index")
        }
        next
      }
      if (substr(fence, 1, 1) == substr(open, 1, 1) && length(fence) >= length(open) && info == "") {
        open = ""; close_block(); next
      }
    }
    { if (open != "" && file != "") print > file }
    END { if (open != "") { print md ":" start ": fenced block is not closed"; exit 1 } }
  ' "$md"
}

# has_diagnostic FILE STDERR: STDERR has a line `FILE:LINE:COLUMN: ...`.
has_diagnostic() {
  local l
  while IFS= read -r l; do
    if [[ "$l" == "$1:"* ]] && [[ "${l#"$1:"}" =~ ^[0-9]+:[0-9]+:\  ]]; then
      return 0
    fi
  done <"$2"
  return 1
}

# ac_06_grammar_doc_blocks OSTREL TMP: runs every block, reports failures.
ac_06_grammar_doc_blocks() {
  local ostrel=$1 tmp=$2 md dir n kind line rc file before
  local -i ok=0 total=0
  while IFS= read -r -d '' md; do
    dir="$tmp/blocks/$(printf '%s' "$md" | tr '/.' '__')"
    mkdir -p "$dir" || return 1
    if ! extract "$md" "$dir" >"$dir.err"; then
      while IFS= read -r line; do structure "$line"; done <"$dir.err"
      continue
    fi
    [ -f "$dir/index" ] || continue
    while read -r n kind line; do
      total+=1
      file="$dir/$n.ostl"
      timeout "$BLOCK_TIMEOUT" "$ostrel" check "$file" >"$dir/$n.out" 2>"$dir/$n.stderr" </dev/null
      rc=$?
      before=$suite_fail
      case "$kind:$rc" in
        ostl:0) ok+=1 ;;
        ostl-error:1)
          if has_diagnostic "$file" "$dir/$n.stderr"; then
            ok+=1
          else
            failblock "$md:$line: ostl-error block exits 1 without a FILE:LINE:COLUMN diagnostic"
          fi
          ;;
        ostl:*) failblock "$md:$line: ostl block: ostrel check exits $rc, want 0" ;;
        *) failblock "$md:$line: ostl-error block: ostrel check exits $rc, want 1" ;;
      esac
      if [ "$suite_fail" != "$before" ]; then
        head -n 5 "$dir/$n.stderr" | sed 's/^/      /'
      fi
    done <"$dir/index"
  done < <(find docs -type f -name '*.md' -print0 2>/dev/null | sort -z)
  echo "   grammar doc blocks: $ok of $total pass"
}

# structure_rules: docs/grammar.md exists and has both block kinds.
structure_rules() {
  local tmp
  if [ ! -f docs/grammar.md ]; then
    structure "docs/grammar.md missing"
    return
  fi
  tmp=$(mktemp -d) || return
  if extract docs/grammar.md "$tmp" >/dev/null && [ -f "$tmp/index" ]; then
    grep -q ' ostl ' "$tmp/index" || structure "docs/grammar.md has no block tagged ostl"
    grep -q ' ostl-error ' "$tmp/index" || structure "docs/grammar.md has no block tagged ostl-error"
  elif [ ! -f "$tmp/index" ]; then
    structure "docs/grammar.md has no block tagged ostl or ostl-error"
  fi
  rm -rf "$tmp"
}

# run_all OSTREL: structure rules plus suite in the current directory.
run_all() {
  local tmp
  tmp=$(mktemp -d) || return 1
  structure_rules
  ac_06_grammar_doc_blocks "$1" "$tmp"
  rm -rf "$tmp"
}

grammar_docs_selftest() {
  local scratch fake bad=0
  scratch=$(mktemp -d) || return 1
  fake="$scratch/fake-ostrel"
  # Fake compiler: behaviour is chosen by a word in the checked file.
  cat >"$fake" <<'EOF'
#!/usr/bin/env bash
[ "${1:-}" = check ] && [ -f "${2:-}" ] || exit 2
f=$2
if grep -q PANIC "$f"; then exit 101; fi
if grep -q HANG "$f"; then sleep 30; fi
if grep -q NODIAG "$f"; then echo "error" >&2; exit 1; fi
if grep -q BAD "$f"; then echo "$f:1:5: error[E0001]: bad" >&2; exit 1; fi
exit 0
EOF
  chmod +x "$fake"
  mkdir -p "$scratch/clean/docs/sub" || return 1
  printf '# Grammar\n\n```ostl\nfn main()\n```\n\n```ostl-error\nBAD\n```\n\n```text\nPANIC\n```\n' \
    >"$scratch/clean/docs/grammar.md"
  printf '````markdown\n```ostl\nPANIC\n```\n````\n\n~~~ostl\nfine\n~~~\n' >"$scratch/clean/docs/sub/other.md"
  # sel LABEL WANT_STRUCTURE WANT_SUITE SETUP
  sel() {
    local label=$1 ws=$2 wf=$3
    shift 3
    rm -rf "$scratch/tree" && cp -r "$scratch/clean" "$scratch/tree" || return 1
    (
      cd "$scratch/tree" || exit 1
      eval "$*" || exit 3
      structure_fail=0
      suite_fail=0
      run_all "$fake" >"$scratch/out" 2>&1
      [ "$structure_fail" = "$ws" ] && [ "$suite_fail" = "$wf" ]
    ) && return 0
    echo "   selftest '$label': want structure=$ws suite=$wf"
    sed 's/^/      /' "$scratch/out"
    return 1
  }
  sel "clean tree passes" 0 0 true || bad=1
  sel "ostl block fails check" 0 1 "sed -i 's/^fn main()/BAD/' docs/grammar.md" || bad=1
  sel "ostl-error block passes check" 0 1 "sed -i 's/^BAD$/fine/' docs/grammar.md" || bad=1
  sel "ostl-error without diagnostic" 0 1 "sed -i 's/^BAD$/BAD NODIAG/' docs/grammar.md" || bad=1
  sel "panic in ostl-error block" 0 1 "sed -i 's/^BAD$/PANIC/' docs/grammar.md" || bad=1
  sel "hang is a failure" 0 1 "sed -i 's/^fine$/HANG/' docs/sub/other.md" || bad=1
  sel "block in other doc fails" 0 1 "sed -i 's/^fine$/BAD/' docs/sub/other.md" || bad=1
  sel "tag with extra words" 0 1 "sed -i 's/^~~~ostl$/~~~ostl title/; s/^fine$/BAD/' docs/sub/other.md" || bad=1
  sel "grammar.md missing" 1 0 "rm docs/grammar.md" || bad=1
  sel "no ostl block" 1 0 "sed -i 's/^\`\`\`ostl$/\`\`\`text/' docs/grammar.md" || bad=1
  sel "no ostl-error block" 1 0 "sed -i 's/^\`\`\`ostl-error$/\`\`\`text/' docs/grammar.md" || bad=1
  sel "unclosed fence" 1 0 "printf '\`\`\`ostl\nfn main()\n' >>docs/sub/other.md" || bad=1
  rm -rf "$scratch"
  return $bad
}

if ! (BLOCK_TIMEOUT=2 && grammar_docs_selftest); then
  echo "   grammar docs self test failed"
  exit 1
fi
echo "   grammar_docs_selftest ok"

cd "$REPO" || exit 1
ostrel="${OSTREL:-}"
if [ -z "$ostrel" ]; then
  if ! cargo build --locked --release -q -p ostrel_cli; then
    echo "   GRAMMAR DOCS FAIL: ostrel_cli does not build"
    exit 1
  fi
  ostrel="${CARGO_TARGET_DIR:-target}/release/ostrel"
fi
case $ostrel in /*) ;; *) ostrel="$REPO/$ostrel" ;; esac
if [ ! -x "$ostrel" ]; then
  echo "   GRAMMAR DOCS FAIL: compiler binary $ostrel not found"
  exit 1
fi
structure_fail=0
suite_fail=0
run_all "$ostrel"
if [ "$structure_fail" = 1 ]; then
  exit 1
fi
if [ -f "$PENDING_FILE" ]; then
  if [ "$suite_fail" = 0 ]; then
    echo "   GRAMMAR DOCS STRUCTURE: every block passes; remove $PENDING_FILE"
    exit 1
  fi
  echo "   GRAMMAR DOCS PENDING: failures above are not blocking while $PENDING_FILE exists:"
  sed 's/^/      /' "$PENDING_FILE"
  echo "   NOT EVIDENCE for AC-06"
  exit 0
fi
exit "$suite_fail"
