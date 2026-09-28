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
//! Policy is the generated file's, never a copy: the editor-collision
//! defaults (`enabled: false` for the entrenched incumbents `json`,
//! `jsonc`, `css`, `markdown`, `yaml`, `c`, `toml`, `xml`, `ini` and
//! `proto`) are written by the generator's overrides table and read
//! here. Capabilities are the entry's fields: `lexStream` (which gates
//! semantic tokens, [`Entry::is_clean`]), `syncGroups`, `semanticTokens`
//! ([`Entry::overrides`]) and `errorCodes`.
//!
//! Routing, rule for rule from `ts/src/registry.js`: workspace entries
//! apply to documents inside their folder, deepest folder first, then
//! session-wide ones, by language id and then by extension; outside
//! them the client's `languageId` wins only when it names an enabled,
//! non-modifier entry; otherwise the extension match; ties surface as
//! [`Resolution::ambiguous`], never a silent pick. The design's media
//! types are carried by every entry and answered by
//! [`Router::resolve_media_type`] for hosts, and
//! [`Router::resolve_with_media_type`] puts them last in the order for
//! a host that knows a file's media type; the server's document routing
//! never consults them, in TypeScript or here, since an LSP client sends
//! none.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use crate::semantic::Overrides;
pub use crate::types::Entry;
use crate::types::Scope;

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
/// or by extension. [`Via::MediaType`] is a host's lookup
/// ([`Router::resolve_media_type`]) and never the outcome of
/// [`Router::resolve`]: an LSP client sends no media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Via {
    WorkspaceLanguageId,
    WorkspaceExtension,
    LanguageId,
    Extension,
    MediaType,
}

impl Via {
    /// The TypeScript spelling: `workspace:languageId`,
    /// `workspace:extension`, `languageId`, `extension`; and
    /// `mediaType` for the host lookup TypeScript does not have.
    pub const fn as_str(self) -> &'static str {
        match self {
            Via::WorkspaceLanguageId => "workspace:languageId",
            Via::WorkspaceExtension => "workspace:extension",
            Via::LanguageId => "languageId",
            Via::Extension => "extension",
            Via::MediaType => "mediaType",
        }
    }
}

impl fmt::Display for Via {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

impl<'a> Resolution<'a> {
    /// Nothing claims the document.
    pub const fn none() -> Self {
        Resolution {
            entry: None,
            via: None,
            ambiguous: Vec::new(),
        }
    }

    fn found(entry: &'a Entry, via: Via) -> Self {
        Resolution {
            entry: Some(entry),
            via: Some(via),
            ambiguous: Vec::new(),
        }
    }
}

/// The configuration tiers and the routing over them, the TypeScript
/// `Registry` class (`go/registry.go` is its single-tier form). Global
/// entries (bundled, then user, a later one replacing an earlier one
/// with the same language id IN ITS PLACE, as a JavaScript `Map.set`
/// does) are keyed by language id; workspace entries are folder-scoped
/// and kept in the order given.
///
/// Hot reload is the server's, over two entry points here: a changed
/// manifest replaces the workspace tier ([`Router::set_workspace`], what
/// `rebuildRegistry` does), and a changed grammar file names the entries
/// to rebuild ([`Router::reloaded_by`]).
#[derive(Debug, Clone, Default)]
pub struct Router {
    global: Vec<Entry>,
    by_id: HashMap<String, usize>,
    workspace: Vec<Entry>,
}

impl Router {
    /// The tiers, as `new Registry(bundled, workspace, user)` takes them.
    pub fn new(bundled: Vec<Entry>, workspace: Vec<Entry>, user: Vec<Entry>) -> Router {
        let mut router = Router {
            global: Vec::new(),
            by_id: HashMap::new(),
            workspace,
        };
        for entry in bundled.into_iter().chain(user) {
            router.put_global(entry);
        }
        router
    }

    /// Key an entry by its language id. A replaced key keeps its first
    /// position, as a JavaScript `Map` does, and the extension scan's
    /// order (which entry a tie names first) follows that position.
    fn put_global(&mut self, entry: Entry) {
        match self.by_id.get(entry.language_id()) {
            Some(&index) => self.global[index] = entry,
            None => {
                self.by_id
                    .insert(entry.language_id().to_string(), self.global.len());
                self.global.push(entry);
            }
        }
    }

    /// The global entry with this language id, if any.
    pub fn get(&self, language_id: &str) -> Option<&Entry> {
        self.by_id
            .get(language_id)
            .map(|&index| &self.global[index])
    }

