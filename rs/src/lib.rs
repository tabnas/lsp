// Copyright (c) 2026 Richard Rodger, MIT License

//! The tabnas language server in Rust: every feature the unified server
//! derives from the engine contract, over the Rust engine, as a library
//! for Rust hosts and a stdio server.
//!
//! This crate is the Rust port of the pipeline in `ts/src/` (canonical)
//! and its Go port in `go/`; it mirrors them by fixture parity
//! (`test/fixtures/lsp-conformance.json`), never by code sharing, and
//! TypeScript is authoritative: a Rust mismatch is a port defect. It
//! exists so that Rust hosts, the `aless` terminal viewer first, can use
//! the server's analysis directly as a dependency, and so that
//! `tabnas-lsp --stdio` runs where Node does not.
//!
//! The module map, one module per part of the canonical pipeline:
//!
//! | Module | Mirrors | Does |
//! |---|---|---|
//! | [`types`] | the shared definitions of `ts/src/*.js` | positions and encodings, [`Doc`], [`Diagnostic`], [`Entry`], the collector types, [`MakeInstance`], [`Config`], [`LoadError`] |
//! | [`documents`] | `ts/src/documents.js`, `go/documents.go` | the line index and every position conversion (UTF-16 or UTF-8 wire units from the engine's rows, columns and byte offsets); the store of open documents |
//! | [`instances`] | `ts/src/instances.js`, `go/core.go` | one instance per entry, the one permanent mux subscriber pair, serialized parses, quarantine after three failures, invalidation on reload |
//! | [`trace`] | `ts/src/core.js` `reconcile`, `anchor` | the lex-trace collector and the documented reconciliation contract, run against the source text ([`reconcile_in`]) |
//! | [`semantic`] | `ts/src/core.js` `tokenType`, `semanticTokens` | the CANON map, prefix conventions, the fixed legend, LSP tokens and their delta encoding |
//! | [`mod@analyze`] | `ts/src/core.js` `analyze`, `diagnostics` | one parse per change: diagnostics through recovery, semantic tokens, outline, hover data |
//! | [`mod@outline`] | `ts/src/core.js` `outline`, `go/outline.go` | rule events to nested `DocumentSymbol`s |
//! | [`mod@hover`] | `ts/src/server.js` `onHover` | the token under the cursor |
//! | [`mod@completion`] | `ts/src/core.js` `completion`, `go/completion.go` | the engine's continuations as completion items |
//! | [`registry`] | `ts/src/registry.js`, `go/registry.go` | the embedded `ts/data/registry.json`, the configuration tiers and document routing |
//! | [`loaders`] | `ts/src/loaders.js` | the L1, L2 and L3 lanes and the grammar firewall |
//! | [`jsonrpc`] | `go/jsonrpc.go` | JSON-RPC 2.0 framing over stdio, a reader thread and a writer |
//! | [`server`] | `ts/src/server.js`, `go/server.go` | the protocol front-end: capabilities, incremental sync, debounced version-stamped diagnostics, `tabnas/status`, workspace grammars and hot reload |
//! | [`mod@highlight`] | (Rust hosts only) | [`highlight()`] and [`Highlighter`]: parse and return byte spans to colour |
//!
//! The binary, `src/bin/tabnas-lsp.rs`, is a launcher over
//! [`server::serve`]; with the `fleet` feature it links the fleet
//! grammars in, with `dialects` it compiles `.abnf`, `.ebnf` and `.gbnf`
//! grammar files.

#![forbid(unsafe_code)]

pub mod analyze;
pub mod completion;
pub mod documents;
pub mod highlight;
pub mod hover;
pub mod instances;
pub mod jsonrpc;
pub mod loaders;
pub mod outline;
pub mod registry;
pub mod semantic;
pub mod server;
pub mod trace;
pub mod types;

pub use analyze::{analyze, diagnostics, Analysis, SemanticTokens};
pub use completion::{completion, CompletionItem};
pub use documents::DocumentStore;
pub use highlight::{highlight, Highlight, Highlighter, Span};
pub use hover::{hover, Hover, MarkupContent};
pub use instances::Instances;
pub use loaders::Loader;
pub use outline::{outline, DocumentSymbol};
pub use registry::{Registry, RegistryError, Resolution, Router, Via, REGISTRY_JSON};
pub use semantic::{
    default_token_type, encode, prefix_token_type, semantic_tokens, semantic_tokens_data,
    semantic_tokens_reconciled, token_type, token_type_name, Overrides, SemanticToken, TokenType,
    DEFAULT_TOKEN_TYPES, LEGEND, PREFIX_TYPES,
};
pub use server::{serve, Server};
pub use trace::{anchor, reconcile, reconcile_in, LexTrace, TokenPoint};
pub use types::{
    CodeDescription, Collected, Config, Diagnostic, Doc, Entry, EntrySource, Issue, Load,
    LoadError, MakeInstance, Position, PositionEncoding, Range, RuleEvent, RuleEventState, Scope,
    SpecSource,
};

/// This crate's version. It MUST equal `ts/package.json` "version": the
/// release orchestrator rewrites every version site together, and
/// `tests/version_test.rs` fails the build if they drift. Mirrors
/// `VERSION` in `ts/src/core.js` and `const VERSION` in `go/lsp.go`.
pub const VERSION: &str = "0.1.4";

/// The README's Rust examples run as doctests, so a stale one fails the
/// gate rather than misleading the reader. Its `toml` and `bash` fences
/// are skipped; rustdoc runs only the `rust` ones.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme_examples {}
