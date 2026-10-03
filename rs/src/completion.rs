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
//! The prefix is the document text up to the cursor, cut at the byte
//! offset [`Doc::offset_at`] gives for the position in the document's
//! negotiated encoding, so a cursor in the middle of a token (inside a
//! string, a number, a keyword) asks what may follow the token's first
//! part, exactly as the TypeScript `doc.text.substring(0,
//! doc.offsetAt(position))` does: the engine is the one that decides what
//! a half-typed token admits.
//!
//! Where the answer can differ from the TypeScript server's, none of it
//! in the conformance fixtures, and none of it this module's doing:
//!
//! - A `character` past the end of its line clamps to the line's end in
//!   [`Doc::offset_at`] (the protocol's rule, and the Go port's); the
//!   TypeScript store adds it to the line start and so completes against
//!   text from the lines after the cursor.
//! - A grammar callback that panics: the TypeScript engine lets the throw
//!   reach the core, whose `catch` answers with no items; the Rust engine
//!   catches the panic inside `continuations` and answers with the start
//!   rule's openers. Only grammar code (an L1 closure) can panic;
//!   pure-data grammars cannot. The `catch_unwind` here is the TypeScript
//!   `catch` for anything that does get out.

use std::panic::{catch_unwind, AssertUnwindSafe};

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

/// The completion items at `position` in `doc`: the engine's
/// continuations of the text before the cursor, sentinels dropped, in the
/// engine's order (ascending token identity). `instances`, when given,
/// supplies the parse lock the query runs under with no collector active,
/// since `continuations` parses internally and its events must not reach
/// any analysis (`WithParseLock` in Go; TypeScript needs no lock on its
/// one thread). A host analysing on one thread with no collector active
/// may pass `None`, as Go allows. `entry` is unused, as it is in the
/// canonical `completion(inst, entry, doc, position)`; it keeps the
/// signature the other derivations share.
pub fn completion(
    instances: Option<&Instances>,
    inst: &Tabnas,
    entry: &Entry,
    doc: &Doc,
    position: Position,
) -> Vec<CompletionItem> {
    let _ = entry;
    let prefix = &doc.text[..doc.offset_at(position)];
    // The TypeScript `try { inst.continuations(prefix) } catch { return
    // [] }`, and Go's `recover()`: caught inside the lock, so the lock and
    // the collector slot are released on the normal path.
    let query = || catch_unwind(AssertUnwindSafe(|| inst.continuations(prefix))).ok();
    let continuations = match instances {
        Some(instances) => instances.with_parse_lock(query),
        None => query(),
    };
    match continuations {
        Some(continuations) => items(inst, &continuations.tokens),
        None => Vec::new(),
    }
}

/// The items for a list of continuation token names, sentinels dropped:
/// the loop of the canonical `completion`.
fn items(inst: &Tabnas, names: &[String]) -> Vec<CompletionItem> {
    names
        .iter()
        .filter(|name| !is_sentinel(name))
        .map(|name| item(inst, name))
        .collect()
}

/// One continuation token as an item. A fixed token is labelled by its
/// source, kind Operator, with `insertText`; any other by its name, kind
/// Keyword; `detail` is the name either way. An empty fixed source counts
/// as none, as the TypeScript `fixedSrc || name` (and Go's `"" != fixed`)
/// has it.
fn item(inst: &Tabnas, name: &str) -> CompletionItem {
    match fixed_source(inst, name).filter(|source| !source.is_empty()) {
        Some(source) => CompletionItem {
            label: source.clone(),
            kind: COMPLETION_KIND_OPERATOR,
            detail: Some(name.to_owned()),
            insert_text: Some(source),
        },
        None => CompletionItem {
            label: name.to_owned(),
            kind: COMPLETION_KIND_KEYWORD,
            detail: Some(name.to_owned()),
            insert_text: None,
        },
    }
}

/// The fixed source text of the token with this name, if the grammar
/// declares one: `inst.token(name)` then `inst.fixed(tin)` in
/// TypeScript (`FixedTin` in Go, which starts from the token identity the
/// engine returned alongside the name).
///
/// The name resolves the way the TypeScript `token` does: first as a
/// fixed SOURCE (`fixed.token[ref]`, so `fixed_source(inst, ":")` is
/// `":"`), then as a token name. The lookup reads the instance and never
/// allocates a token, where the TypeScript `token` registers an unknown
/// name; an unknown name has no fixed source either way.
pub fn fixed_source(inst: &Tabnas, name: &str) -> Option<String> {
    let tin = inst.fixed(name).or_else(|| inst.options.token(name))?;
    inst.fixed_source(tin).map(str::to_owned)
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

    #[test]
    fn items_drop_sentinels_and_keep_the_engine_order() {
        let inst = Tabnas::new();
        let names: Vec<String> = ["#ZZ", "#CL", "#NR", "#AA", "#OB", "#BD"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        let labels: Vec<(String, u32)> = items(&inst, &names)
            .into_iter()
            .map(|item| (item.label, item.kind))
            .collect();
        assert_eq!(
            labels,
            [
                (":".to_string(), COMPLETION_KIND_OPERATOR),
                ("#NR".to_string(), COMPLETION_KIND_KEYWORD),
                ("{".to_string(), COMPLETION_KIND_OPERATOR),
            ]
        );
    }

    #[test]
    fn an_empty_fixed_source_names_the_token() {
        // `fixedSrc || name`: an empty source is falsy in TypeScript.
        let mut inst = Tabnas::new();
        inst.token_with_source("#EM", "");
        assert_eq!(fixed_source(&inst, "#EM").as_deref(), Some(""));
        let item = item(&inst, "#EM");
        assert_eq!(item.label, "#EM");
        assert_eq!(item.kind, COMPLETION_KIND_KEYWORD);
        assert_eq!(item.insert_text, None);
        assert_eq!(item.detail.as_deref(), Some("#EM"));
    }
}