    /// Resolve a document to the entry that serves it: workspace entries
    /// scoped to a folder containing the document (deepest first), then
    /// session-wide workspace entries, by language id then by extension;
    /// then the client's language id when it names an enabled
    /// non-modifier global entry (editors send `plaintext` for unknown
    /// extensions); then the extension match over the global entries,
    /// ties reported.
    pub fn resolve(&self, language_id: &str, uri: &str) -> Resolution<'_> {
        let fs_path = fs_path_of(uri);
        let ext = ext_of(uri);

        // Workspace candidates, most specific first: entries scoped to a
        // folder containing this document (deepest folder wins), then
        // unscoped session-wide entries. An unscoped entry still applies
        // to a non-file document (untitled:, vscode-notebook-cell:),
        // which a folder-scoped one never can.
        let usable = || self.workspace.iter().filter(|entry| entry.is_routable());
        let mut scoped: Vec<(&Entry, usize)> = match &fs_path {
            None => Vec::new(),
            Some(fs_path) => usable()
                .filter_map(|entry| {
                    let scope = scope_of(entry)?;
                    contains(scope, fs_path).then(|| (entry, js_length(scope)))
                })
                .collect(),
        };
        // Longest scope first, measured as `String(_scope).length`
        // measures it; the sort is stable, as Array.prototype.sort is, so
        // equal depths keep the order given.
        scoped.sort_by_key(|&(_, length)| std::cmp::Reverse(length));
        let candidates: Vec<&Entry> = scoped
            .into_iter()
            .map(|(entry, _)| entry)
            .chain(usable().filter(|entry| scope_of(entry).is_none()))
            .collect();

        if let Some(entry) = candidates
            .iter()
            .find(|entry| entry.language_id() == language_id)
        {
            return Resolution::found(entry, Via::WorkspaceLanguageId);
        }
        if let Some(ext) = &ext {
            if let Some(entry) = candidates
                .iter()
                .find(|entry| claims(&entry.extensions, ext))
            {
                return Resolution::found(entry, Via::WorkspaceExtension);
            }
        }

        if let Some(direct) = self.get(language_id) {
            if direct.is_routable() {
                return Resolution::found(direct, Via::LanguageId);
            }
        }

        let Some(ext) = ext else {
            return Resolution::none();
        };
        tie_break(
            &self.global,
            |entry| {
                entry
                    .extensions
                    .iter()
                    .filter(|x| x.to_lowercase() == ext)
                    .count()
            },
            Via::Extension,
        )
    }

    /// The global entry that claims a media type (`application/json`;
    /// parameters after `;` and letter case ignored), ties reported as
    /// [`Router::resolve`] reports them. A lookup for hosts that know a
    /// document's media type (a Content-Type, a MIME database) and not
    /// its editor language id; the language server never routes by it,
    /// and neither does the TypeScript registry, since an LSP client
    /// sends no media type. Workspace entries are folder-scoped and a
    /// media type has no folder, so only the global tiers answer.
    pub fn resolve_media_type(&self, media_type: &str) -> Resolution<'_> {
        let wanted = media_essence(media_type);
        if wanted.is_empty() {
            return Resolution::none();
        }
        tie_break(
            &self.global,
            |entry| {
                entry
                    .media_types
                    .iter()
                    .filter(|declared| media_essence(declared) == wanted)
                    .count()
            },
            Via::MediaType,
        )
    }

    /// [`Router::resolve`], and then, when nothing claims the document
    /// and the host knows its media type, [`Router::resolve_media_type`]:
    /// the whole routing order for a host that reads files from outside
    /// an editor (language id when it resolves, extension, media type).
    /// The language server never calls it, since an LSP client sends no
    /// media type, so the protocol's routing stays the TypeScript one.
    pub fn resolve_with_media_type(
        &self,
        language_id: &str,
        uri: &str,
        media_type: Option<&str>,
    ) -> Resolution<'_> {
        let resolution = self.resolve(language_id, uri);
        match media_type {
            Some(media_type) if resolution.entry.is_none() => self.resolve_media_type(media_type),
            _ => resolution,
        }
    }

    /// Every entry, global tier first, then the workspace tier: what
    /// `tabnas/status` lists and what hot reload walks.
    pub fn all(&self) -> impl Iterator<Item = &Entry> {
        self.global.iter().chain(self.workspace.iter())
    }

    /// The global tiers, bundled and user merged by language id, in
    /// first-declared order.
    pub fn global(&self) -> &[Entry] {
        &self.global
    }

    /// The workspace tier, in the order given.
    pub fn workspace(&self) -> &[Entry] {
        &self.workspace
    }

    /// Replace the workspace tier, keeping the global tiers: the half of
    /// hot reload a changed manifest triggers (`rebuildRegistry` builds a
    /// new registry from the same bundled and user tiers and the
    /// re-read workspace entries). The caller invalidates the instances
    /// of the old and the new workspace entries, as TypeScript does.
    pub fn set_workspace(&mut self, workspace: Vec<Entry>) {
        self.workspace = workspace;
    }

    /// The workspace entries a changed file reloads: those whose grammar
    /// or spec file ([`crate::loaders::watched_file`]) is one of
    /// `changed`. The half of hot reload a grammar-file change triggers
    /// (`onDidChangeWatchedFiles`); the caller invalidates each entry's
    /// instance and re-analyzes its documents, and the next make re-reads
    /// the file, since the loader caches nothing.
    pub fn reloaded_by(&self, changed: &[PathBuf]) -> Vec<&Entry> {
        self.all()
            .filter(|entry| {
                crate::loaders::watched_file(entry).is_some_and(|file| changed.contains(&file))
            })
            .collect()
    }
}

