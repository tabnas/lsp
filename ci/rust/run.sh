#!/usr/bin/env bash
# Rust gate. Kept in one script so local and hosted validation cannot
# quietly drift apart: the ci-rust job in .github/workflows/ci.yml runs
# this file, and so can you. `make test-rs` is the fast inner loop; this
# is the full gate.
#
# The engine is a PATH DEPENDENCY on the sibling checkout
# (rs/Cargo.toml: `tabnas = { package = "tabnas-parser", path = "../../parser/rs" }`), and so are the
# BNF-dialect compilers behind the `dialects` feature and the fleet
# grammars behind `fleet`. These crates are on crates.io, but the
# committed manifest names them by path alone, so there is no registry
# version to fall back on, and cargo reads EVERY path
# dependency's manifest to resolve the graph, features on or off, so all
# of them have to be present to build at all. Clone them next to this
# repo before running (the ci-rust job clones the same list); the ci-go
# job resolves the engine through go.work, so the two ports are measured
# against the same engine.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)

cd "$ROOT/rs"

# Every sibling rs/Cargo.toml names by path, read from the manifest so
# this list cannot drift from it, checked before cargo fails on one with
# a less useful message. Their OWN path dependencies (bnf under the
# compilers, json under jsonic, hoover under ini, xml under feed) are read
# by cargo the same way; cargo names a missing one, and the workflow's
# clone list carries them.
SIBLINGS=$(awk '
  match($0, /path = "\.\.\/\.\.\/[^\/"]+\/rs"/) {
    s = substr($0, RSTART, RLENGTH)
    sub(/^path = "\.\.\/\.\.\//, "", s); sub(/\/rs"$/, "", s)
    print s
  }
' Cargo.toml | sort -u)
for SIBLING in $SIBLINGS; do
  if [[ ! -f "$ROOT/../$SIBLING/rs/Cargo.toml" ]]; then
    echo "no $SIBLING checkout at $ROOT/../$SIBLING/rs" >&2
    echo "clone https://github.com/tabnas/$SIBLING as a sibling of $(basename "$ROOT")" >&2
    exit 1
  fi
done

# Run through the MSRV toolchain when one is available. The workflow
# installs it explicitly, but a contributor running this script gets
# whatever `cargo` is on their PATH -- and a newer toolchain accepts code
# and formatting that the MSRV rejects, so the "local and hosted cannot
# drift" claim this script exists for would hold everywhere except the
# compiler version. Loud rather than silent when the toolchain is absent,
# because a quiet fallback is the drift.
#
# `rustup` treats `1.85` and `1.85.1` as DISTINCT versioned channels, so
# the full name of the toolchain that matched is what cargo is run
# through: `cargo +1.85` on a machine carrying only `1.85.1-…` resolves
# the absent `1.85` channel and tries to download it.
MSRV=$(awk -F'"' '/^rust-version = /{print $2; exit}' Cargo.toml)
CARGO=(cargo)
if [[ -n "$MSRV" ]]; then
  MSRV_RE=$(printf '%s' "$MSRV" | sed 's/\./\\./g')
  TOOLCHAIN=""
  if command -v rustup >/dev/null 2>&1; then
    # `|| true` because no match is the normal case this block exists to
    # handle: `grep` exits 1 on no match, `pipefail` makes that the
    # pipeline's status, and `set -e` would end the script silently.
    TOOLCHAIN=$(rustup toolchain list 2>/dev/null \
      | awk '{print $1}' \
      | grep -E "^${MSRV_RE}([.-]|$)" \
      | head -n 1) || true
  fi
  if [[ -n "$TOOLCHAIN" ]]; then
    CARGO=(cargo "+$TOOLCHAIN")
  else
    echo "warning: MSRV $MSRV is not installed; running on $(rustc --version 2>/dev/null)" >&2
    echo "         install it with: rustup toolchain install $MSRV" >&2
    echo "         a newer toolchain can accept what $MSRV rejects" >&2
  fi
fi

