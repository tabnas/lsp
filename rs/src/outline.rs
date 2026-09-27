// Copyright (c) 2026 Richard Rodger, MIT License

//! The outline (design §8): `ruleDone` events, forced closes synthesized
//! by recovery included, filtered by rule name and nested by span
//! containment into `DocumentSymbol`s. Mirrors `outline` in
//! `ts/src/core.js` (canonical) and `go/outline.go`.
//!
//! The derivation: an open-pass event whose rule the rules map names
//! (`map` Object, `list` Array by default; an entry's `outlineRules`
//! over that) opens a symbol at its first matched token; the matching
//! close-pass event (same rule index) ends it at ITS first matched
//! token's end, or, for a forced close with no token, at the open
//! token's end. Symbols are sorted by start (wider first on ties) and a
//! stack walk assigns parents. `kind` is 18 for Array, 19 for Object.
//!
//! Status: [`outline_rules`] and the types are complete; [`outline`] is
//! a signature for the analyze module agent (the `analyze` fixture
//! section pins the name tree).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::{Doc, Entry, Range, RuleEvent};

/// Rule names to symbol labels, before an entry's `outlineRules`.
pub const DEFAULT_OUTLINE_RULES: [(&str, &str); 2] = [("map", "Object"), ("list", "Array")];

/// `SymbolKind.Array`.
pub const SYMBOL_KIND_ARRAY: u32 = 18;
/// `SymbolKind.Object`.
pub const SYMBOL_KIND_OBJECT: u32 = 19;

/// An LSP hierarchical document symbol. `children` is always present,
/// empty for a leaf, as both canonical ports emit it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: u32,
    pub range: Range,
    pub selection_range: Range,
    #[serde(default)]
    pub children: Vec<DocumentSymbol>,
}

/// The rules map for an entry: the defaults, with the entry's
/// `outlineRules` laid over them.
pub fn outline_rules(entry: Option<&Entry>) -> HashMap<String, String> {
    let mut rules: HashMap<String, String> = DEFAULT_OUTLINE_RULES
        .iter()
        .map(|&(rule, label)| (rule.to_string(), label.to_string()))
        .collect();
    if let Some(overrides) = entry.and_then(|entry| entry.outline_rules.as_ref()) {
        rules.extend(
            overrides
                .iter()
                .map(|(rule, label)| (rule.clone(), label.clone())),
        );
    }
    rules
}

/// Nested symbols from one parse's rule events.
#[allow(unused_variables)] // stub
pub fn outline(events: &[RuleEvent], entry: Option<&Entry>, doc: &Doc) -> Vec<DocumentSymbol> {
    todo!("outline::outline: ts/src/core.js outline")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rules_map_takes_the_entry_overrides() {
        let defaults = outline_rules(None);
        assert_eq!(defaults.get("map").map(String::as_str), Some("Object"));
        assert_eq!(defaults.get("list").map(String::as_str), Some("Array"));
        let mut entry = Entry::new("x");
        entry.outline_rules = Some(HashMap::from([
            ("map".to_string(), "Table".to_string()),
            ("section".to_string(), "Section".to_string()),
        ]));
        let rules = outline_rules(Some(&entry));
        assert_eq!(rules.get("map").map(String::as_str), Some("Table"));
        assert_eq!(rules.get("list").map(String::as_str), Some("Array"));
        assert_eq!(rules.get("section").map(String::as_str), Some("Section"));
    }

    #[test]
    fn a_symbol_serializes_with_its_children_present() {
        let symbol = DocumentSymbol {
            name: "Object".into(),
            kind: SYMBOL_KIND_OBJECT,
            range: Range::default(),
            selection_range: Range::default(),
            children: Vec::new(),
        };
        let json = serde_json::to_value(&symbol).unwrap();
        assert_eq!(json["kind"], 19);
        assert!(json["children"].is_array());
        assert!(json.get("selectionRange").is_some());
    }
}
