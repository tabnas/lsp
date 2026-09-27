// Copyright (c) 2026 Richard Rodger, MIT License

//! The parse pipeline (design §8): ONE parse per change, with the mux
//! collecting, yields diagnostics, semantic tokens and outline together.
//! Protocol-free: [`crate::server`] is a thin front-end over this module,
//! and a Rust host calls it directly.
//!
//! Mirrors `analyze` and `diagnostics` in `ts/src/core.js` (canonical)
//! and `Analyze` / `Diagnostics` in `go/core.go`. The engine calls are
//! `parse_recover` (multi-error diagnostics: every recovered error plus
//! the fatal one, if the parse ended on one) through
//! [`Instances::parse`], which runs the parse with the collector active.
//!
//! Diagnostics are built from the engine error's SERIALIZED form
//! (`code`, `message`, `hint`, `row`, `col`, `pos`, `len`: the same
//! JSON both canonical runtimes consume), so the three ports cannot
//! disagree about codes or the `len` unit; the range comes from
//! [`Doc::range_from`]. Semantic tokens are served only for `lexStream:
//! clean` entries; the outline is derived from the rule events whatever
//! the entry.
//!
//! Status: signatures for the analyze module agent. The `analyze`
//! section of `test/fixtures/lsp-conformance.json` (codes in order, the
//! first diagnostic's range, the outline's name tree) is the contract;
//! `rs/tests/conformance_test.rs` `run_analyze` executes it.

use tabnas::{Tabnas, TabnasError, Value};

use crate::instances::Instances;
use crate::outline::DocumentSymbol;
use crate::semantic::SemanticToken;
use crate::trace::TokenPoint;
use crate::types::{Diagnostic, Doc, Entry};

/// The semantic tokens of one analysis: the delta-encoded `data` the
/// server sends (over the fixed [`crate::LEGEND`]) and the decoded
/// tokens beside it for hosts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SemanticTokens {
    pub data: Vec<u32>,
    pub tokens: Vec<SemanticToken>,
}

/// Everything one parse yields (`analyze`'s return in TypeScript,
/// `Analysis` in Go).
#[derive(Debug, Clone)]
pub struct Analysis {
    /// The parsed value, when the parse produced one.
    pub value: Option<Value>,
    /// The engine's errors, in order: the recovered ones, then the fatal
    /// one if the parse ended on it.
    pub errors: Vec<TabnasError>,
    /// The parse did not run to completion (recovery gave up, or an
    /// internal error): diagnostics are served, structural results are
    /// not cached.
    pub failed: bool,
    pub diagnostics: Vec<Diagnostic>,
    /// `None` for an entry whose lex stream is not clean.
    pub semantic_tokens: Option<SemanticTokens>,
    pub outline: Vec<DocumentSymbol>,
    /// The reconciled lex trace: every token the parse used, mapped to a
    /// legend entry or not, for hover and for hosts.
    pub reconciled: Vec<TokenPoint>,
}

/// Run one parse of `doc` with `inst` (built for `entry`, with recovery
/// enabled) and derive every artifact.
#[allow(unused_variables)] // stub
pub fn analyze(instances: &Instances, inst: &Tabnas, entry: &Entry, doc: &Doc) -> Analysis {
    todo!("analyze::analyze: ts/src/core.js analyze")
}

/// Engine errors to LSP diagnostics: severity 1, the engine's `code`,
/// `source` `tabnas:<languageId>` (`tabnas` with no entry), the message
/// with the hint after a blank line, a `codeDescription` into the error
/// registry for every code but `unknown`, the range through the
/// document text.
#[allow(unused_variables)] // stub
pub fn diagnostics(errors: &[TabnasError], entry: Option<&Entry>, doc: &Doc) -> Vec<Diagnostic> {
    todo!("analyze::diagnostics: ts/src/core.js diagnostics")
}

/// The semantic tokens of one parse's lex events for an entry: the
/// trace reconciled, mapped through the entry's overrides and encoded
/// ([`crate::semantic::semantic_tokens`] and [`crate::semantic::encode`]).
#[allow(unused_variables)] // stub
pub fn semantic_tokens_of(lex: &[TokenPoint], entry: &Entry, doc: &Doc) -> SemanticTokens {
    todo!("analyze::semantic_tokens_of")
}
