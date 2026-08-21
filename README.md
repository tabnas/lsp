# tabnas/lsp

The tabnas language server and language-server generator.

One LSP server serves **every** tabnas grammar — fleet plugins,
serialized `GrammarSpec` data, or BNF-dialect text added dynamically
through workspace configuration — and the same machinery **generates**
standalone single-language servers (Node packages or Go binaries) plus
the editor plugins to use them, from one parser module or grammar.

```
npx @tabnas/lsp tabnas-lsp --stdio        # the unified server
npx tabnas-lsp-gen --spec my-grammar.json --language-id mydsl \
  --out mydsl-tools                        # a branded server + editor plugins
```

Diagnostics (multi-error, via engine recovery), completion (engine
continuations), semantic tokens (reconciled lex trace), and outline
(post-process rule events) are derived from the grammar itself — no
per-language feature code.

| Where | What |
|---|---|
| [`doc/design.md`](doc/design.md) | the design: architecture, prior art, the dynamic/static lanes, security, roadmap |
| [`AGENTS.md`](AGENTS.md) | working in this repo: layout, contracts, commands |
| [`ts/`](ts/) | canonical TypeScript package `@tabnas/lsp` (server, loaders, generator) |
| [`go/`](go/) | Go port `github.com/tabnas/lsp/go` (pipeline + stdio server library) |
| [`editors/`](editors/) | generated multi-language editor plugins for the unified server |
| [`test/fixtures/`](test/fixtures/) | cross-runtime conformance fixtures (the TS↔Go parity contract) |

Part of the [tabnas](https://tabnas.dev) project. MIT licensed.