/// The routing scope of an entry: `_scope` when declared (`null` is
/// session-wide), else the sandbox folder, where an empty folder counts
/// as none (`e._dir || null`).
fn scope_of(entry: &Entry) -> Option<&Path> {
    match &entry.scope {
        Scope::Dir => entry
            .dir
            .as_deref()
            .filter(|dir| !dir.as_os_str().is_empty()),
        Scope::Session => None,
        Scope::Folder(folder) => Some(folder),
    }
}

/// A path's length as JavaScript counts a string's: UTF-16 code units.
fn js_length(path: &Path) -> usize {
    path.to_string_lossy().encode_utf16().count()
}

/// Whether an extension list claims `ext` (already lowercased).
fn claims(extensions: &[String], ext: &str) -> bool {
    extensions.iter().any(|x| x.to_lowercase() == ext)
}

/// A media type's essence: before any parameter, trimmed, lowercased.
fn media_essence(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// The canonical tie-break over the global entries: every enabled,
/// non-modifier entry, in order, once per claim it makes. The first
/// claimant wins; a claim by any OTHER entry is recorded (a later
/// duplicate claim by the winner is not, and one by a loser is recorded
/// again, as the TypeScript loop records it).
fn tie_break(entries: &[Entry], hits: impl Fn(&Entry) -> usize, via: Via) -> Resolution<'_> {
    let mut best: Option<&Entry> = None;
    let mut ambiguous: Vec<&Entry> = Vec::new();
    for entry in entries.iter().filter(|entry| entry.is_routable()) {
        for _ in 0..hits(entry) {
            match best {
                Some(winner) if !std::ptr::eq(winner, entry) => ambiguous.push(entry),
                _ => best = Some(entry),
            }
        }
    }
    let Some(best) = best else {
        return Resolution::none();
    };
    let mut resolution = Resolution::found(best, via);
    if !ambiguous.is_empty() {
        resolution.ambiguous = std::iter::once(best)
            .chain(ambiguous)
            .map(|entry| entry.language_id().to_string())
            .collect();
    }
    resolution
}

/// The lowercased extension of a URI or path (`.json`), `None` when the
/// last segment has none. `extOf` in `ts/src/registry.js`
/// (`/(\.[^./\\]+)$/`), `ExtOf` in `go/documents.go`.
pub fn ext_of(uri: &str) -> Option<String> {
    let dot = uri.rfind('.')?;
    let ext = &uri[dot + 1..];
    if ext.is_empty() || ext.contains(['/', '\\']) {
        return None;
    }
    Some(format!(".{}", ext.to_lowercase()))
}

/// The filesystem path of a `file:` URI, percent-decoded, with the
/// Windows drive form `/c:/dir` reduced to `c:/dir`; `None` for any
/// other scheme (`untitled:`, `vscode-notebook-cell:`), to which folder
/// scoping never applies. `fsPathOf` in `ts/src/registry.js`.
///
/// Two inputs the TypeScript pattern and decoder reject come back
/// `None` too: a line terminator after the authority (the pattern's
/// `.` never matches one), and a malformed escape or an escape that is
/// not UTF-8 (where `decodeURIComponent` throws).
pub fn fs_path_of(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    if path.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return None;
    }
    let mut path = decode_uri_component(path)?;
    // Windows drive form: /c:/dir -> c:/dir
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        path.remove(0);
    }
    Some(path)
}

