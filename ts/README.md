# @tabnas/lsp

The tabnas language server and language-server generator.

One LSP server serves **every** tabnas grammar: fleet plugins,
serialized `GrammarSpec` data, or BNF-dialect text added dynamically
through workspace configuration, and the same machinery **generates**
standalone single-language servers (Node packages, Go binaries or
Rust binaries) plus the editor plugins to use them, from one parser
module or grammar.

```bash
# the unified server, over stdio
npx --package=@tabnas/lsp tabnas-lsp --stdio

# a branded single-language server + editor plugins
npx --package=@tabnas/lsp tabnas-lsp-gen \
  --spec my-grammar.json --language-id mydsl --out mydsl-tools
```

`--package` is required either way: this package ships two binaries and
neither is named `@tabnas/lsp`, so the package-name form of `npx` cannot
pick one, and a bare `npx tabnas-lsp-gen` would fetch an unrelated
registry package.

## What it provides

Diagnostics, semantic tokens, document symbols (outline) and completion,
over incremental `didChange` sync, for any grammar the registry can
reach. Grammars can be added without rebuilding the server: an installed
plugin module, a serialized `GrammarSpec`, or a BNF dialect compiled at
load time, declared per workspace folder and reloaded when the file
changes.

## Generated servers

`tabnas-lsp-gen --runtime node|go|rust` picks the generated server's
runtime: a Node package over this one, a Go module over
`github.com/tabnas/lsp/go`, or a Cargo crate over the Rust port,
`tabnas-lsp`, which the crate takes from this repository by git
dependency because the tabnas crates are not on crates.io. A pure-data
grammar (`--spec`, or BNF text that `--grammar` compiles at generation
time) serves from any of the three; a grammar that is live code needs
its twin in the chosen runtime (`--go-plugin`, `--rust-plugin`). The
full matrix is in the design's section 7.

## Engine

Requires a `@tabnas/parser` providing the LSP engine contract (recovery,
`ruleDone` and `continuations`) which ships from **0.9.0**.

The peer range is deliberately open (`>=0`), the fleet convention, so an
install resolves the newest published engine. That is what satisfies the
contract in practice: install this package and npm gives you 0.9.0 or
later. Pinning an older engine by hand is the one way to get a resolution
that installs cleanly and then cannot serve: completion returns nothing.

## Documentation

Full design, the dynamism ladder, the generator matrix, and the security
model are in [`doc/design.md`](https://github.com/tabnas/lsp/blob/main/doc/design.md).
Contributor and agent guidance is in
[`AGENTS.md`](https://github.com/tabnas/lsp/blob/main/AGENTS.md).

## License

MIT
