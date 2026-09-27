// Copyright (c) 2026 Richard Rodger, MIT License

//! Completion (design §8) via the engine's continuation primitive:
//! the tokens that may legally follow the document prefix up to the
//! cursor, as completion items, with a fixed token's source as its
//! label. Mirrors `completion` in `ts/src/core.js` (canonical) and
//! `Complete` in `go/completion.go`.
//!
//! The engine call is `continuations(prefix)`, which parses internally,
//! so it runs under [`Instances::with_parse_lock`] with no collector
//! active. Sentinel tokens a user can never type are filtered:
//! `#ZZ` (end of source, returned whenever the prefix already parses),
//! `#AA` (match-any) and `#BD` (the bad-token marker). A token with a
//! fixed source (`fixed_source`) is labelled by that source with kind
//! Operator (24) and `insertText` set; any other by its name with kind
//! Keyword (14); `detail` is always the token name.
//!
//! Status: the constants and the item type are complete;
//! [`completion`] and [`fixed_source`] are signatures for the
//! completion module agent. The `completions` fixture section (sorted
//! labels at a position) is the contract; `rs/tests/conformance_test.rs`
//! `run_completions` executes it.

use serde::{Deserialize, Serialize};
use tabnas::Tabnas;

use crate::instances::Instances;
use crate::types::{Doc, Entry, Position};

/// Tokens the engine may name as legal continuations that a user can
/// never type.
pub const SENTINEL_TOKENS: [&str; 3] = ["#ZZ", "#AA", "#BD"];

/// The characters the server advertises as completion triggers
/// (`completionProvider.triggerCharacters` in both canonical servers).
pub const TRIGGER_CHARACTERS: [&str; 5] = [":", ",", "{", "[", "\""];

/// `CompletionItemKind.Keyword`, for a token named by its token name.
pub const COMPLETION_KIND_KEYWORD: u32 = 14;
/// `CompletionItemKind.Operator`, for a token with a fixed source.
pub const COMPLETION_KIND_OPERATOR: u32 = 24;

/// An LSP completion item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionItem {
    pub label: String,
    pub kind: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert_text: Option<String>,
}

/// Whether a continuation token is one of the [`SENTINEL_TOKENS`].
pub fn is_sentinel(name: &str) -> bool {
    SENTINEL_TOKENS.contains(&name)
}

/// The completion items at `position` in `doc`. `instances`, when given,
/// supplies the parse lock the query runs under (a host analysing on
/// one thread with no collector may pass `None`, as Go allows).
#[allow(unused_variables)] // stub
pub fn completion(
    instances: Option<&Instances>,
    inst: &Tabnas,
    entry: &Entry,
    doc: &Doc,
    position: Position,
) -> Vec<CompletionItem> {
    todo!("completion::completion: ts/src/core.js completion")
}

/// The fixed source text of the token with this name, if the grammar
/// declares one (`inst.token(name)` then `inst.fixed(tin)` in
/// TypeScript; `FixedTin` in Go).
#[allow(unused_variables)] // stub
pub fn fixed_source(inst: &Tabnas, name: &str) -> Option<String> {
    todo!("completion::fixed_source")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinels_are_the_three_engine_internal_tokens() {
        for name in SENTINEL_TOKENS {
            assert!(is_sentinel(name));
        }
        assert!(!is_sentinel("#ST"));
        assert!(!is_sentinel("#CL"));
    }

    #[test]
    fn an_item_serializes_in_the_lsp_shape() {
        let item = CompletionItem {
            label: ":".into(),
            kind: COMPLETION_KIND_OPERATOR,
            detail: Some("#CL".into()),
            insert_text: Some(":".into()),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["insertText"], ":");
        assert_eq!(json["kind"], 24);
        let plain = CompletionItem {
            label: "#NR".into(),
            kind: COMPLETION_KIND_KEYWORD,
            detail: Some("#NR".into()),
            insert_text: None,
        };
        assert!(serde_json::to_value(&plain)
            .unwrap()
            .get("insertText")
            .is_none());
    }
}
