// Copyright (c) 2026 Richard Rodger, MIT License

//! The language registry: the GENERATED `ts/data/registry.json` (design
//! §5) embedded at build time so a host needs no file access, the
//! configuration tiers over it, and document routing.
//!
//! The file is derived from the fleet's `tabnas.plugin.json` descriptors
//! by `ts/tools/gen-registry.js` and is never hand-edited; [`Registry`]
//! reads it and the [`Entry`] accessors apply the same defaults
//! `ts/src/registry.js` `normalize` applies. [`Router`] is that file's
//! `Registry` class: the tiers (workspace over user over bundled) and
//! `resolve`, which turns a document's language id and URI into the
//! entry that serves it. `go/registry.go` is the flat single-tier form
//! a generated server needs.
//!
//! Status: the embedded file and its defaults are complete and tested;
//! the tiers and routing ([`Router`], [`ext_of`], [`fs_path_of`],
//! [`contains`]) are signatures for the registry module agent to fill,
//! rule for rule from `ts/src/registry.js`: the client's `languageId`
//! wins only when it names an enabled, non-modifier entry; otherwise the
//! most specific extension match; workspace entries apply to documents
//! inside their folder, deepest folder first, then session-wide ones;
//! ties surface as [`Resolution::ambiguous`], never a silent pick.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::semantic::Overrides;
pub use crate::types::Entry;

/// The generated registry, as committed at `ts/data/registry.json`.
pub const REGISTRY_JSON: &str = include_str!("../../ts/data/registry.json");

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

impl Registry {
    /// The bundled tier as a host or a [`Router`] takes it: every entry,
    /// cloned, in file order.
    pub fn to_entries(&self) -> Vec<Entry> {
        self.entries.clone()
    }
}

// ---------------------------------------------------------------------
// Tiers and routing: the `Registry` class of ts/src/registry.js.

/// How a document was resolved to its entry (`via` in
/// `ts/src/registry.js`): a workspace entry by the client's language id
/// or by extension, else a global (user or bundled) entry by language id
/// or by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Via {
    WorkspaceLanguageId,
    WorkspaceExtension,
    LanguageId,
    Extension,
}

impl Via {
    /// The TypeScript spelling: `workspace:languageId`,
    /// `workspace:extension`, `languageId`, `extension`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Via::WorkspaceLanguageId => "workspace:languageId",
            Via::WorkspaceExtension => "workspace:extension",
            Via::LanguageId => "languageId",
            Via::Extension => "extension",
        }
    }
}

/// The outcome of routing one document: the entry that serves it (none
/// when nothing claims it), how it was reached, and, when an extension
/// is claimed by more than one enabled entry, every claimant's language
/// id with the chosen one first, so the tie can surface as a diagnostic
/// rather than a silent pick.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution<'a> {
    pub entry: Option<&'a Entry>,
    pub via: Option<Via>,
    pub ambiguous: Vec<String>,
}

impl Resolution<'_> {
    /// Nothing claims the document.
    pub const fn none() -> Self {
        Resolution {
            entry: None,
            via: None,
            ambiguous: Vec::new(),
        }
    }
}

/// The configuration tiers and the routing over them, the TypeScript
/// `Registry` class (`go/registry.go` is its single-tier form). Global
/// entries (bundled, then user, later ones replacing earlier ones with
/// the same language id) are keyed by language id; workspace entries are
/// folder-scoped and kept in the order given.
///
/// Status: signatures. The registry module agent ports `resolve` rule
/// for rule, including the Windows-shaped path handling of `contains`
/// and the `_scope` versus `_dir` distinction (see [`Entry::routing_scope`]).
#[derive(Debug, Clone, Default)]
pub struct Router {
    global: Vec<Entry>,
    workspace: Vec<Entry>,
}

impl Router {
    /// The tiers, as `new Registry(bundled, workspace, user)` takes them.
    #[allow(unused_variables)] // stub
    pub fn new(bundled: Vec<Entry>, workspace: Vec<Entry>, user: Vec<Entry>) -> Router {
        todo!("registry::Router::new: the global map (user over bundled) and the workspace list")
    }

    /// The global entry with this language id, if any.
    #[allow(unused_variables)] // stub
    pub fn get(&self, language_id: &str) -> Option<&Entry> {
        todo!("registry::Router::get")
    }

    /// Resolve a document to the entry that serves it: workspace entries
    /// scoped to a folder containing the document (deepest first), then
    /// session-wide workspace entries, by language id then by extension;
    /// then the client's language id when it names an enabled
    /// non-modifier global entry; then the most specific extension match
    /// over the global entries, ties reported.
    #[allow(unused_variables)] // stub
    pub fn resolve(&self, language_id: &str, uri: &str) -> Resolution<'_> {
        todo!("registry::Router::resolve: ts/src/registry.js Registry.resolve")
    }

    /// Every entry, global tier first, then the workspace tier: what
    /// `tabnas/status` lists and what hot reload walks.
    pub fn all(&self) -> impl Iterator<Item = &Entry> {
        self.global.iter().chain(self.workspace.iter())
    }
}

/// The lowercased extension of a URI or path (`.json`), `None` when the
/// last segment has none. `extOf` in `ts/src/registry.js`, `ExtOf` in
/// `go/documents.go`.
#[allow(unused_variables)] // stub
pub fn ext_of(uri: &str) -> Option<String> {
    todo!("registry::ext_of")
}

/// The filesystem path of a `file:` URI, percent-decoded, with the
/// Windows drive form `/c:/dir` reduced to `c:/dir`; `None` for any
/// other scheme (`untitled:`, `vscode-notebook-cell:`), to which folder
/// scoping never applies. `fsPathOf` in `ts/src/registry.js`.
#[allow(unused_variables)] // stub
pub fn fs_path_of(uri: &str) -> Option<String> {
    todo!("registry::fs_path_of")
}

/// Whether `fs_path` is `dir` or inside it. A Windows-shaped `dir` (a
/// drive letter or a UNC root) is compared with forward slashes, by the
/// path's shape and never by the host platform; a POSIX backslash is an
/// ordinary character. `contains` in `ts/src/registry.js`.
#[allow(unused_variables)] // stub
pub fn contains(dir: &Path, fs_path: &str) -> bool {
    todo!("registry::contains")
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
