// Copyright (c) 2026 Richard Rodger, MIT License

//! Semantic tokens for tabnas grammars, derived from the engine's lex
//! trace with no per-language code.
//!
//! This crate is the Rust port of ONE half of the tabnas language server
//! pipeline (`ts/src/core.js`, canonical; `go/semantic.go`, the Go
//! port): the reconciled lex trace, the token-name to token-type map,
//! and the LSP semantic-token encoding. It exists so that Rust hosts,
//! the `aless` terminal viewer first, can highlight any tabnas grammar's
//! text the way the language server does. It is not a language server:
//! diagnostics, outline and completion stay in the TypeScript and Go
//! packages.
//!
//! The pipeline, module by module:
//!
//! - [`trace`]: a [`LexTrace`] collector installed with the engine's
//!   `subscribe_lex`, and [`reconcile`], the documented lex-trace
//!   contract (newest event per position wins, a claimed span shadows
//!   older events inside it).
//! - [`semantic`]: the CANON default map, the prefix conventions, the
//!   fixed [`LEGEND`], [`token_type`], and [`semantic_tokens`] with the
//!   LSP delta [`encode`].
//! - [`registry`]: the generated `ts/data/registry.json`, embedded, for
//!   `lexStream` (which grammars may serve semantic tokens at all) and
//!   the per-entry `semanticTokens` overrides.
//! - [`mod@highlight`]: the host-facing convenience, [`highlight()`] and
//!   [`Highlighter`], which parse and return byte spans.
//!
//! Parity with the canonical pipeline is pinned by
//! `test/fixtures/lsp-conformance.json` (its `semantic` section), which
//! the TypeScript, Go and Rust suites all execute.

#![forbid(unsafe_code)]

pub mod highlight;
pub mod registry;
pub mod semantic;
pub mod trace;

pub use highlight::{highlight, Highlight, Highlighter, Span};
pub use registry::{Entry, Registry, RegistryError, REGISTRY_JSON};
pub use semantic::{
    default_token_type, encode, prefix_token_type, semantic_tokens, semantic_tokens_data,
    semantic_tokens_reconciled, token_type, token_type_name, Overrides, SemanticToken, TokenType,
    DEFAULT_TOKEN_TYPES, LEGEND, PREFIX_TYPES,
};
pub use trace::{reconcile, LexTrace, TokenPoint};

/// This crate's version. It MUST equal `ts/package.json` "version": the
/// release orchestrator rewrites every version site together, and
/// `tests/version_test.rs` fails the build if they drift. Mirrors
/// `VERSION` in `ts/src/core.js` and `const VERSION` in `go/lsp.go`.
pub const VERSION: &str = "0.1.3";

/// The README's Rust examples run as doctests, so a stale one fails the
/// gate rather than misleading the reader. Its `toml` and `bash` fences
/// are skipped; rustdoc runs only the `rust` ones.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme_examples {}