# Assert the lock's entry for THIS crate still matches the manifest,
# BEFORE anything runs cargo. Without `--locked` (see below) a cargo
# command silently rewrites Cargo.lock in the runner, so a version bump
# that updates rs/Cargo.toml and forgets rs/Cargo.lock passes every
# check and ships a stale lock. This has to come first -- after a cargo
# command the lock has already been fixed up and the check can never fail.
#
# Only this crate's entry is asserted. A sibling crate's entry legitimately
# moves whenever its checkout does, which is the same reason blanket
# `--locked` is wrong here.
CRATE=$(awk -F'"' '/^name = /{print $2; exit}' Cargo.toml)
WANT=$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)
HAVE=$(awk -v c="$CRATE" -F'"' '
  $0 == "name = \"" c "\"" { f = 1; next }
  f && /^version = / { print $2; exit }
' Cargo.lock)

if [[ "$WANT" != "$HAVE" ]]; then
  echo "Cargo.lock records $CRATE ${HAVE:-<missing>}, but Cargo.toml says $WANT" >&2
  echo "run a cargo command and commit the updated rs/Cargo.lock" >&2
  exit 1
fi

# That version check is the common case stated clearly; it is NOT the whole
# check. A pull request that adds, removes or re-pins a DEPENDENCY leaves
# the crate's own version alone, so it sails past the comparison above while
# leaving the committed lock stale -- cargo then regenerates it in the
# runner and everything goes green.
#
# So the resolution is compared before and after cargo runs -- but only the
# part this crate's own manifest decides. The engine and the other sibling
# crates are resolved from checkouts of MAIN, and whenever one of those
# checkouts bumps its version or adds, removes or re-pins one of ITS
# dependencies, cargo legitimately rewrites that crate's stanza and the
# transitive stanzas beneath it. A comparison of the whole lock would then
# fail every pull request here, Rust or not, until someone re-committed
# rs/Cargo.lock for another repository's change. What is compared is
# therefore the header, this crate's own stanza (its dependency list) and
# the stanzas of its DIRECT dependencies (their versions and sources),
# with every PATH dependency's version and dependency list masked: exactly
# the lines a change to rs/Cargo.toml moves, and none a sibling moves. A
# shared registry dependency a sibling drags to another version still
# shows, because that is this crate's own resolution changing too.
DIRECT=$(awk '
  /^\[/ { section = $0 }
  section ~ /dependencies/ && /^[A-Za-z0-9_-]+[ \t]*=/ {
    split($0, parts, "="); gsub(/[ \t]/, "", parts[1]); print parts[1]
  }
' Cargo.toml | sort -u | tr '\n' ' ')

# The direct dependencies declared with `path =`: the sibling crates,
# whose versions this manifest does not decide.
PATHDEPS=$(awk '
  /^\[/ { section = $0 }
  section ~ /dependencies/ && /^[A-Za-z0-9_-]+[ \t]*=.*path[ \t]*=/ {
    split($0, parts, "="); gsub(/[ \t]/, "", parts[1]); print parts[1]
  }
' Cargo.toml | sort -u | tr '\n' ' ')

lock_local_view() {
  awk -v crate="$CRATE" -v direct="$DIRECT" -v paths="$PATHDEPS" '
    BEGIN {
      n = split(direct, names, " ")
      for (i = 1; i <= n; i++) keep[names[i]] = 1
      keep[crate] = 1
      n = split(paths, names, " ")
      for (i = 1; i <= n; i++) sibling[names[i]] = 1
      keepit = 1            # the header before the first stanza
    }
    /^\[\[package\]\]$/ { name = ""; keepit = 0; indeps = 0; hold = $0; held = 1; next }
    held && /^name = / {
      name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name)
      keepit = (name in keep)
      held = 0
      if (keepit) { print hold; print }
      next
    }
    !keepit { next }
    (name in sibling) && /^version = / { print "version = \"<sibling>\""; next }
    (name in sibling) && /^dependencies = \[/ { indeps = 1; print "dependencies = [<sibling>]"; next }
    (name in sibling) && indeps { if ($0 ~ /^\]/) indeps = 0; next }
    { print }
  ' "$1"
}

LOCK_BEFORE=$(mktemp)
cp Cargo.lock "$LOCK_BEFORE"
# On any exit, a red run included, put the lock back if a cargo command
# rewrote it, then drop the snapshot: the tree is left as it was found.
trap 'if [ -f "$LOCK_BEFORE" ] && ! cmp -s "$LOCK_BEFORE" Cargo.lock; then cp "$LOCK_BEFORE" Cargo.lock; fi; rm -f "$LOCK_BEFORE"' EXIT

# NOT `--locked`, deliberately, and this is the one place a fleet crate's
# gate differs from the engine's own (parser ci/rust/run.sh does pass it).
# Cargo.lock records the engine and the other siblings by version, and
# they are resolved from checkouts of MAIN. So the day one of them bumps
# its crate version, `--locked` here fails with "cannot update the lock
# file" on every pull request in this repo, including ones that touch no
# Rust at all -- a red build caused by another repository's release.
#
# NOT `--all` on fmt either. cargo defines it as "all packages, and also
# their local path-based dependencies", and every sibling IS such a
# dependency, so `--all` reaches into the sibling checkouts: an
# unformatted file over there fails this gate even when every file here
# is clean.
# Every phase below is a transient task, and AGENTS.md wants a line from
# one at least every 30 seconds: a cold runner can spend longer than that
# inside one cargo phase compiling a single target with nothing printed,
# and silence reads as a hang. `phase` names the step, runs it, and keeps a
# heartbeat beside it until it ends; the phase's own output still flows.
phase() {
  local label=$1
  shift
  echo "gate: $label"
  (
    elapsed=0
    while sleep 30; do
      elapsed=$((elapsed + 30))
      echo "gate: $label ... still running (${elapsed}s)"
    done
  ) &
  local heartbeat=$!
  local rc=0
  "$@" || rc=$?
  kill "$heartbeat" 2>/dev/null || true
  wait "$heartbeat" 2>/dev/null || true
  return "$rc"
}

phase fmt "${CARGO[@]}" fmt --check
phase build "${CARGO[@]}" build --all-targets
phase test "${CARGO[@]}" test --all-targets
# `--all-targets` does NOT include doctests -- cargo documents the selector
# as "Test all targets (does not include doctests)" -- so a broken example
# in the crate docs (the README's fences run as doctests) passes a gate
# that only runs it.
phase "doc tests" "${CARGO[@]}" test --doc
phase clippy "${CARGO[@]}" clippy --all-targets --all-features -- -D warnings

# A broken or ambiguous intra-doc link is a rustdoc WARNING, and no arm
# above runs rustdoc over the crate docs. `-D warnings` through
# RUSTDOCFLAGS turns that into a failure; `--no-deps` keeps it about this
# crate rather than the engine.
phase rustdoc env RUSTDOCFLAGS="-D warnings" "${CARGO[@]}" doc --no-deps

# Now that cargo has had every chance to rewrite it, the lock must still
# describe the same resolution it did when committed.
if ! diff -q <(lock_local_view "$LOCK_BEFORE") \
             <(lock_local_view Cargo.lock) >/dev/null; then
  echo "rs/Cargo.lock does not match rs/Cargo.toml -- cargo rewrote it:" >&2
  diff <(lock_local_view "$LOCK_BEFORE") \
       <(lock_local_view Cargo.lock) >&2 || true
  echo >&2
  echo "run a cargo command and commit the updated rs/Cargo.lock" >&2
  exit 1
fi
