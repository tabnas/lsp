# Agents Guide — lsp

## Core principle: dependencies change only on explicit instruction

**Dependencies may only be changed by explicit instruction from the
maintainer.** This covers every dependency this repository declares, in
every runtime and every manifest:

- `package.json` `dependencies`, `peerDependencies` and `devDependencies`,
  and their lockfiles;
- `go.mod` `require` and `replace` lines, their versions, and `go.sum`;
- `Cargo.toml` dependency tables and `Cargo.lock`;
- any other manifest here, nested test modules included.

Adding, removing, re-pointing or re-versioning any of them is a
dependency change.

- **A dependency never arrives as a side effect.** Watch for an import,
  `go mod tidy`, `npm install`, `cargo update`, a stamped template, or a
  fix for something else. If a change would alter a dependency, stop and
  ask before making it. Do not make it and explain afterwards.
- **An explicit instruction names the change**, for example "bump the
  parser requirement in X to 0.12" or "cascade the parser release". A
  goal is not an instruction for its means. "Make CI green", "ship the C
  library" or "fix the build" does not authorise a dependency change,
  however direct the route through one looks.
- **This repository's own version sites are not dependencies.** They
  include the root entry of its own lockfile. A release bump moves them.
- **Versions track the latest release.** Every dependency is kept at
  its latest published version, and none is held on an older one. That
  is the maintainer's standing instruction, so moving a dependency to
  its latest version needs no further one. Holding a dependency back,
  or adding, removing or re-pointing one, still does.

## Core principle: transient tasks report progress

**Every transient task produces status output at least every 30 seconds,
with an estimate of how far through it is, as a percentage, where one can
be made.** This is the maintainer's instruction. A transient task is any
work that runs for a while and then ends: a build, a test or conformance
sweep, an install or a fetch, a release, a wait on CI, a benchmark, a
script or loop you write, and anything sent to the background.

- **Minimal is enough.** One line with the step and a count, such as
  `conformance: 412 of 1500 (27%)`, meets it. When no total is known, print
  what is known (the step, the current item, the elapsed time) and say the
  percentage is unknown rather than inventing one.
- **Build it into what you write.** A script or loop prints a line per
  item or per interval. A quiet tool gets its progress or verbose flag, or
  a wrapper that prints a heartbeat, so that nothing runs silent for more
  than 30 seconds.
- **Silence reads as a hang.** Whoever is watching, a person or an agent,
  cannot tell a slow task from a stuck one without it, and so cannot
  decide whether to wait or to stop it.

A quick command that finishes within 30 seconds needs nothing extra.

## What this project is

The unified tabnas language server and the language-server generator
(design: [`doc/design.md`](doc/design.md); history: the unified-LSP
notes in the `admin` repo). Two products over one core:

- **`tabnas-lsp --stdio`** — one server process serving every
  registered grammar, routing per document; grammars are added
  dynamically as plugin modules (L1), serialized `GrammarSpec` data
  (L2), or BNF-dialect text (L3) via workspace configuration.
- **`tabnas-lsp-gen`** — generates standalone single-language servers
  (Node package or Go module; the runtime follows where the grammar
  can execute) plus editor plugins (VS Code, Neovim, Emacs, Sublime,
  Helix, Kate, Zed scaffold). `--unified` regenerates this repo's own
  [`editors/`](editors/).

