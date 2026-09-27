// Copyright (c) 2026 Richard Rodger, MIT License

//! Hover (design §8): the token under the cursor plus its description,
//! degrading to nothing. Mirrors `onHover` in `ts/src/server.js`, which
//! today answers `null` (token descriptions are tracked work, design
//! §14); the Go port has no hover. This module is the seam for that
//! feature: the token is found in the analysis's reconciled trace, and
//! the content, when TypeScript ships it, is pinned by a `hover` fixture
//! section. Until TypeScript answers with content, parity means this
//! port answers `None` too.
//!
//! Status: types complete; [`hover`] and [`token_at`] are signatures for
//! the analyze module agent.

use serde::{Deserialize, Serialize};

use crate::analyze::Analysis;
use crate::trace::TokenPoint;
use crate::types::{Doc, Entry, Position, Range};

/// LSP `MarkupContent`; `kind` is `markdown` or `plaintext`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkupContent {
    pub kind: String,
    pub value: String,
}

/// An LSP hover result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hover {
    pub contents: MarkupContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

/// The hover at a position over the current analysis of `doc`, `None`
/// when there is nothing to say (no token there, no description for it,
/// or, today, always: see the module docs).
#[allow(unused_variables)] // stub
pub fn hover(analysis: &Analysis, entry: &Entry, doc: &Doc, position: Position) -> Option<Hover> {
    todo!("hover::hover")
}

/// The reconciled token whose span covers a wire position, if any.
#[allow(unused_variables)] // stub
pub fn token_at<'a>(
    reconciled: &'a [TokenPoint],
    doc: &Doc,
    position: Position,
) -> Option<&'a TokenPoint> {
    todo!("hover::token_at")
}
