// Copyright (c) 2026 Richard Rodger, MIT License

//! The language registry, read from the GENERATED `ts/data/registry.json`
//! (design §5), embedded at build time so a host needs no file access.
//!
//! The file is derived from the fleet's `tabnas.plugin.json` descriptors
//! by `ts/tools/gen-registry.js` and is never hand-edited; this module
//! reads it and applies the same defaults `ts/src/registry.js`
//! `normalize` applies. Two fields matter to semantic tokens: `lexStream`
//! (`clean` | `speculative`), which gates whether an entry serves them at
//! all, and `semanticTokens`, the entry's token-name overrides.

use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer};

use crate::semantic::Overrides;

/// The generated registry, as committed at `ts/data/registry.json`.
pub const REGISTRY_JSON: &str = include_str!("../../ts/data/registry.json");

/// A registry entry, with the descriptor's fields as written. Use the
/// accessors for the normalized values.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// The plugin package name, `@tabnas/json`.
    pub name: String,
    /// The editor language id, when the descriptor declares one.
    #[serde(default)]
    pub language_id: Option<String>,
    #[serde(default, deserialize_with = "null_is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, deserialize_with = "null_is_empty")]
    pub media_types: Vec<String>,
    /// `grammar` | `compiler` | `modifier`.
    #[serde(default)]
    pub plugin_kind: Option<String>,
    /// `data` | `compiled` | `closure` | `imperative` | `external`.
    #[serde(default)]
    pub grammar_kind: Option<String>,
    /// `clean` | `speculative`.
    #[serde(default)]
    pub lex_stream: Option<String>,
    /// Engine token name to LSP token type name.
    #[serde(default)]
    pub semantic_tokens: Option<Overrides>,
    #[serde(default)]
    pub sync_groups: Option<Vec<String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default, deserialize_with = "null_is_empty")]
    pub error_codes: Vec<String>,
}

/// `null` for an array field reads as an empty array, as the TypeScript
/// `normalize` reads it (`e.extensions || []`).
fn null_is_empty<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    Ok(Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default())
}

/// A declared string field, absent when missing, null OR EMPTY: the
/// TypeScript `normalize` tests truthiness, so `""` takes the default.
fn declared(field: &Option<String>) -> Option<&str> {
    field.as_deref().filter(|value| !value.is_empty())
}

impl Entry {
    /// The language id: the declared one, else the package name without
    /// its `@tabnas/` scope. An empty declaration counts as none, as it
    /// does in the TypeScript `normalize` (`e.languageId || ...`).
    pub fn language_id(&self) -> &str {
        match declared(&self.language_id) {
            Some(id) => id,
            None => self.name.strip_prefix("@tabnas/").unwrap_or(&self.name),
        }
    }

    /// The lex stream classification, `clean` when the descriptor is
    /// silent.
    pub fn lex_stream(&self) -> &str {
        declared(&self.lex_stream).unwrap_or("clean")
    }

    /// Whether the entry serves semantic tokens: its lex stream is
    /// `clean` (no negotiated re-lexing, no rewind), so the trace
    /// reconciles to the tokens the parse used.
    pub fn is_clean(&self) -> bool {
        self.lex_stream() == "clean"
    }

    /// The entry's token-name overrides, if it declares any.
    pub fn overrides(&self) -> Option<&Overrides> {
        self.semantic_tokens.as_ref()
    }

    /// `grammar` when the descriptor is silent.
    pub fn plugin_kind(&self) -> &str {
        declared(&self.plugin_kind).unwrap_or("grammar")
    }

    /// Enabled unless the descriptor says `false` (the editor-collision
    /// policy the generator applies).
    pub fn is_enabled(&self) -> bool {
        self.enabled != Some(false)
    }
}

/// The registry: the generated file's entries, indexed by language id.
#[derive(Debug, Clone, Deserialize)]
pub struct Registry {
    /// The generator that wrote the file.
    #[serde(default)]
    pub generated: Option<String>,
    /// The declared entry count; [`Registry::from_json`] checks it.
    pub count: usize,
    pub entries: Vec<Entry>,
}

/// A registry that does not parse or does not describe itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryError(pub String);

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RegistryError {}

impl Registry {
    /// Parse a registry document, refusing one whose `count` disagrees
    /// with its entries (the CI sanity check, applied here too).
    pub fn from_json(src: &str) -> Result<Registry, RegistryError> {
        let registry: Registry = serde_json::from_str(src)
            .map_err(|error| RegistryError(format!("registry.json: {error}")))?;
        if registry.count != registry.entries.len() {
            return Err(RegistryError(format!(
                "registry.json count {} disagrees with its {} entries",
                registry.count,
                registry.entries.len()
            )));
        }
        if registry.entries.is_empty() {
            return Err(RegistryError("registry.json has no entries".into()));
        }
        Ok(registry)
    }