Every feature derives from the engine contract that shipped for the
LSP program (parser#94–#109, both runtimes): `parse.recover` /
`ParseRecover` (multi-error diagnostics), `sub({ruleDone})` /
`SubRuleDone` (outline), the reconciled lex trace (semantic tokens),
`continuations` / `Continuations` (completion).

## Repository map

| Path | What it is |
|---|---|
| `ts/` | **Canonical** TypeScript package (`@tabnas/lsp` on npm). Plain CommonJS, no build step. `src/`: `core.js` (protocol-free pipeline), `server.js` (protocol front-end), `registry.js` (routing), `documents.js` (position encoding), `instances.js` (cache/mux/quarantine), `loaders.js` (L1/L2/L3 + firewall), `generate.js` (the generator). Bins: `bin/tabnas-lsp.js`, `bin/tabnas-lsp-gen.js`. |
| `go/` | Go port — module `github.com/tabnas/lsp/go`. Same pipeline over the Go engine, plus a dependency-free stdio JSON-RPC server (`Serve`). Exists so generated Go servers are real: `tabnas-lsp-gen --runtime go` emits a module over this package. |
| `editors/` | **Generated** (`make gen-editors`) multi-language editor plugins for the unified server. Never hand-edit — `ts/test/geneditors.test.js` gates staleness. |
| `test/fixtures/` | Cross-runtime fixtures: `json-grammar.json` (the shared pure-data strict-JSON grammar, copied from `parser/ts/test/json-builder.fixture.json` with a drift gate) and `lsp-conformance.json` (diagnostics/outline/completion cases both runtimes execute). |
| `ts/data/` | Generated: `registry.json` (from fleet `tabnas.plugin.json` descriptors) and `diagnostic-fixtures.json` (262 cases from the fleet `test/spec` TSV corpus). Both generators refuse to write from a partial fleet checkout. |

## Authority and alignment rules

1. **TypeScript is canonical.** The Go port mirrors it **by fixture
   parity, not code sharing**: `test/fixtures/lsp-conformance.json` is
   executed by `ts/test/conformance.test.js` and
   `go/conformance_test.go`. A Go mismatch is a port defect; fixture
   values change only when the TS pipeline's behavior changes.
2. **An engine divergence found here is an engine bug there.** The
   conformance suite runs the same inputs through both engines; when
   the two runtimes disagree on parse results, fix `parser` (TS wins,
   per its DIVERGENCE.md bar) — do not paper over it in this repo.
   Precedent: the suite's first run caught Go recovery silently
   accepting trailing content that TS reported (`"x" q`), fixed in
   `parser/go/parser.go` with mirrored regression tests.
3. **Derive, never duplicate.** `ts/data/*` and `editors/` are
   generated, staleness-gated, and never hand-edited. The registry
   OVERRIDES table in `ts/tools/gen-registry.js` is interim
   scaffolding: a descriptor field beats it, and the generator names
   redundant overrides for deletion.
4. **Wrappers stay thin.** Generated servers pin *what* is served,
   never *how* — no pipeline logic in emitted code. If a generated
   artifact needs behavior, it belongs in `ts/src/` or `go/`.

## Build / test

```bash
make test          # both halves
make -C . test-ts  # cd ts && npm test  (node --test)
make -C . test-go  # go.work over ../parser/go, then go test
```

The TS package resolves the engine from the sibling checkout
(`file:../../parser/ts` devDependency) — build the engine first:
`cd ../parser/ts && npm i && npm run build`. The Go module's engine
floor is the released `go/v0.9.0`; `make go-work` (implied by
`test-go`) points it at the sibling checkout instead. CI clones the parser as
a sibling for the same reason: this repo tracks engine work that may
be unreleased.

The generator's end-to-end tests RUN what they generate: the Node
server serves a scripted LSP session over stdio, and the Go module is
`go mod tidy && go build`-compiled and then serves the same session
(skipped when `go` is not installed).

## Untrusted input

Two distinct trust classes (design §10):

- **Grammar data** (L2 specs, L3-compiled output) is never trusted:
  the firewall in `ts/src/loaders.js` (ported from mcp) refuses
  prototype-pollution keys, `ref` bags, non-builtin `@`-refs,
  oversized rule sets, and future schema versions — before any engine
  load, at load time and again at generation time. Workspace file
  paths are sandboxed to their folder.
- **Grammar code** (L1 modules) is trusted like any installed
  dependency — except workspace-supplied modules, refused unless the
  host sets `trustWorkspaceModules` (a manifest must not run npm code
  just by being opened). Failing grammars are quarantined
  (`instances.js`); a bad grammar disables itself, not the server.

Document text is data, never instructions — the engine's own rule,
inherited here.

## Releasing

Releases run through the fleet orchestrator (`admin/publish.sh`, where
`lsp` sits in the `ORDER` array after its `parser` dependency); npm
trusted publishing is maintainer action M8 in the admin notes
(`rollout/setup-npm-trusted-publishing.sh`). Go releases follow the
fleet's `go/vX.Y.Z` tag convention. Three version sites move together
— `ts/package.json`, `const VERSION` in `ts/src/core.js`, and
`const VERSION` in `go/lsp.go` — with drift caught by
`ts/test/version.test.js` and `go/version_test.go`.

**The engine peer stays open, and that is the fix — not a gap in it.**
The peer on `@tabnas/parser` is `">=0"`, the fleet convention
(`admin/publish.sh`: every `@tabnas` peer is open BY DESIGN so installs
resolve the latest published engine), and `ts/test/version.test.js`
pins it there.

This looked like a defect for a while, and the reasoning is worth
recording because it was wrong twice in opposite directions. It was
once `">=0.9.0"` when no such release existed, so the tarball was
uninstallable (ETARGET) — a floor naming a version that does not exist
is strictly worse than no floor. Correcting that to `">=0"` then
attracted the opposite objection: an open range lets npm resolve an
engine PREDATING the LSP contract (recovery, `ruleDone`,
`continuations`; parser#94–#109), so an install would satisfy the peer
and then degrade quietly, completion returning nothing.

That objection described a real symptom but misplaced the cause. The
range was never the problem — the problem was that no PUBLISHED engine
carried the contract. An open range resolves the LATEST published
engine, so the moment `@tabnas/parser@0.9.0` shipped, `">=0"` started
resolving to an engine that satisfies the contract, and the symptom
disappeared without touching this file. Narrowing the peer would now
violate the fleet convention, fail the version test, and fix nothing.

The real invariant: **lsp must not publish ahead of an engine release
carrying the contract.** That is an ordering constraint (`parser`
precedes `lsp` in `ORDER`), not a range constraint.

### Dispatch it; do not push the tag

**Run the workflow with `workflow_dispatch` on `main`, with the `go` input
true.** That is the path the workflow's own header calls normal, and it is
the only one an agent can take: **a session's credentials cannot push tag
refs — `git push origin ts/v…` fails with HTTP 403**, while branch pushes
from the same credentials succeed. It is a ref-type boundary, not a broken
token or a network fault. Nothing is lost by never touching a tag, because
the workflow creates both tags itself, in one atomic push, *after* npm
accepts the publish. Pushing a tag by hand is the orchestrator's path
(`admin/publish.sh`), not yours.

The steps, in order:

1. Bump the version sites named above, together.
2. Verify against the **published** dependencies rather than your checkout.
   The release runner installs fresh from the registry; a working tree
   usually does not, so reproduce that before believing anything:

   ```bash
   (
     cd ts
     # package-lock.json is TRACKED here — regenerate it, do not delete it
     rm -rf node_modules
     npm install
     npm test
   )
   ```

   **Removing the lockfile is not enough on its own.** It does not touch
   `node_modules`, and the sibling symlinks that make local development work
   (`ts/node_modules/@tabnas/…` pointing at a checkout) survive it — the
   suite then passes against unreleased code while appearing to verify the
   published one. Reinstalling is the part that matters.

   There is no `build` script here, and none is needed: this package is
   plain CommonJS and the tests run the sources directly. `npm run build`
   fails with `Missing script: "build"`.

   On the Go side, `GOWORK=off` is necessary and **not sufficient** — it
   disables the workspace and nothing else. A `replace` carrying no version
   on the left applies to every version, so the `require` still resolves to
   the sibling directory. Assert its absence first:

   ```bash
   (
     cd go
     go mod edit -json | grep -q '"Replace": null' || { echo 'go.mod has a replace'; exit 1; }
     GOWORK=off go test -count=1 ./...
   )
   ```

   `-count=1` because shared fixtures live outside the Go module, so a
   changed corpus does not invalidate the test cache.
3. **Merge the bump through a reviewed PR.** That is the house convention —
   `CONTRIBUTING.md` squash-merges PRs and takes the title as the commit
   message — and what `release.yml`'s own header describes. A direct push to
   `main` is a recovery path, not the normal one: CI still gates it, but
   nothing reviews it, and step 5 then publishes that unreviewed commit
   immutably. If you take it, say so.
4. **Wait for `main` CI to go green on the bump commit.** The release
   workflow **has no test step** — it reads `main`, builds against
   already-published dependencies, publishes and tags. `ci.yml` and
   `deps-gate.yml` on the bump commit are the only gates there are. An npm
   version is immutable, and a Go module tag is worse: proxy.golang.org
   caches module versions permanently, so a `go/vX.Y.Z` naming the wrong
   commit cannot be moved, only superseded.
5. **Record the release commit, then dispatch.** The confirmation
   below compares each tag against the commit you released, and a run
   that publishes and then fails to tag can be followed by `main`
   moving — so capture it *before* the dispatch, and read it from the
   remote rather than a local ref that may be stale:

   ```bash
   REL=$(git ls-remote origin refs/heads/main | cut -f1)
   ```

   Then dispatch `release.yml` on `main` with `go: true`.

   Keep that SHA. If a later run has to repair this release, the comparison
   must still be against the commit npm actually served — re-reading `main`
   at repair time gives you whatever it has become, which is exactly the
   value the faulty anchor would also produce, so the check would agree with
   itself and pass. If you no longer have it, recover it from the original
   run: the `head_sha` of that `release.yml` run is the commit it published.
6. Confirm — and make the check **fail**, not merely print:

   ```bash
   V=x.y.z
   npm view @tabnas/lsp@$V version
   GH=$(npm view @tabnas/lsp@$V gitHead)
   [ -n "$GH" ] || { echo "npm records no gitHead for $V"; exit 1; }
   for T in "ts/v$V" "go/v$V"; do
     S=$(git ls-remote origin "refs/tags/$T" | cut -f1)
     [ -n "$S" ] || { echo "missing tag $T"; exit 1; }
     [ "$S" = "$GH" ] || { echo "$T is $S, but npm shipped $GH"; exit 1; }
   done
   [ "$GH" = "$REL" ] || { echo "shipped $GH, not the $REL you cleared"; exit 1; }
   ```

   Counting the refs is not enough either. `grep v$V` exits 0 when *either*
   ref matches; a bare `wc -l` prints the count and exits 0 regardless; and
   even `[ "$n" = 2 ]` passes in the case this section warns about, because an
   anchor fallback writes *both* tags on a commit npm never served — and two
   wrong tags count as two. Comparing each tag against the commit you
   released is what catches that.

   The refs carry the commit directly: `release.yml` creates them with
   `git tag "$T" "$ANCHOR"`, so they are lightweight and there is no `^{}`
   to peel.

   `$REL` is deliberately not what the tags are measured against. It is
   your record of what you meant to release, and a repair can make the
   tags agree with it while npm serves something else: publish from A,
   lose the atomic tag push, re-capture `main` at B, and the repair tags
   B — so a `$REL`-only loop passes while the registry still serves A.
   `gitHead` is npm's own record of the commit the tarball was built from,
   so that is what the tags are checked against, and `$REL` is checked
   separately, as the CI question it actually is.

   When the script exits nonzero, the line that failed says what to do. A
   tag that is not `$GH` is wrong, and the two are not equally
   recoverable. A wrong `ts/v$V` simply moves: npm resolves from the
   registry, so the tag is a signpost and nothing reads it. A wrong
   `go/v$V` does not. `proxy.golang.org` caches a module version's content
   immutably, so once anything has fetched `v$V` that content is what
   consumers get for good, and a corrected tag only makes Git and the
   proxy disagree — and you cannot find out whether it has been fetched
   without causing it, because asking the proxy is itself a fetch. Leave
   that tag where it is and release the next patch from the right commit,
   carrying `retract v$V` in its `go/go.mod`: the cached content stays,
   but `go get` stops selecting the bad version and reports it as
   retracted.

   The last line is a different failure. The tags are honest and `$REL` is
   the stale capture — `main` moved before the run checked out — but what
   shipped is then a commit you never cleared CI on, and `release.yml`
   runs no tests of its own. Confirm `$GH` is green on `main` before
   calling the release good.

   **The dispatch also creates the GitHub Release (admin ADR-19).** Once
   the tags are on the remote, `release.yml` calls
   `.github/workflows/github-release.yml`, which creates a notes-only
   Release on `go/v$V` (on `ts/v$V` where there is no Go module). The release
   is done when that Release is published. If the `github-release` job
   failed after npm and Go had shipped, fix the cause, then dispatch
   `github-release.yml` on `main` with the tag: it creates the Release if it
   is missing and leaves an existing one alone. `release.yml` itself cannot
   do this, because it refuses a re-dispatch once every tag exists.

### When a dispatch dies half-way

The workflow fails closed on a dispatch from any ref but `main`, and when
every tag it would create already exists (the "you forgot to bump" signal).
It fails *open* on an already-published npm version, so a run that published
and then died before tagging can be re-dispatched — **but only while `main`
still points at the release commit.**

That caveat is the sharp edge. The repair logic anchors new tags to an
*existing* tag. If the run published to npm and died before the atomic push,
neither tag exists to supply that anchor — so if `main` has moved on, the
anchor falls back to the new `HEAD` while the publish step skips the version
already on npm. Both tags then land on a commit that is not the one npm
serves, and for the Go module that is permanent. In that state, recover the
original SHA and tag it by hand, or bump to the next patch. Do not just
re-dispatch.

### Never commit the local wiring

Testing against unreleased siblings means symlinked `node_modules`,
`replace` directives and a workspace. None of it may reach a commit, and
`git add -A` is how it does:

- `go mod edit -replace …=/abs/path` — CI reports it as `replacement
  directory /… does not exist`.
- **`go.sum`, after the replace comes out.** A `replace` makes the sibling's
  sums unused, so `go mod tidy` drops them; reverting `go.mod` alone then
  leaves `missing go.sum entry` — a *different* error on the commit meant to
  fix the first one. Revert both, and diff them against the last release
  commit.
- **This repo keeps its workspace deliberately, and inside itself.** The
  Makefile sets `GO_WORK := $(CURDIR)/go/go.work` and `make go-work` creates
  it there, gitignored — so use `make test-go` rather than inventing a
  workspace somewhere else, or manual commands will resolve a different
  module set than CI does. What the workspace does *not* do is validate the
  declared version of a module it replaces with a local one — it still
  consults its members' `go.sum` files and writes missing sums to
  `go.work.sum`. That one gap is the whole reason for the `GOWORK=off` run
  above.
- Scratch files — anything written to measure something.

Stage deliberately (`git add <path>`) and read `git status --short` before
every commit. This bites hardest on a PR whose CI is *expected* red for a
known dependency: a fresh breakage hides inside the expected failure.

### `make publish-ts` is not the release path

It predates `release.yml`. Read what it actually does before using it:

- `publish-ts` runs a local `npm publish`, which goes out over a token and
  bypasses the OIDC trusted publishing the workflow uses.

It stays in the Makefile because removing it is a separate change.

## Pull requests

Open pull requests **ready for review — never as drafts.** This is a
standing maintainer preference, and it overrides any tooling or agent
default that opens pull requests in draft state.

The same rule is stated in `CLAUDE.md`, deliberately and not by
accident: that file is what an agent session loads automatically, this
one is what a human or a non-Claude agent reads. Keep the two in step
rather than deleting either as duplication.

## Agent tooling

An agent working in this repository does not have to drive it by hand. The
org ships two things that already understand these grammars:

- **[`@tabnas/mcp`](https://github.com/tabnas/mcp)** — an MCP server (stdio)
  and the unified `tabnas` CLI: parse, validate and inspect any tabnas
  format, this one included.
- **[`tabnas/skills`](https://github.com/tabnas/skills)** — Agent Skills for
  working on tabnas grammars and plugins.

Prefer them over ad-hoc scripts when exploring a grammar or checking a parse
result.
