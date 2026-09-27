#!/usr/bin/env bash
# Rust gate. Kept in one script so local and hosted validation cannot
# quietly drift apart: the ci-rust job in .github/workflows/ci.yml runs
# this file, and so can you. `make test-rs` is the fast inner loop; this
# is the full gate.
#
# The engine is a PATH DEPENDENCY on the sibling checkout
# (rs/Cargo.toml: `tabnas = { path = "../../parser/rs" }`), and the crate
# is unpublished, so there is no registry version to fall back on. Clone
# https://github.com/tabnas/parser next to this repo before running; the
# ci-go job resolves the same sibling through go.work, so the two ports
# are measured against the same engine.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
ENGINE="$ROOT/../parser/rs"

if [[ ! -f "$ENGINE/Cargo.toml" ]]; then
  echo "no engine checkout at $ENGINE" >&2
  echo "clone https://github.com/tabnas/parser as a sibling of $(basename "$ROOT")" >&2
  exit 1
fi

cd "$ROOT/rs"

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
# Only this crate's entry is asserted. The engine's entry legitimately
# moves whenever the sibling checkout does, which is the same reason
# blanket `--locked` is wrong here.
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
# So the whole resolution is compared, before and after cargo runs, with one
# exemption: the engine's recorded version. That entry legitimately moves
# whenever the sibling checkout does, and exempting exactly it is what makes
# a full comparison usable here when blanket `--locked` is not.
lock_without_engine_version() {
  awk '
    /^\[\[package\]\]$/  { eng = 0 }
    /^name = "tabnas"$/  { eng = 1 }
    eng && /^version = / { print "version = \"<engine>\""; next }
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
# Cargo.lock records the engine by version, and the engine is resolved
# from a sibling checkout of MAIN. So the day parser bumps its crate
# version, `--locked` here fails with "cannot update the lock file" on
# every pull request in this repo, including ones that touch no Rust at
# all -- a red build caused by another repository's release.
#
# NOT `--all` on fmt either. cargo defines it as "all packages, and also
# their local path-based dependencies", and the engine IS such a
# dependency, so `--all` reaches into the sibling parser checkout: an
# unformatted file over there fails this gate even when every file here
# is clean.
"${CARGO[@]}" fmt --check
"${CARGO[@]}" build --all-targets
"${CARGO[@]}" test --all-targets
# `--all-targets` does NOT include doctests -- cargo documents the selector
# as "Test all targets (does not include doctests)" -- so a broken example
# in the crate docs (the README's fences run as doctests) passes a gate
# that only runs it.
"${CARGO[@]}" test --doc
"${CARGO[@]}" clippy --all-targets --all-features -- -D warnings

# A broken or ambiguous intra-doc link is a rustdoc WARNING, and no arm
# above runs rustdoc over the crate docs. `-D warnings` through
# RUSTDOCFLAGS turns that into a failure; `--no-deps` keeps it about this
# crate rather than the engine.
RUSTDOCFLAGS="-D warnings" "${CARGO[@]}" doc --no-deps

# Now that cargo has had every chance to rewrite it, the lock must still
# describe the same resolution it did when committed.
if ! diff -q <(lock_without_engine_version "$LOCK_BEFORE") \
             <(lock_without_engine_version Cargo.lock) >/dev/null; then
  echo "rs/Cargo.lock does not match rs/Cargo.toml -- cargo rewrote it:" >&2
  diff <(lock_without_engine_version "$LOCK_BEFORE") \
       <(lock_without_engine_version Cargo.lock) >&2 || true
  echo >&2
  echo "run a cargo command and commit the updated rs/Cargo.lock" >&2
  exit 1
fi