    /// The embedded [`REGISTRY_JSON`], parsed once.
    ///
    /// # Panics
    ///
    /// When the embedded file does not parse. It is generated and
    /// committed, and `tests/registry_test.rs` parses it, so a build that
    /// passes its tests cannot reach the panic.
    pub fn bundled() -> &'static Registry {
        static BUNDLED: OnceLock<Registry> = OnceLock::new();
        BUNDLED.get_or_init(|| {
            Registry::from_json(REGISTRY_JSON).expect("the embedded ts/data/registry.json is sound")
        })
    }

    /// The entry with this language id.
    pub fn entry(&self, language_id: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| entry.language_id() == language_id)
    }

    /// Whether the registry knows this language id AND classifies its
    /// lex stream as clean. An unknown id is `false`: the registry says
    /// nothing about it, and the caller decides.
    pub fn clean(&self, language_id: &str) -> bool {
        self.entry(language_id).is_some_and(Entry::is_clean)
    }

    /// The language's token-name overrides, if the registry knows the
    /// language and it declares any.
    pub fn overrides(&self, language_id: &str) -> Option<&Overrides> {
        self.entry(language_id).and_then(Entry::overrides)
    }

    /// Every language id, in file order.
    pub fn language_ids(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(Entry::language_id)
    }

    /// The entries by language id.
    pub fn by_language_id(&self) -> HashMap<&str, &Entry> {
        self.entries
            .iter()
            .map(|entry| (entry.language_id(), entry))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_that_disagrees_is_refused() {
        let bad = r#"{"count": 2, "entries": [{"name": "@tabnas/x"}]}"#;
        let error = Registry::from_json(bad).unwrap_err();
        assert!(error.to_string().contains("disagrees"), "{error}");
        let empty = r#"{"count": 0, "entries": []}"#;
        assert!(Registry::from_json(empty).is_err());
        assert!(Registry::from_json("nope").is_err());
    }

    #[test]
    fn falsy_fields_take_the_typescript_defaults() {
        // `normalize` tests truthiness: an empty language id is no id,
        // an empty classification is the default one, and a null array
        // is an empty one. The generated file never carries these
        // shapes; a hand-written registry may.
        let src = r#"{"count": 1, "entries": [
            {"name": "@tabnas/x", "languageId": "", "extensions": null,
             "mediaTypes": null, "errorCodes": null, "lexStream": "",
             "pluginKind": "", "semanticTokens": null, "syncGroups": null,
             "enabled": null}
        ]}"#;
        let registry = Registry::from_json(src).unwrap();
        let x = registry
            .entry("x")
            .expect("an empty id falls back to the package name");
        assert_eq!(x.language_id(), "x");
        assert!(x.extensions.is_empty());
        assert!(x.media_types.is_empty());
        assert!(x.error_codes.is_empty());
        assert_eq!(x.lex_stream(), "clean");
        assert_eq!(x.plugin_kind(), "grammar");
        assert_eq!(x.overrides(), None);
        assert!(x.is_enabled());
        assert!(registry.clean("x"));
    }

    #[test]
    fn normalization_follows_the_typescript_registry() {
        let src = r##"{"count": 3, "entries": [
            {"name": "@tabnas/x"},
            {"name": "@tabnas/y", "languageId": "why", "lexStream": "speculative",
             "semanticTokens": {"#PL": "type"}, "enabled": false},
            {"name": "plain", "lexStream": "clean"}
        ]}"##;
        let registry = Registry::from_json(src).unwrap();
        let x = registry.entry("x").unwrap();
        assert_eq!(x.language_id(), "x");
        assert_eq!(x.lex_stream(), "clean");
        assert!(x.is_clean());
        assert!(x.is_enabled());
        assert_eq!(x.plugin_kind(), "grammar");
        assert_eq!(x.overrides(), None);
        let y = registry.entry("why").unwrap();
        assert!(!y.is_clean());
        assert!(!y.is_enabled());
        assert_eq!(
            y.overrides().unwrap().get("#PL").map(String::as_str),
            Some("type")
        );
        assert!(registry.entry("y").is_none());
        assert!(registry.entry("plain").is_some());
        assert!(registry.clean("x"));
        assert!(!registry.clean("why"));
        assert!(!registry.clean("unknown"));
        assert_eq!(registry.overrides("why").unwrap().len(), 1);
        assert_eq!(registry.overrides("x"), None);
        assert_eq!(
            registry.language_ids().collect::<Vec<_>>(),
            ["x", "why", "plain"]
        );
        assert_eq!(registry.by_language_id().len(), 3);
    }
}
