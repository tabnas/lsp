# Agents Guide — lsp

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
the workflow creates both tags itself — in one atomic push, *after* npm
accepts the publish. Pushing a tag by hand is the orchestrator's path
(`admin/publish.sh`), not yours.

The steps, in order:

1. Bump the three version sites named above, together.
2. Verify, building first:

   ```bash
   (cd ts && npm run build && npm test)
   (cd go && GOWORK=off go test ./...)   # only sound with no `replace` — see below
   ```

   **Build first.** `npm test` runs the compiled output and does **not**
   compile, so a bumped source file is otherwise checked as stale `dist/` —
   or not at all, on a fresh checkout.
3. **Merge the bump through a reviewed PR.** That is the house convention —
   `CONTRIBUTING.md` squash-merges PRs and takes the title as the commit
   message — and what `release.yml`'s own header describes. A direct push to
   `main` is a recovery path, not the normal one: CI still gates it, but
   nothing reviews it, and step 5 then publishes that unreviewed commit
   immutably. If you take it, say so.
4. **Wait for `main` CI to go green on the bump commit.** The release
   workflow **has no test step** — it reads `main`, builds against
   already-published dependencies, publishes and tags. `ci.yml` on the bump
   PR is the only gate there is. An npm version is immutable, and a Go
   module tag is worse: proxy.golang.org caches module versions permanently,
   so a `go/vX.Y.Z` naming the wrong commit cannot be moved, only
   superseded.
5. Dispatch `release.yml` on `main` with `go: true`.
6. Confirm `npm view @tabnas/lsp@$V version`, and **query both tags
   exactly**:

   ```bash
   V=x.y.z
   git ls-remote --tags origin "refs/tags/ts/v$V" "refs/tags/go/v$V" | wc -l   # want 2
   ```

   `git ls-remote --tags origin | grep v$V` is not a check. `grep` exits 0
   if *either* ref matches, so it reports success in precisely the
   half-finished state — npm published and `ts/v` written, `go/v` not — that
   the workflow is built to let you repair by re-dispatching.

The workflow fails closed on a dispatch from any ref but `main`, and when
every tag it would create already exists (the "you forgot to bump" signal).
It fails *open* on an already-published npm version, so a run that published
and then died before tagging is repairable by re-dispatching rather than
stuck.

### Verifying against the published module, not your checkout

`GOWORK=off` is necessary and **not sufficient**. It disables the workspace
and nothing else — it does *not* neutralise a `replace` in `go.mod`, because
a replacement with no version on the left applies to every version. The
`require` then still resolves to the sibling directory, and the suite goes
green against the very checkout you were trying to stop using:

```
$ GOWORK=off go list -m github.com/tabnas/parser/go
github.com/tabnas/parser/go v0.9.6 => /…/parser/go
```

Assert the absence first, and only then believe the run:

```bash
cd go
go mod edit -json | grep -q '"Replace": null' || { echo 'go.mod still has a replace'; exit 1; }
GOWORK=off go test ./...
```

The TypeScript equivalent is `ts/package-lock.json`: it is gitignored, it
pins the previous versions, and `npm install` after a dependency bump will
happily keep them — the suite then passes against the packages you were
replacing. Delete it before verifying. Both of these produce a green local
run against the wrong version, which is the only kind of green worth
distrusting.

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
- **A `go.work` belongs outside every repo**, one level up, `use`ing each
  module, so no repo can track it. It also never consults `go.sum`, so it
  cannot tell you whether a *declared* version is sound.
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