/// `decodeURIComponent`: every `%XX` escape decoded, the bytes read as
/// UTF-8; `None` where JavaScript throws a `URIError`.
fn decode_uri_component(src: &str) -> Option<String> {
    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = bytes.get(at + 1..at + 3)?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Whether `fs_path` is `dir` or inside it. A Windows-shaped `dir` (a
/// drive letter or a UNC root) is compared with forward slashes, by the
/// path's shape and never by the host platform; a POSIX backslash is an
/// ordinary character. `contains` in `ts/src/registry.js`.
pub fn contains(dir: &Path, fs_path: &str) -> bool {
    let mut dir = dir.to_string_lossy().into_owned();
    if windowsy(&dir) {
        dir = dir.replace('\\', "/");
    }
    let dir = dir.trim_end_matches('/');
    fs_path == dir
        || fs_path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `/^([A-Za-z]:|\\\\)/`: a drive letter or a UNC root.
fn windowsy(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || path.starts_with("\\\\")
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

    #[test]
    fn ext_of_takes_the_last_segments_extension_lowercased() {
        assert_eq!(ext_of("file:///a/b/X.JSON").as_deref(), Some(".json"));
        assert_eq!(ext_of("untitled:Untitled-1.toml").as_deref(), Some(".toml"));
        assert_eq!(ext_of("file:///a/.bashrc").as_deref(), Some(".bashrc"));
        assert_eq!(ext_of("file:///a.d/b"), None);
        assert_eq!(ext_of("file:///a.d\\b"), None);
        assert_eq!(ext_of("file:///a/b."), None);
        assert_eq!(ext_of("noext"), None);
        assert_eq!(ext_of(""), None);
    }

    #[test]
    fn fs_path_of_decodes_file_uris_only() {
        assert_eq!(
            fs_path_of("file:///ws/a%20b/x.json").as_deref(),
            Some("/ws/a b/x.json")
        );
        // The authority is dropped; the path is what routing sees.
        assert_eq!(
            fs_path_of("file://host/share/x").as_deref(),
            Some("/share/x")
        );
        // Windows drive form, escaped or not.
        assert_eq!(
            fs_path_of("file:///c%3A/ws/x.toml").as_deref(),
            Some("c:/ws/x.toml")
        );
        assert_eq!(fs_path_of("file:///C:/ws").as_deref(), Some("C:/ws"));
        // Multi-byte escapes decode as UTF-8.
        assert_eq!(fs_path_of("file:///%C3%A9").as_deref(), Some("/\u{e9}"));
        assert_eq!(fs_path_of("untitled:Untitled-1"), None);
        assert_eq!(fs_path_of("vscode-notebook-cell:/x#1"), None);
        assert_eq!(fs_path_of("file:"), None);
        assert_eq!(fs_path_of("file://host"), None);
        // Where decodeURIComponent throws, and where the pattern's `.`
        // stops at a line terminator.
        assert_eq!(fs_path_of("file:///a%zz"), None);
        assert_eq!(fs_path_of("file:///a%4"), None);
        assert_eq!(fs_path_of("file:///a%+f"), None);
        assert_eq!(fs_path_of("file:///%C3"), None);
        assert_eq!(fs_path_of("file:///a\nb"), None);
        assert_eq!(fs_path_of("file:///a%0Ab").as_deref(), Some("/a\nb"));
    }

    #[test]
    fn contains_is_path_containment_by_shape() {
        let dir = Path::new("/ws/alpha");
        assert!(contains(dir, "/ws/alpha"));
        assert!(contains(dir, "/ws/alpha/x.json"));
        assert!(!contains(dir, "/ws/alphabet/x.json"));
        assert!(!contains(dir, "/ws"));
        assert!(contains(Path::new("/ws/alpha/"), "/ws/alpha/x"));
        assert!(contains(Path::new("/"), "/anything"));
        // Windows-shaped folders compare with forward slashes.
        assert!(contains(Path::new("c:\\ws\\alpha"), "c:/ws/alpha/x"));
        assert!(contains(Path::new("\\\\srv\\share"), "//srv/share/x"));
        // A POSIX backslash is an ordinary character.
        assert!(contains(Path::new("/work/a\\b"), "/work/a\\b/x"));
        assert!(!contains(Path::new("/work/a\\b"), "/work/a/b/x"));
    }

    fn global(name: &str, extensions: &[&str]) -> Entry {
        let mut entry = Entry::new(name);
        entry.extensions = extensions.iter().map(|x| x.to_string()).collect();
        entry
    }

    #[test]
    fn the_client_language_id_wins_only_when_it_is_routable() {
        let mut off = global("@tabnas/json", &[".json"]);
        off.enabled = Some(false);
        let mut modifier = global("@tabnas/debug", &[".dbg"]);
        modifier.plugin_kind = Some("modifier".into());
        let router = Router::new(
            vec![global("@tabnas/toml", &[".toml"]), off, modifier],
            vec![],
            vec![],
        );
        let r = router.resolve("toml", "file:///x/cargo.lock");
        assert_eq!(r.entry.map(Entry::language_id), Some("toml"));
        assert_eq!(r.via, Some(Via::LanguageId));
        // A generic id falls through to the extension.
        let r = router.resolve("plaintext", "file:///x/A.TOML");
        assert_eq!(r.entry.map(Entry::language_id), Some("toml"));
        assert_eq!(r.via.map(Via::as_str), Some("extension"));
        assert!(r.ambiguous.is_empty());
        // A disabled or modifier entry is never a target, by id or by
        // extension.
        assert_eq!(
            router.resolve("json", "file:///x/a.json"),
            Resolution::none()
        );
        assert_eq!(
            router.resolve("debug", "file:///x/a.dbg"),
            Resolution::none()
        );
        assert!(router.get("json").is_some());
        // No extension, no fallback.
        assert_eq!(
            router.resolve("plaintext", "file:///x/README"),
            Resolution::none()
        );
    }

    #[test]
    fn an_extension_claimed_twice_is_reported_not_silently_picked() {
        let router = Router::new(
            vec![
                global("@tabnas/a", &[".x"]),
                global("@tabnas/b", &[".y", ".X"]),
                global("@tabnas/c", &[".x", ".x"]),
            ],
            vec![],
            vec![],
        );
        let r = router.resolve("plaintext", "file:///doc.x");
        assert_eq!(r.entry.map(Entry::language_id), Some("a"));
        // Every claim by another entry is recorded, a repeated one twice,
        // as the canonical loop records it.
        assert_eq!(r.ambiguous, ["a", "b", "c", "c"]);
    }

    #[test]
    fn a_user_entry_replaces_the_bundled_one_in_its_place() {
        let mut user = global("@tabnas/a", &[".x"]);
        user.name = "mine".into();
        user.language_id = Some("a".into());
        let router = Router::new(
            vec![global("@tabnas/a", &[".x"]), global("@tabnas/b", &[".x"])],
            vec![],
            vec![user],
        );
        assert_eq!(router.get("a").map(|e| e.name.as_str()), Some("mine"));
        let r = router.resolve("plaintext", "file:///doc.x");
        // Still first: Map.set keeps the key's position.
        assert_eq!(r.entry.map(|e| e.name.as_str()), Some("mine"));
        assert_eq!(r.ambiguous, ["a", "b"]);
        assert_eq!(router.global().len(), 2);
        assert_eq!(router.all().count(), 2);
    }

    #[test]
    fn media_types_answer_a_host_lookup_with_ties_reported() {
        let mut json = global("@tabnas/json", &[".json"]);
        json.media_types = vec!["application/json".into()];
        let mut other = global("@tabnas/other", &[]);
        other.media_types = vec!["Application/JSON".into(), "text/x-other".into()];
        let router = Router::new(vec![json, other], vec![], vec![]);
        let r = router.resolve_media_type("application/json; charset=utf-8");
        assert_eq!(r.entry.map(Entry::language_id), Some("json"));
        assert_eq!(r.via, Some(Via::MediaType));
        assert_eq!(r.ambiguous, ["json", "other"]);
        let r = router.resolve_media_type(" TEXT/X-OTHER ");
        assert_eq!(r.entry.map(Entry::language_id), Some("other"));
        assert!(r.ambiguous.is_empty());
        assert_eq!(router.resolve_media_type(""), Resolution::none());
        assert_eq!(router.resolve_media_type("text/plain"), Resolution::none());
        assert_eq!(Via::MediaType.to_string(), "mediaType");
    }

    #[test]
    fn an_empty_sandbox_folder_is_no_scope() {
        // `_scope` defaults to `_dir || null`: an empty folder is none,
        // so the entry is session-wide rather than scoped to "".
        let mut entry = global("mydsl", &[".mydsl"]);
        entry.dir = Some(PathBuf::new());
        assert_eq!(scope_of(&entry), None);
        let router = Router::new(vec![], vec![entry], vec![]);
        let r = router.resolve("mydsl", "untitled:x");
        assert_eq!(r.via, Some(Via::WorkspaceLanguageId));
    }
}
