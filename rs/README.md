# tabnas-lsp (Rust)

Semantic tokens for tabnas grammars, derived from the engine's lex trace
with no per-language code: crate `tabnas_lsp`, over the
[`tabnas`](https://github.com/tabnas/parser) parsing engine.

This is the Rust port of ONE half of the tabnas language server pipeline.
The canonical TypeScript implementation is [`../ts`](../ts)
(`src/core.js`: `reconcile`, `tokenType`, `semanticTokens`) and the Go
port is [`../go`](../go) (`semantic.go`); this crate tracks them by
fixture parity, and TypeScript is authoritative. It exists so that a
Rust host, the `aless` terminal viewer first, can colour any tabnas
grammar's text the way the language server does. It is not a language
server: diagnostics, outline and completion stay in the TypeScript and
Go packages.

The pipeline is the design's (`doc/design.md` §4, §5, §8, §11):

1. **Trace.** A `LexTrace` recorder, installed with the engine's
   `subscribe_lex`, keeps every token event the lexer announces:
   ignored trivia and retractions included.
2. **Reconcile.** `reconcile` applies the documented lex-trace contract:
   newest event per source position wins, and a kept token's byte span
   shadows any older event starting inside it.
3. **Map.** `token_type` resolves each token name through the entry's
   `semanticTokens` overrides, then the CANON defaults (`#ST` string,
   `#NR` number, `#CM` comment, `#VL` keyword, the brackets and
   separators operator), then the prefix conventions (`KW_` keyword,
   `LIT_` string, `TRIVIA_` comment, `PP_` macro, `PUNC_` operator, `ID`
   and `#ID` variable), onto the fixed nine-entry `LEGEND`.
4. **Position.** `semantic_tokens` converts engine rows and columns to
   LSP units (0-based lines, UTF-16 columns), keeps the same column and
   length in Unicode scalar values beside them, and splits a token that
   spans lines into one token per line. `encode` is the LSP
   delta-encoded `data` array.

The registry (`Registry`, the generated `ts/data/registry.json` embedded
at build time) says which grammars serve semantic tokens at all
(`lexStream: clean`) and carries each entry's overrides.

## Use

Build a parser for the grammar, then hand it to `highlight`, which
installs the recorder, parses with whatever recovery the parser's
options enable, and returns the byte spans to colour:

```rust
use tabnas::{Options, Tabnas};
use tabnas_lsp::{highlight, TokenType};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The shared conformance grammar: strict JSON as a pure-data spec.
    let spec = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test/fixtures/json-grammar.json"
    ))?;
    let mut options = Options::default();
    options.parse.recover.enabled = true;
    let mut parser = Tabnas::with_options(options);
    parser.grammar_json(&spec)?;

    let text = r#"{"a":[1,2],"b":true}"#;
    let result = highlight(&mut parser, text, None);
    assert!(result.errors.is_empty());
    assert!(!result.partial);

    let coloured: Vec<(&str, TokenType)> = result
        .spans
        .iter()
        .map(|span| (&text[span.start..span.end], span.kind))
        .collect();
    assert_eq!(coloured[0], ("{", TokenType::Operator));
    assert_eq!(coloured[1], ("\"a\"", TokenType::String));
    assert_eq!(coloured[4], ("1", TokenType::Number));
    assert_eq!(coloured[11], ("true", TokenType::Keyword));
    Ok(())
}
```

Errors do not stop highlighting. With recovery on, the parse continues
past each error and every token the lexer produced is coloured; with it
off, the spans cover the text up to the failure and `partial` is true.
`errors` carries the engine's errors either way.

The engine has no unsubscribe API, so the recorder `highlight` installs
stays on the parser. Use a fresh instance per parse, as `aless` does, or
keep one `Highlighter`, which installs the recorder once and drains it
between parses:

```rust
use tabnas::Tabnas;
use tabnas_lsp::{Highlighter, Registry};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test/fixtures/json-grammar.json"
    ))?;
    let mut parser = Tabnas::new();
    parser.grammar_json(&spec)?;

    // The registry says whether a grammar's lex stream is clean, and
    // carries the entry's token-name overrides.
    let registry = Registry::bundled();
    assert!(registry.clean("json"));
    let overrides = registry.overrides("json").cloned();

    let mut highlighter = Highlighter::with_overrides(parser, overrides);
    let first = highlighter.highlight("[1,2]");
    let second = highlighter.highlight("{\"k\":\"v\"}");
    assert_eq!(first.spans.len(), 5);
    assert_eq!(second.spans.len(), 5);
    Ok(())
}
```

The LSP form is beside the spans. `result.tokens` holds one
`SemanticToken` per span, with `row`, `col` and `len` in the wire units
and `col_chars` and `len_chars` in scalar values, and
`tabnas_lsp::encode(&result.tokens)` is the `data` array a server would
send.

## Parity

`test/fixtures/lsp-conformance.json` carries a `semantic` section:
documents, the parse's error count, the expected tokens as
`[line, character, length, type]` rows and the delta-encoded `data`.
`ts/test/conformance.test.js`, `go/conformance_test.go` and
`rs/tests/conformance_test.rs` all execute it. The values come from the
TypeScript pipeline; when this crate disagrees, this crate changes.

## Install

The `tabnas` crate is not published to a registry, so the engine is
consumed as a **sibling checkout**, the standard tabnas development
model. Clone `https://github.com/tabnas/parser` next to this repository
and point at both:

```toml
[dependencies]
tabnas-lsp = { path = "../lsp/rs" }
tabnas = { path = "../parser/rs" }
```

Both entries are needed. A crate's dependencies are not passed on to its
dependents, so `tabnas-lsp` alone does not put `tabnas` in your extern
prelude, and the examples above that name `tabnas::Tabnas` would not
resolve.

A git dependency works the same way: `tabnas-lsp = { git =
"https://github.com/tabnas/lsp" }` with a `[patch]` entry redirecting
`tabnas` at your own checkout or git reference.

## Test

```bash
cd rs
cargo fmt --check
cargo build --all-targets
cargo test --all-targets
cargo test --doc      # --all-targets does not include doctests
cargo clippy --all-targets --all-features -- -D warnings
```

`ci/rust/run.sh` runs the same commands through the MSRV toolchain
(`rust-version` in `Cargo.toml`), checks the lock, and is what CI runs.
