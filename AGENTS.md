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
floor is a pseudo-version of parser main; `make go-work` (implied by
`test-go`) points it at the sibling checkout. CI clones the parser as
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

Not yet wired: npm trusted publishing for `@tabnas/lsp` is maintainer
action M8 in the admin notes (`rollout/setup-npm-trusted-publishing.sh`
once the repo exists). Go releases follow the fleet's `go/vX.Y.Z` tag
convention. The `VERSION` const in `go/lsp.go` and `ts/package.json`
version move together.
