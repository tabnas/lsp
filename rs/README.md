# tabnas-lsp (Rust)

The tabnas language server in Rust: crate `tabnas_lsp`, over the
[`tabnas`](https://github.com/tabnas/parser) parsing engine, as a
library for Rust hosts and as the `tabnas-lsp` stdio server.

This is the Rust port of the whole pipeline. The canonical TypeScript
implementation is [`../ts`](../ts) and the Go port is [`../go`](../go);
this crate tracks them by fixture parity, never by code sharing, and
TypeScript is authoritative: when this crate disagrees with the
fixtures, this crate changes. It exists so that a Rust host, the `aless`
terminal viewer first, can use the server's analysis as a dependency
(semantic tokens for any grammar's text, multi-error diagnostics), and
so that `tabnas-lsp --stdio` runs where Node does not.

## Architecture

One module per part of the canonical pipeline (design
[`doc/design.md`](../doc/design.md) §3, §8, §9, §11), the shared types
in one place, and a thin binary. Every part is complete:

| Module | Mirrors | Contract | Status |
|---|---|---|---|
| `types` | the shared definitions across `ts/src/*.js` | `Position`, `Range`, `PositionEncoding`; `Doc`; `Diagnostic`; `Entry` with `Load`, `Scope`, `EntrySource`; `RuleEvent`, `Collected`; `MakeInstance`; `Config`; `LoadError`, `Issue` | complete |
| `documents` | `ts/src/documents.js`, `go/documents.go` | the line index; byte offsets, engine rows and columns to and from wire positions in the negotiated encoding; a diagnostic's range; incremental changes; `DocumentStore` | complete: UTF-16 and UTF-8 wire units, incremental changes, the store's negotiated encoding; tested on every encoding edge and swept against the TypeScript store |
| `instances` | `ts/src/instances.js`, `go/core.go` | one instance per cache key; the ONE permanent mux subscriber pair; serialized parses with an active collector; quarantine after 3 failures; invalidation on reload | complete: parses serialized across threads by a re-entrant gate, a panicking `MakeInstance` counted toward quarantine; tested against the fixture grammar |
| `trace` | `ts/src/core.js` `reconcile` | the `subscribe_lex` collector and the reconciliation contract | complete, fixture-tested |
| `semantic` | `ts/src/core.js` `tokenType`, `semanticTokens` | the CANON map, prefix conventions, fixed legend, LSP tokens, delta encoding | complete, fixture-tested |
| `analyze` | `ts/src/core.js` `analyze`, `diagnostics` | one parse per change: diagnostics through recovery, semantic tokens (in the document's encoding), outline, the reconciled trace | complete: fixture-tested, and equal to the TypeScript pipeline on every document of the random sweep (below) wherever the two engines agree on the parse, and on every document when both are fed the TypeScript engine's events |
| `outline` | `ts/src/core.js` `outline`, `go/outline.go` | rule events to nested `DocumentSymbol`s by span containment, at the engine's columns | complete: the `outlines` fixture section pins whole symbol trees, ranges included; nesting bounded at `MAX_OUTLINE_DEPTH` (256) so no document can exhaust a stack, a bound TypeScript does not have |
| `hover` | `ts/src/server.js` `onHover` | the token under the cursor (`token_at`, `token_range`) and its description (TypeScript answers `null` today; parity means `None` until it ships) | complete |
| `completion` | `ts/src/core.js` `completion`, `go/completion.go` | continuations of the text before the cursor as items, sentinels filtered, fixed source as label, under the parse lock | complete: fixture-tested; the TypeScript items, in order, pinned at the start, middle and end of a document, inside tokens and in both encodings; equal to the TypeScript core at every random cursor of the sweep except where the prefix ends inside an unterminated string or block comment, an engine divergence registered in `tests/completion_test.rs` |
| `registry` | `ts/src/registry.js`, `go/registry.go` | the embedded `ts/data/registry.json`; the tiers (`Router`); routing by language id and extension, folder-scoped workspace entries, ties surfaced | complete: routing case for case with the TypeScript suites; the collision policy read from the generated file, never copied; media types a host lookup (`resolve_media_type`, and last in `resolve_with_media_type`), never the server's routing, as in TypeScript; hot reload's entry points (`set_workspace`, `reloaded_by`) |
| `loaders` | `ts/src/loaders.js` | L1 linked grammars, L2 specs, L3 dialect text; the grammar firewall and its caps; the sandbox; `Loader` as the binary's `MakeInstance` | complete: the firewall rule for rule with the TypeScript limits and messages, one test per refusal, its builtin set asked of the engine; the sandbox on real paths; L3 lowers all three dialects to pure data, where TypeScript refuses every `.ebnf` and `.gbnf` file (a TypeScript defect, reported) |
| `jsonrpc` | `go/jsonrpc.go` | Content-Length framing; `Message`; a `Connection` with a reader thread, a locked writer and `recv_timeout` for the debounce | complete; malformed input is answered (`ParseError`, `InvalidRequest`) or logged, never fatal |
| `server` | `ts/src/server.js`, `go/server.go` | capabilities; incremental sync; 150 ms debounce; version-stamped push diagnostics; stale results suppressed; `tabnas/status`; workspace grammars and hot reload; negotiated encoding, document size cap and parse deadline | complete; `tests/server_test.rs` drives the built binary through a scripted session |
| `highlight` | (Rust hosts only) | `highlight()` and `Highlighter`: parse and return byte spans to colour | complete, fixture-tested |
| `src/bin/tabnas-lsp.rs` | `ts/bin/tabnas-lsp.js` | `--stdio`, `--version`, `--help`; builds `Config` from the bundled registry and the loader | complete; exits 0 after `shutdown`, 1 otherwise, as `vscode-languageserver` does |

### Features

- `dialects`: L3 grammar files. Links `tabnas-abnf`, `tabnas-ebnf` and
  `tabnas-gbnf` (three crates, three dialects, dispatched by extension).
- `fleet`: the bundled grammars linked into the binary and registered
  by package name (`loaders::fleet`): csv, feed, ini, json, json5,
  jsonc, jsonic, jsonl, toml, xml, yaml, zon. The library never needs
  it: a host supplies its own parsers through `MakeInstance`.

Both are off by default. Every optional crate is a sibling checkout and
cargo reads each manifest to resolve, feature on or off, so all of them
(and the siblings they name: bnf, hoover) must be present to build;
`ci/rust/run.sh` checks the list first.

### The engine contract used

`parse_recover` (multi-error diagnostics), `subscribe_rule_done`
(outline), `subscribe_lex` (semantic tokens), `continuations`
(completion), `parse_budget` (the deadline), `GrammarSpec::from_value`
and `Tabnas::grammar` (L2), all opt-in in the engine and all on the
engine's own thread of control: the server runs one message at a time
and never parses off it.

## Use

Build a parser for the grammar, then hand it to `highlight`, which
installs the recorder, parses with whatever recovery the parser's
options enable, and returns the byte spans to colour. The parser is
consumed: one instance, one parse.

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
    let result = highlight(parser, text, None);
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
stays on the parser, and every parse fills every recorder ever installed
on it; that is why `highlight` takes the instance by value rather than
letting a second call stack a second recorder. To parse repeatedly with
one instance, keep one `Highlighter`, which installs the recorder once
and drains it between parses:

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

The LSP form is beside the spans: `result.tokens` holds one
`SemanticToken` per span and `tabnas_lsp::encode(&result.tokens)` is the
`data` array a server would send.

The full pipeline is the shape the Go port offers: an `Entry` for the
language, an `Instances` over a `MakeInstance` that returns a parser
with the grammar installed and recovery enabled, then
`analyze(&instances, &inst, &entry, &doc)` for diagnostics (every
recovered error), semantic tokens and outline from one parse,
`completion` for the items at a position, and `hover` for the token
under the cursor:

```rust
use std::sync::Arc;

use tabnas::{Options, Tabnas};
use tabnas_lsp::{analyze, completion, Doc, Entry, Instances, LoadError, MakeInstance, Position};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../test/fixtures/json-grammar.json"
    ))?;
    // The host decides what a language is: here, the shared grammar
    // with recovery on, so one parse reports every error.
    let make: MakeInstance = Arc::new(move |_entry: &Entry| {
        let mut options = Options::default();
        options.parse.recover.enabled = true;
        let mut parser = Tabnas::with_options(options);
        parser
            .grammar_json(&spec)
            .map_err(|e| LoadError::new(e.to_string()))?;
        Ok(parser)
    });
    let mut instances = Instances::new(make);
    let mut entry = Entry::new("jsonf");
    entry.language_id = Some("jsonf".into());
    let inst = instances.get(&entry, None)?.expect("not quarantined");

    let text = "{\"a\":true blah,\"b\":[1,2] blah}";
    let doc = Doc::new("file:///t.jsonf", "jsonf", 1, text);
    let analysis = analyze(&instances, &inst, &entry, &doc);
    let codes: Vec<&str> = analysis
        .diagnostics
        .iter()
        .filter_map(|d| d.code.as_deref())
        .collect();
    assert_eq!(codes, ["unexpected", "unexpected"]);
    assert_eq!(analysis.outline[0].name, "Object");
    assert_eq!(analysis.outline[0].children[0].name, "Array");

    let after_key = Doc::new("file:///t.jsonf", "jsonf", 2, "{\"a\"");
    let items = completion(Some(&instances), &inst, &entry, &after_key, Position::new(0, 4));
    assert_eq!(items[0].label, ":");
    Ok(())
}
```

Positions are UTF-16 code units, the protocol default; a document built
`with_encoding(PositionEncoding::Utf8)` is measured in bytes instead. A
host that wants the server rather than the library builds a `Config`
and calls `server::serve`.

## Parity

`test/fixtures/lsp-conformance.json` is the contract, executed by
`ts/test/conformance.test.js`, `go/conformance_test.go` and
`rs/tests/conformance_test.rs`, one runner per section: `analyze`
(diagnostic codes in order, the first diagnostic's range, the outline's
name tree), `completions` (sorted labels at a position), `outlines`
(whole symbol trees, ranges included) and `semantic` (error count,
decoded tokens, delta-encoded data). All three runtimes pass every
section. Values come from the TypeScript pipeline; when this crate
disagrees, this crate changes, and a TypeScript defect is reported, not
papered over.

The fixtures pin chosen cases; `tests/parity_sweep.rs` finds the ones
nobody chose. It is ignored in the ordinary suite because it needs node
and the ts/ package installed (`cd ts && npm install`):

```bash
cd rs
cargo test --test parity_sweep -- --ignored --nocapture
```

It generates seeded random documents (valid, invalid, multi-error,
multi-byte, multi-line, with comments; under a plain entry, one with
outline and token overrides, and a fail-fast one; three completion
positions each), answers them with `ts/src/core.js` and with this crate,
and compares diagnostics (every field), outlines (whole trees), token
data, `failed` and completion items. Each side also reports its
ENGINE's raw view of the parse in code points, so a difference is
classified: the port's when the engines agree, the engine's when they
do not. And this crate's pipeline functions are fed the TypeScript
engine's own events, which must give the TypeScript results whatever
the engines do. Seeds 1 to 4, 300 documents and 900 positions each:
no port mismatch, and the pipeline over the TypeScript events agrees
on every document.

What remains are ENGINE divergences (TypeScript and Rust engines at the
same release, the shared grammar), which belong to the parser repository
and are not papered over here. Each reproduces with the engine alone:

| Input | TypeScript engine | Rust engine |
|---|---|---|
| a completion prefix ending inside an unterminated string or block comment: `{"`, `[/*` | the start rule's openers `#NR #ST #VL #OB #OS` | the enclosing container's continuations: `#NR #ST #VL #CB`, or `... #OB #OS #CS` in a list |
| `"\n1]` (an `unprintable` character in a string) | the parse ends there: one error | parsing resumes after it: value `1`, a second error (`unexpected ]`) |
| `/*cccccccccccccccc}` (an unterminated block comment) | the bad token stops at 18 code points and `}` lexes on | the bad token runs to the end of the source (`len` 19) |
| `"\n`, `/*\n` | the message's source excerpt indents each continuation line by two spaces (`unprintable character: \n` and two spaces) | no indent |
| `{"":}` | recovered value `{}` | `{"": null}` |
| ` [] [` (trailing content) | the root rule's close `ruleDone` fires twice | once |
| `/*[]` with recovery off | the bad token reaches `lex` subscribers, and the open rule's `ruleDone` fires | neither |

Only the first four change what a client sees (completion items;
diagnostics, outline and token data; ranges and tokens; messages).
TypeScript is canonical, so each is a Rust engine bug unless the parser
maintainer rules the TypeScript behaviour the defect.

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
`tabnas` at your own checkout or git reference. The library needs no
feature; a host never links the fleet.

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
(`rust-version` in `Cargo.toml`), checks that every sibling the manifest
names is present, checks the lock, and is what CI runs. The random
sweep against the TypeScript core (see Parity) is run by hand:
`cargo test --test parity_sweep -- --ignored --nocapture`.
