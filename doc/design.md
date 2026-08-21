# Design: the tabnas language server and language-server generator

**Status:** adopted design, 2026-08-21. This is the repo's living design
reference. It condenses and extends the adversarially-verified design
exploration and implementation plan in the `admin` repo
(`notes/2026-08-17-unified-lsp-design.md`,
`notes/2026-08-17-unified-lsp-implementation-plan.md`), which remain the
record of how the decisions were reached. Where this document and the
code disagree, fix one of them — this file is maintained.

## 1. What this is

One repository, two complementary products over the same core:

1. **A unified language server** (`tabnas-lsp --stdio`): one process
   serving *every* registered tabnas grammar. LSP is natively a
   multi-language protocol — each `didOpen` carries a `languageId`, and
   the server routes per document — so one server replaces N
   per-language servers in an IDE. Grammars can be **dynamically
   added** at runtime: a plugin module, a serialized `GrammarSpec`, or
   BNF-dialect grammar text, registered through workspace configuration
   without touching this repo.

2. **A language-server generator** (`tabnas-lsp-gen`): given one tabnas
   parser module — or a grammar in data form — it **statically
   generates** a standalone, single-language LSP server, plus the
   editor plugins needed to use it. The server runtime follows the
   parser module's language: a TypeScript plugin yields a Node server;
   a Go plugin (or any pure-data grammar) yields a Go server binary.
   Other runtimes arrive through the pure-`GrammarSpec` lane and the C
   ABI as those mature (§7.4).

The two are the same machinery at different binding times: the unified
server binds grammars at runtime from a registry; the generator binds
one grammar at generation time and emits the thin static wrapper. Both
derive every feature from artifacts grammars already carry — engine
events, structured diagnostics, descriptors — never from per-language
feature code. Language-specific semantics enter only through an
explicit provider interface (Tier 3, §12).

## 2. Prior art — what other parser tools do

Surveyed 2026-08; links in §15. The field splits into four models:

**Grammar-to-LSP generators.** [Langium](https://langium.org/)
(TypeScript, Eclipse/TypeFox — the successor to
[Xtext](https://eclipse.dev/Xtext/)) is the closest precedent: a
grammar declaration generates a typed AST, a full LSP server with
default implementations of completion, symbols, references and
validation, and a scaffolded VS Code extension (via a Yeoman
generator); it runs in Node and the browser. Xtext before it generated
whole Eclipse IDEs and grew LSP output in 2.11. Spoofax and Rascal are
the language-workbench ancestors of the same idea. **What tabnas takes
from this:** generation is the right delivery for "my language, my
server, my extension" (Tier 2), and the generator must scaffold the
editor client, not just the server. **What tabnas does differently:**
Langium generates *one language per project* and the language is
defined in Langium's own grammar DSL; tabnas grammars are live engine
plugins (or portable `GrammarSpec` data), and the *same* grammar also
serves the unified multi-language server without generation.

**Editor-embedded incremental parsers.**
[tree-sitter](https://tree-sitter.github.io/) is not an LSP: editors
(Neovim, Helix, Zed) link the parser in-process and drive highlighting,
folding, indent and outline from declarative queries over the syntax
tree; diagnostics are limited to ERROR nodes, and semantic features
remain per-editor. **Taken:** deriving highlighting/outline from the
parser's own structure (tabnas: the lex trace and `ruleDone` events)
rather than a separate TextMate grammar; error tolerance as a
first-class property. **Rejected:** the per-editor integration matrix —
tabnas standardizes on LSP so one server serves every LSP client, at
the cost of tree-sitter's zero-latency in-process feel.

**Completion cores over live parser state.**
[antlr4-c3](https://github.com/mike-lischke/antlr4-c3) computes
completion candidates by walking ANTLR's ATN — grammar-agnostic,
editor-agnostic, the standard answer to "ANTLR has no official LSP".
[Chevrotain](https://chevrotain.io/) (Langium's engine) ships error
recovery and a `computeContentAssist` API. The tabnas engine's
`continuations()` (parser#100/#101/#107) is the same idea over the rule
stack: position-aware within partially-matched alternates plus a
pop-closure over the stack. ANTLR's IntelliJ plugin (live parse trees,
profiling) is the genre precedent for the grammar-authoring IDE (§13).

**Hand-built tolerant parsers.** Roslyn, the TypeScript compiler, and
tolerant-php-parser write error recovery directly into bespoke parsers.
Tabnas cannot ask every grammar author to do that; recovery lives in
the engine once (panic-mode with grammar-declared sync points,
parser#94–#109) and every plugin — and every generated server — gets it
for free. This is the load-bearing difference between wrapping *a*
parser in an LSP and making a *parser toolkit* LSP-capable.

## 3. Two lanes, one core

```
                      ┌───────────────────────────────┐
   fleet descriptors  │        ts/src (core)          │   go/ (port)
   tabnas.plugin.json │  registry documents instances │   mirrors core
          │           │  core (analyze) loaders       │   by fixture
          ▼           │  generate                     │   parity
   ts/data/registry ─▶└───────┬───────────────┬───────┘
                              │               │
                    runtime binding      generation-time binding
                              │               │
                    ┌─────────▼────────┐ ┌────▼─────────────────────┐
                    │ tabnas-lsp       │ │ tabnas-lsp-gen           │
                    │ unified server   │ │ single-language servers  │
                    │ (all grammars,   │ │ (node pkg | go module)   │
                    │ dynamic add)     │ │ + editor plugins         │
                    └──────────────────┘ └──────────────────────────┘
```

The protocol layer (`ts/src/server.js`) is a thin front-end over a
protocol-free core (`ts/src/core.js`), the mcp one-core/thin-front-ends
discipline. The generator emits wrappers that *call this package* (Node
targets) or the Go port (Go targets); it does not emit copies of the
pipeline, so a fix here reaches every generated server on update.

## 4. The engine contract (shipped)

The server rests on five opt-in, default-off engine extensions, all
merged in `parser` for both runtimes (TS #94–#102, Go #102–#109), all
pinned by shared spec fixtures with the flags off:

| Extension | TS surface | Go surface |
|---|---|---|
| Error recovery (multi-error) | `parse.recover` options; `{value, errors}` result | `Options.Parse.Recover`; `ParseRecover(src) (any, []*TabnasError, error)` |
| Post-process rule event | `sub({ ruleDone })` | `SubRuleDone(fn)` — `RuleDone{State, Alt, Forced}` |
| Reconciled lex trace | documented dedup contract + retraction events | same contract |
| Continuations | `tn.continuations(src)` → `{tins, tokens}` | `Continuations(src) ([]Tin, []string)` |
| Cancellation/budget | `parse.budget` hook | `Options.Parse.Budget` |

Recovery derives sync points from the grammar itself: leading tokens of
close alternates whose `g` tags intersect `syncGroups`, with a
normative structural fallback for untagged grammars. Fleet grammars
carry sync tags since the C1/C4 descriptor wave (12 repos). Force-popped
rules emit synthesized `ruleDone` closes (`forced: true`) so outline
spans survive broken documents.

## 5. The registry — how languages plug in

`ts/data/registry.json` is **generated** from fleet
`tabnas.plugin.json` descriptors (`tools/gen-registry.js`), never
hand-edited; an interim overrides table supplies LSP fields until each
repo's descriptor carries them (each override is deleted as the
descriptor becomes authoritative — the generator names redundant
entries). An entry carries:

- routing: `languageId`, `extensions`, `mediaTypes`
- classification: `pluginKind` (grammar | compiler | modifier),
  `grammarKind` (data | compiled | closure | imperative | external)
- capabilities: `lexStream` (clean | speculative — gates semantic
  tokens), `syncGroups`, `semanticTokens` map, `errorCodes`
- policy: `enabled` (host-side only — a descriptor cannot enable
  itself; the editor-collision policy defaults entrenched incumbents
  `json`, `css`, `markdown`, `yaml`, `c`, `toml`, `xml`, `ini`,
  `proto`, `jsonc` to off and unclaimed ids on)
- loading: `load` — `{module}` | `{spec}` | `{grammar}` (§6)

**Routing rule:** the client's `languageId` wins only when it resolves
to an enabled, non-modifier entry (editors send `plaintext` for unknown
extensions); otherwise most-specific extension match; precedence
workspace > user > bundled; ties surface as diagnostics, never a
silent pick.

## 6. Dynamic lane: how grammars are added at runtime

The dynamism ladder (design §7), as implemented:

| Level | Mechanism | Trust gate |
|---|---|---|
| L0 | bundled fleet registry (build time) | fleet-trusted |
| L1 | `load: {module}` — `require()` a plugin package | workspace modules require explicit opt-in (`trustWorkspaceModules`); bundled/user modules are dependencies you installed |
| L2 | `load: {spec}` — serialized `GrammarSpec` JSON | firewall (§10), always |
| L3 | `load: {grammar}` — `.abnf`/`.ebnf`/`.gbnf` text, compiled per dialect with `builtins: true` | compiles to a pure spec, then the L2 firewall |
| L4 | watch grammar files, recompile, re-analyze open docs | as its level |

Workspace entries arrive through `initializationOptions.languages` or a
`.tabnas/lsp.json` file per workspace folder:

```json
{ "languages": [ {
    "languageId": "mydsl",
    "extensions": [".mydsl"],
    "load": { "grammar": "./grammar/mydsl.abnf" }
} ] }
```

L1 module resolution uses the probe loader: every candidate export
(bare, `.default`, CamelCase, lowercase) is tried on a throwaway child
instance and only the export that actually installs rules is applied to
the real one — export shape alone cannot identify a plugin
(`@tabnas/jsonic` exports its wrapper as the module). Grammar-layer
`base` chains are applied host-side; compiler bases are library deps
and are not applied (`abnf` requires `bnf` internally; applying it
would not make `.abnf` parse).

L3 dispatches on dialect — `@tabnas/abnf`, `@tabnas/ebnf`,
`@tabnas/gbnf` are separate packages and separate dialects (ABNF's
compiler rejects the other two) — and compiles with `builtins: true`
explicitly, because the default conversion is closure-mode and does not
serialize.

Instances are cached per `(languageId, optionsHash, workspaceFolder)`
with exactly **one** permanent mux subscriber installed at creation
(`ctx.sub` aliases the instance's shared subscriber list; per-parse
subscription corrupts the instance), rebuild-on-reload (`tn.grammar()`
prepends — never re-apply a spec to a cached instance), and per-grammar
failure quarantine (a grammar that throws repeatedly is disabled; the
server survives).

## 7. Static lane: the generator

### 7.1 Why generate at all, when the unified server exists

Three audiences the unified server cannot serve:

- **Branded, single-language distribution** (Tier 2): "the `mydsl`
  language server" as its own npm package/binary and marketplace
  extension, with no tabnas fleet visible to its users. Langium/Xtext
  prove this is how language authors actually ship.
- **Go-native deployment**: editors on machines with no Node. A Go
  plugin's grammar should serve from a static Go binary — the
  generator emits the module; `go build` does the rest.
- **Pinning**: a generated server freezes grammar + engine versions;
  the unified server tracks its registry. CI and reproducible-tooling
  contexts want the former.

### 7.2 Inputs and runtime matrix

The generator accepts one grammar, in any of four forms, and emits a
server whose runtime matches where the grammar can execute:

| Input | `--runtime node` | `--runtime go` |
|---|---|---|
| `--entry <languageId>` (fleet registry) | static registration over `@tabnas/lsp` | pure-data entries: embed spec; closure/imperative: `--go-plugin` import |
| `--module <npm plugin>` | package depending on the module | ✗ (a TS module cannot run in Go — pass the Go twin via `--go-plugin`) |
| `--spec <GrammarSpec.json>` | embed + L2 load | **`go:embed` the spec** — the module depends only on the engine |
| `--grammar <file.abnf\|.ebnf\|.gbnf>` | dialect package compiles at server start | pre-compile to a pure spec at generation time, then embed (the C-ABI precedent: pre-compile → L2) |

The pure-`GrammarSpec` row is the important one: a *data* grammar is
runtime-independent, so the "parser module language" really chooses the
**server runtime**, and any grammar that round-trips the L2 lane can be
served from either. The Go strategy is deliberately `go:embed`-first
(design §9.3): linking N plugin modules re-creates the dependency
lockstep ADR-4 exists to avoid, so only closure/imperative grammars —
the two kinds that are live code — link their Go packages.

Spec inputs pass the full L2 firewall (§10) **at generation time** as
well as at load time; a generator that emits a server around a poisoned
grammar would just be a slower way to load it.

### 7.3 What gets emitted

```
out/
  server/            node: package.json + server.js (+ data/grammar.json)
                     go:   go.mod + main.go (+ grammar.json, //go:embed)
  editors/
    vscode/          full extension: package.json contributions,
                     extension.js (vscode-languageclient), language-configuration
    nvim/            vim.lsp.config() registration (Neovim >= 0.11) + lspconfig note
    emacs/           eglot + lsp-mode registration
    sublime/         LSP package client settings
    helix/           languages.toml fragment
    kate/            settings JSON fragment
    zed/             declarative extension scaffold + README (LSP binding in
                     Zed needs the extension's small Rust shim; scaffold only)
  README.md          how to run, wire, and rebuild
```

The emitted Node server is ~20 lines: build one registry entry, call
`startServer` with a fixed registry. The emitted Go server is ~30
lines over `github.com/tabnas/lsp/go`. Generated wrappers contain no
pipeline logic by design — they pin *what* is served, not *how*.

`--unified` generates the multi-language VS Code extension (and editor
fragments) for the whole bundled registry — languages contributed only
for collision-policy-enabled entries — which is how this repo's own
extension is built rather than hand-maintained.

### 7.4 Other parser-module languages (rust, …)

There is no tabnas Rust runtime today
(`parser/doc/rust-port-feasibility.md` is the exploration). The design
holds three doors open without pretending they exist: (a) any pure-data
grammar already serves from either existing runtime — a Rust *user*
can have a Go binary or Node server for their grammar now; (b) the C
ABI widening (`tabnas_parse_ex`, `tabnas_events`, `tabnas_expected`,
plan P3) makes a thin LSP shim writable in any C-capable host; (c) a
future Rust engine port slots in as a third `--runtime` with the same
embed-the-spec shape. The generator refuses unknown runtimes with that
explanation rather than emitting something that cannot work.

## 8. Feature derivations

One debounced parse per change; every artifact from that single pass:

| Feature | Derived from |
|---|---|
| Diagnostics | `errors[]` (recovery) → structured diagnostic → LSP `Diagnostic`; `codeDescription.href` → tabnas.dev error registry; `len` is code points and is converted through the document text (§9) |
| Completion | `continuations()` — sentinels (`#ZZ`, `#AA`, `#BD`) filtered; fixed-token source as label |
| Semantic tokens | reconciled lex trace (newest-per-position + span shadowing) → CANON default map + prefix conventions (`KW_*`→keyword, …) + per-entry overrides; fixed superset legend so hot-adds never re-register; served only for `lexStream: clean` entries |
| Outline | `ruleDone` events (incl. `forced` closes) → rule-name filter → span-nested `DocumentSymbol`s |
| Hover | token under cursor + descriptions (tracked; degrade to nothing) |
| Cross-file | multisource `documentLink` (workspace-sandboxed, off by default; tracked) |

`c` and `markdown` are `grammarKind: external` — the engine event
stream sees almost nothing for them, so their symbols/outline arrive
through Tier-3 providers (§12), not by pretending the generic pipeline
covers them.

## 9. Documents and position encoding

All encoding knowledge lives in the document store and only there.
Engine diagnostics carry `row`/`col`/`pos` in UTF-16 units (TS) or
runes (Go), but `len` in Unicode **code points** of the token source —
a third unit. Both servers convert through the actual document text
before building a `Range`; the Go server additionally converts
rune-based columns to the client's negotiated encoding (UTF-16 by
default). Astral-plane fixtures pin the conversions in both runtimes.

## 10. Security

Grammar **data** is untrusted; grammar **code** is trusted like any
dependency you installed, and quarantined on failure. The L2 firewall
(ported from mcp, which validates agent-authored grammars with the same
rules) runs before any engine load:

1. prototype-pollution scan — `__proto__`/`constructor`/`prototype`
   anywhere in the tree (via `getOwnPropertyNames`, descriptors read);
2. `ref` refusal — live functions are not JSON; serialized grammars
   name `$`-suffixed engine builtins only;
3. `@`-ref scan over options and alt function positions (`b p r a e h
   c`), passing `@@…`, `@SKIP`, serialized regexes, and builtins;
4. rule-count cap;
5. schema-`v` gate against the engine's `BUILTIN_SCHEMA_VERSION`;
6. composition-aware trial load against the entry's declared stack.

Beyond the firewall, and shipped today: total-complexity bounds on
grammar data (rule count, total alternates, nesting depth, file bytes);
workspace manifests are trust-gated (they can claim `.json` to shadow a
trusted language — default-deny for module loads, extension claims
surfaced); every parse/provider call is wrapped; per-grammar quarantine
keeps one bad grammar from taking the server down; multisource
resolution is workspace-sandboxed and off by default.

**Not yet shipped, and load-bearing for the ReDoS story** (§14 tracks
both): parse budgets/cancellation and document-size caps. Serialized
regexes are legal grammar data and pass the `ref` scan by design, so
until the engine's `parse.budget` hook is wired here, a hostile
workspace grammar's regex is bounded only by the caps above — which
bound how much grammar is LOADED, not how long a parse may run.
Generated servers
inherit all of this by construction, because they wrap the same core.

## 11. Protocol decisions

Incremental sync; version-stamped **push** diagnostics (pull
scheduled); stale structural results suppressed rather than served
against newer content (cached spans are not yet edit-transformed);
debounce 150ms; dynamic registration reserved for hot-added grammars;
custom requests namespaced `tabnas/*` under `experimental`
(`tabnas/status` now; `syntaxTree`/`parseTrace`/`grammarModel`/
`railroad` tracked — precedents: `rust-analyzer/syntaxTree`, clangd's
`textDocument/ast`, ANTLR's IntelliJ live trees). In-flight client
cancellation requires the parse off the protocol thread (worker +
`SharedArrayBuffer` abort flag read by the engine's budget hook);
until then the hook enforces deadlines only.

## 12. Tiers for language authors

- **Tier 1 — grammar only, zero LSP code**: a `.abnf`/spec file + a
  small manifest → diagnostics, completion, semantic tokens, outline.
  Synthetic BNF rules (`<prod>$stepN`) are canonicalized via the
  spec's `meta.provenance` map before they reach hover/outline.
- **Tier 2 — a branded server**: `tabnas-lsp-gen` output (§7), or a
  hand-written 10-line static registration over this package.
- **Tier 3 — language semantics**: an explicit provider interface
  (symbols enrichment, definitions, references, formatting), proved
  fleet-internally on `markdown` (own AST) and `c` (span-carrying CST)
  before any third party needs it. Tracked.

## 13. Cross-runtime parity

TS is canonical; Go mirrors by **fixtures, not code sharing**:

- `test/fixtures/json-grammar.json` — a pure-data strict-JSON
  `GrammarSpec` (copied from the engine's builder fixture; a drift test
  asserts byte-equality whenever the sibling checkout is present). It
  is both the shared test grammar and the standing proof of the L2
  lane both servers depend on.
- `test/fixtures/lsp-conformance.json` — document → expected
  diagnostics codes / outline / completion cases, executed by
  `ts/test/conformance.test.js` and `go/conformance_test.go`.
- `ts/data/diagnostic-fixtures.json` — 262 cases generated from the
  fleet's `test/spec` TSV corpus (`ERROR:<code>` rows across 27
  grammars); the fleet-wide diagnostics gate for checkouts with the
  grammars installed.

## 14. Status and roadmap

Shipped in this repo: the unified server (registry, routing, documents,
instances, diagnostics, semantic tokens, outline, completion,
`tabnas/status`), the L1/L2/L3 loaders with the firewall, the Go core
(`go/`) with the same pipeline over `ParseRecover`/`SubRuleDone`/
`Continuations`, the generator with Node and Go targets and the editor
plugin matrix, and the conformance fixtures.

Tracked next, in rough value order: **parse budgets and document-size
caps** (the §10 gap — the engine's `parse.budget` hook exists and is
simply not wired here yet, and it is what bounds a hostile grammar's
serialized regex); hover token descriptions; worker
isolation + in-flight cancellation; edit-transformation of cached
spans; browser build (`vscode-languageserver/browser` — the web
playground already runs the engine client-side); marketplace packaging
of the `--unified` VS Code extension; Tier-3 providers (markdown, c);
BNF compile diagnostics with source spans (needs bnf IR spans,
plan C5); the grammar-authoring IDE (fixture code-lens, railroad
webview, embed-drift detection); pull diagnostics; DAP is explicitly
deferred (the debug plugin's trace serves grammar authors at
request/response cost).

## 15. Sources

Prior-art review (§2) consulted, 2026-08:

- Langium — <https://langium.org/> and
  <https://github.com/eclipse-langium/langium>
- Xtext — <https://eclipse.dev/Xtext/>
- antlr4-c3 — <https://github.com/mike-lischke/antlr4-c3>; Strumenta's
  walkthrough <https://tomassetti.me/code-completion-with-antlr4-c3/>
- Chevrotain — <https://chevrotain.io/>
- tree-sitter — <https://tree-sitter.github.io/>; editor integrations:
  Zed <https://zed.dev/docs/extensions/languages>, Helix
  <https://helix-editor.com/>, nvim-treesitter
- tolerant-php-parser —
  <https://github.com/microsoft/tolerant-php-parser> (recovery design
  survey in its docs)
- Fleet-internal: `parser/ts/doc/lsp-feasibility.md` (error-recovery
  design), `admin/notes/2026-08-17-unified-lsp-*.md` (verified design
  exploration, implementation plan, maintainer actions)
