// Copyright (c) 2026 Richard Rodger, MIT License

//! The types every module shares, in one place, so that the modules can
//! be built in parallel against a fixed contract: positions and the
//! encoding they are counted in, documents, diagnostics, registry
//! entries, the collector types one parse fills, the [`MakeInstance`]
//! callback a host supplies, the server [`Config`], and the load error.
//!
//! Each type names the TypeScript (canonical) and Go definitions it
//! mirrors. Behaviour lives in the owning module: [`crate::documents`]
//! implements [`Doc`]'s conversions, [`crate::instances`] fills a
//! [`Collected`], [`crate::registry`] routes over [`Entry`]. This file
//! holds definitions, derives, the constructors a definition implies and
//! the accessors that only normalize a field (the TypeScript `normalize`
//! defaults), and nothing that parses, converts or routes.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use tabnas::grammar::GrammarError;
use tabnas::Tabnas;

pub use crate::semantic::Overrides;
pub use crate::trace::TokenPoint;

// ---------------------------------------------------------------------
// Positions

/// An LSP position: 0-based line, 0-based character counted in the
/// document's negotiated [`PositionEncoding`] (UTF-16 code units by
/// default). Mirrors `{line, character}` in `ts/src/documents.js` and
/// `Position` in `go/documents.go`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Position {
    pub const fn new(line: u32, character: u32) -> Self {
        Position { line, character }
    }
}

/// An LSP range, `start` inclusive and `end` exclusive.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub const fn new(start: Position, end: Position) -> Self {
        Range { start, end }
    }

    /// The empty range at `at` (a `selectionRange`, a diagnostic at end
    /// of source).
    pub const fn empty(at: Position) -> Self {
        Range { start: at, end: at }
    }
}

/// The unit an LSP `character` is counted in (design §9), negotiated at
/// `initialize`: UTF-16 code units are the protocol default and what
/// the TypeScript server speaks; UTF-8 bytes are used when the client
/// offers them (`general.positionEncodings`). Every conversion from the
/// engine's units (rows, columns in Unicode scalar values, byte offsets,
/// `len` in scalar values) goes through the document text, in
/// [`crate::documents`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PositionEncoding {
    #[default]
    Utf16,
    Utf8,
}

impl PositionEncoding {
    /// The `positionEncoding` wire name: `utf-16` or `utf-8`.
    pub const fn as_str(self) -> &'static str {
        match self {
            PositionEncoding::Utf16 => "utf-16",
            PositionEncoding::Utf8 => "utf-8",
        }
    }

    /// The encoding a wire name denotes. `utf-32`, which this server does
    /// not offer, and anything else are `None`.
    pub fn from_wire(name: &str) -> Option<Self> {
        match name {
            "utf-16" => Some(PositionEncoding::Utf16),
            "utf-8" => Some(PositionEncoding::Utf8),
            _ => None,
        }
    }
}

impl fmt::Display for PositionEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------
// Diagnostics

/// `DiagnosticSeverity.Error`, the only severity the pipeline emits.
pub const SEVERITY_ERROR: u32 = 1;

/// The error registry a diagnostic's `codeDescription.href` points into,
/// for every code but `unknown` (`ts/src/core.js` `diagnostics`).
pub const ERROR_REGISTRY: &str = "https://tabnas.dev/errors/";

/// An LSP diagnostic, as `ts/src/core.js` `diagnostics` and
/// `go/core.go` `Diagnostic` build it from one engine error: the range
/// converted through the document text, severity 1, the engine's
/// `code`, `source` `tabnas:<languageId>`, the message with the hint
/// appended after a blank line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub range: Range,
    pub severity: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub source: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_description: Option<CodeDescription>,
}

/// `codeDescription`: where the code is documented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeDescription {
    pub href: String,
}

// ---------------------------------------------------------------------
// Documents

/// One open document (`Doc` in `ts/src/documents.js` and
/// `go/documents.go`). The text is the source of truth for every
/// position conversion; [`crate::documents`] implements them on this
/// type, and the line index is built there on first use.
#[derive(Debug, Clone)]
pub struct Doc {
    pub uri: String,
    pub language_id: String,
    pub version: i64,
    pub text: String,
    /// The encoding the client's positions are counted in.
    pub encoding: PositionEncoding,
    /// Byte offset of each line start, built lazily by
    /// `Doc::line_starts` and reset by `Doc::update`.
    pub(crate) line_starts: OnceCell<Vec<usize>>,
}

impl Doc {
    /// A document in the default encoding, UTF-16.
    pub fn new(
        uri: impl Into<String>,
        language_id: impl Into<String>,
        version: i64,
        text: impl Into<String>,
    ) -> Doc {
        Doc {
            uri: uri.into(),
            language_id: language_id.into(),
            version,
            text: text.into(),
            encoding: PositionEncoding::Utf16,
            line_starts: OnceCell::new(),
        }
    }

    /// The same document with its positions counted in `encoding`.
    pub fn with_encoding(mut self, encoding: PositionEncoding) -> Doc {
        self.encoding = encoding;
        self
    }
}

// ---------------------------------------------------------------------
// What one parse collects

/// Which pass of a rule a [`RuleEvent`] closes: `o` or `c` in the
/// TypeScript collector, `RuleDone.State` in Go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleEventState {
    Open,
    Close,
}

impl From<tabnas::rule::RuleState> for RuleEventState {
    fn from(state: tabnas::rule::RuleState) -> Self {
        match state {
            tabnas::rule::RuleState::Open => RuleEventState::Open,
            tabnas::rule::RuleState::Close => RuleEventState::Close,
        }
    }
}

/// The slice of one `ruleDone` event the pipeline keeps (`ruleEvents`
/// in `ts/src/core.js` `analyze`, `ruleEvent` in `go/core.go`): the
/// rule's index and name, the pass, whether recovery synthesized the
/// close (`forced`), the alternate's `r` (replace) name, and the first
/// matched token of the open and close passes when the pass matched
/// any. Positions are the engine's, in [`TokenPoint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleEvent {
    pub i: usize,
    pub name: String,
    pub state: RuleEventState,
    pub forced: bool,
    pub r: String,
    pub o0: Option<TokenPoint>,
    pub c0: Option<TokenPoint>,
}

/// Everything the mux collects during one parse: the lex trace (every
/// token event, retractions and trivia included, reconciled later by
/// [`crate::trace::reconcile`]) and the rule events. `collector` in
/// `ts/src/core.js` and `go/core.go`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collected {
    pub lex: Vec<TokenPoint>,
    pub rules: Vec<RuleEvent>,
}

// ---------------------------------------------------------------------
// Making instances

/// Builds (or rebuilds, on reload) the engine instance for an entry:
/// `makeInstance(entry)` in `ts/src/instances.js`, `MakeInstance` in
/// `go/lsp.go`. The host decides what a language id means: the binary's
/// loader dispatches on the entry's `load` (a linked fleet grammar, an
/// L2 spec, an L3 grammar file); a host such as `aless` supplies its own
/// parsers. The instance comes back with recovery enabled and NO
/// subscribers of the callback's own for the pipeline's events:
/// [`crate::instances::Instances`] installs the one permanent mux pair
/// on whatever this returns, and the engine has no unsubscribe.
pub type MakeInstance = Arc<dyn Fn(&Entry) -> Result<Tabnas, LoadError> + Send + Sync>;

// ---------------------------------------------------------------------
// Errors

/// One thing the grammar firewall refused, at a JSON path into the spec
/// (`issues` on the TypeScript `LoadError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub path: String,
    pub message: String,
}

/// A grammar could not be loaded: the firewall refused it, a file was
/// unreadable or escaped its workspace folder, a dialect is not compiled
/// in, the engine rejected the spec, a module is not linked. `LoadError`
/// in `ts/src/loaders.js`; the `error` return of Go's `MakeInstance`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub message: String,
    pub issues: Vec<Issue>,
}

impl LoadError {
    pub fn new(message: impl Into<String>) -> Self {
        LoadError {
            message: message.into(),
            issues: Vec::new(),
        }
    }

    /// A refusal carrying the firewall's findings.
    pub fn with_issues(message: impl Into<String>, issues: Vec<Issue>) -> Self {
        LoadError {
            message: message.into(),
            issues,
        }
    }
}

impl fmt::Display for LoadError {
    /// The TypeScript form: the message, then one indented
    /// `path: message` line per issue.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        for issue in &self.issues {
            write!(f, "\n  {}: {}", issue.path, issue.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for LoadError {}

impl From<GrammarError> for LoadError {
    fn from(error: GrammarError) -> Self {
        LoadError::new(error.0)
    }
}

// ---------------------------------------------------------------------
// Registry entries

/// Which configuration tier supplied an entry (`_source` in
/// `ts/src/registry.js`): the bundled fleet registry, user settings, or
/// a workspace (`initializationOptions.languages` or a folder's
/// `.tabnas/lsp.json`). Workspace entries are the untrusted tier: their
/// module loads need `trustWorkspaceModules`, and their file paths are
/// sandboxed to their folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntrySource {
    #[default]
    Bundled,
    User,
    Workspace,
}

impl EntrySource {
    pub const fn as_str(self) -> &'static str {
        match self {
            EntrySource::Bundled => "bundled",
            EntrySource::User => "user",
            EntrySource::Workspace => "workspace",
        }
    }
}

/// How an entry's grammar arrives, the dynamism ladder's lanes (design
/// §6; `load` in `ts/src/loaders.js`):
///
/// - `{"module": "@tabnas/toml"}`, L1: live grammar code. In Rust a
///   grammar crate linked into the binary, registered by name.
/// - `{"spec": "./grammar.json"}` or `{"spec": {...}}`, L2: a
///   serialized `GrammarSpec`, always through the firewall.
/// - `{"grammar": "./my.abnf"}`, L3: BNF-dialect text, compiled to a
///   pure spec by its dialect's crate, then the L2 firewall.
///
/// Read as the canonical loader reads `load` ([`Load::from_value`]): a
/// non-null `spec` first, else a non-null `grammar`, else the module,
/// with any other key ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load {
    Module(String),
    Spec(SpecSource),
    Grammar(String),
}

impl Load {
    /// The lane a `load` object names, in the canonical order
    /// (`ts/src/loaders.js`: `null != load.spec`, then
    /// `null != load.grammar`, then `load.module`): a non-null `spec` is
    /// L2, a file when it is a string and inline otherwise; else a
    /// non-null `grammar` string is L3; else L1, the module `module`
    /// names, or the entry's own name (an empty module here) when it is
    /// absent or null. Other keys are ignored. A `load` that is not an
    /// object, or a `grammar` or `module` that is not a string, is
    /// refused, where the canonical loader would read the value as a
    /// path and fail later.
    pub fn from_value(value: serde_json::Value) -> Result<Load, String> {
        use serde_json::Value;
        let Value::Object(mut fields) = value else {
            return Err(format!(
                "invalid value: {}, expected an object naming spec, grammar or module",
                json_kind(&value)
            ));
        };
        match fields.remove("spec") {
            None | Some(Value::Null) => {}
            Some(Value::String(file)) => return Ok(Load::Spec(SpecSource::File(file))),
            Some(inline) => return Ok(Load::Spec(SpecSource::Inline(inline))),
        }
        match fields.remove("grammar") {
            None | Some(Value::Null) => {}
            Some(Value::String(file)) => return Ok(Load::Grammar(file)),
            Some(other) => {
                return Err(format!(
                    "invalid value: {}, expected a string for grammar",
                    json_kind(&other)
                ))
            }
        }
        match fields.remove("module") {
            None | Some(Value::Null) => Ok(Load::Module(String::new())),
            Some(Value::String(name)) => Ok(Load::Module(name)),
            Some(other) => Err(format!(
                "invalid value: {}, expected a string for module",
                json_kind(&other)
            )),
        }
    }
}

impl<'de> Deserialize<'de> for Load {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Load::from_value(value).map_err(serde::de::Error::custom)
    }
}

/// A JSON value's kind, for a message.
fn json_kind(value: &serde_json::Value) -> &'static str {
    use serde_json::Value;
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "map",
    }
}

/// An L2 spec: a file path (relative to the entry's sandbox folder) or
/// the spec inline.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum SpecSource {
    File(String),
    Inline(serde_json::Value),
}

/// An entry's ROUTING scope (`_scope` in `ts/src/registry.js`), kept
/// apart from its sandbox folder: the folder whose documents it serves,
/// `Session` for everywhere, or `Dir` to follow the sandbox folder
/// ([`Entry::dir`]), which is what a folder manifest's entries do. In
/// JSON an absent `_scope` is `Dir`, an explicit `null` is `Session`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    Dir,
    Session,
    Folder(PathBuf),
}

impl<'de> Deserialize<'de> for Scope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Option::<String>::deserialize(deserializer)? {
            None => Scope::Session,
            Some(folder) => Scope::Folder(PathBuf::from(folder)),
        })
    }
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

/// A registry entry: one served language, with the descriptor's fields
/// as written (`ts/data/registry.json`, a workspace manifest, or a
/// host's own construction). `normalize` in `ts/src/registry.js` is the
/// canonical field list and this struct's accessors apply its defaults;
/// `Entry` in `go/lsp.go` is the Go mirror. Use the accessors for the
/// normalized values and the fields to construct or inspect.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// The plugin package name, `@tabnas/json`, or a host's own name.
    pub name: String,
    /// The editor language id, when declared; see [`Entry::language_id`].
    #[serde(default)]
    pub language_id: Option<String>,
    #[serde(default, deserialize_with = "null_is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, deserialize_with = "null_is_empty")]
    pub media_types: Vec<String>,
    /// The grammar this one layers on (`@tabnas/jsonic` under `toml`),
    /// applied host-side for `grammar` entries only: a compiler's base
    /// is a library dependency and is not applied.
    #[serde(default)]
    pub base: Option<String>,
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
    /// Rule name to symbol label, over the defaults (`map` Object,
    /// `list` Array).
    #[serde(default)]
    pub outline_rules: Option<HashMap<String, String>>,
    /// Recovery sync groups; replaces the engine's default when present.
    #[serde(default)]
    pub sync_groups: Option<Vec<String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default, deserialize_with = "null_is_empty")]
    pub error_codes: Vec<String>,
    /// How to load the grammar; see [`Entry::load`] for the default.
    #[serde(default)]
    pub load: Option<Load>,
    /// Engine options for the instance, as serialized options JSON.
    #[serde(default)]
    pub options: Option<serde_json::Value>,
    /// An explicit plugin stack, overriding `base` + `name`.
    #[serde(default)]
    pub stack: Option<Vec<String>>,
    /// Provenance, stamped by the host.
    #[serde(default, rename = "_source")]
    pub source: EntrySource,
    /// The SANDBOX folder: what a workspace entry's relative grammar
    /// paths resolve against, and part of the instance cache key.
    #[serde(default, rename = "_dir")]
    pub dir: Option<PathBuf>,
    /// The ROUTING scope; see [`Scope`].
    #[serde(default, rename = "_scope")]
    pub scope: Scope,
}

impl Entry {
    /// An entry with only its name set: every other field takes the
    /// TypeScript default through the accessors.
    pub fn new(name: impl Into<String>) -> Entry {
        Entry {
            name: name.into(),
            language_id: None,
            extensions: Vec::new(),
            media_types: Vec::new(),
            base: None,
            plugin_kind: None,
            grammar_kind: None,
            lex_stream: None,
            semantic_tokens: None,
            outline_rules: None,
            sync_groups: None,
            enabled: None,
            error_codes: Vec::new(),
            load: None,
            options: None,
            stack: None,
            source: EntrySource::Bundled,
            dir: None,
            scope: Scope::Dir,
        }
    }

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

    /// `closure` when the descriptor is silent (the TypeScript default).
    pub fn grammar_kind(&self) -> &str {
        declared(&self.grammar_kind).unwrap_or("closure")
    }

    /// Enabled unless the descriptor says `false` (the editor-collision
    /// policy the generator applies).
    pub fn is_enabled(&self) -> bool {
        self.enabled != Some(false)
    }

    /// Whether routing may pick this entry at all: enabled and not a
    /// modifier (a modifier is never a routing target).
    pub fn is_routable(&self) -> bool {
        self.is_enabled() && self.plugin_kind() != "modifier"
    }

    /// How to load the grammar: the declared `load`, else the L1 module
    /// named by the entry (`load || {module: name}`).
    pub fn load(&self) -> Load {
        self.load
            .clone()
            .unwrap_or_else(|| Load::Module(self.name.clone()))
    }

    /// The folder whose documents this entry serves, if it is scoped to
    /// one: [`Scope::Folder`], or the sandbox folder under [`Scope::Dir`].
    /// `None` is session-wide.
    pub fn routing_scope(&self) -> Option<&Path> {
        match &self.scope {
            Scope::Dir => self.dir.as_deref(),
            Scope::Session => None,
            Scope::Folder(folder) => Some(folder),
        }
    }

    /// Whether the untrusted, workspace tier supplied this entry.
    pub fn is_workspace(&self) -> bool {
        self.source == EntrySource::Workspace
    }
}

// ---------------------------------------------------------------------
// Server configuration

/// What a server serves and how (`Config` in `go/server.go`; the
/// `startServer` options and `initializationOptions` in
/// `ts/src/server.js`). Built by the binary from the bundled registry
/// and its loader, or by a host from its own entries and
/// [`MakeInstance`].
pub struct Config {
    /// The bundled tier: the languages served. A generated or embedded
    /// server passes exactly the list it serves.
    pub entries: Vec<Entry>,
    /// The user tier, keyed by language id over the bundled one.
    pub user_entries: Vec<Entry>,
    /// The workspace tier the host already knows; the server adds what
    /// `initializationOptions.languages` and folder manifests declare.
    pub workspace_entries: Vec<Entry>,
    /// Builds the engine instance for an entry.
    pub make_instance: MakeInstance,
    /// Allow workspace entries to load L1 modules (`trustWorkspaceModules`
    /// in `initializationOptions` sets it too). Off: a manifest must not
    /// run code just by being opened.
    pub trust_workspace_modules: bool,
    /// How long after the last change a document is re-analyzed
    /// (`DEBOUNCE_MS`, 150 ms).
    pub debounce: Duration,
    /// Refuse to analyze a document larger than this many bytes (design
    /// §10). `None` is no limit, the TypeScript server's behaviour today.
    pub max_document_bytes: Option<usize>,
    /// Stop a parse that runs longer than this, through the engine's
    /// `parse_budget` (design §10). `None` is no deadline.
    pub parse_deadline: Option<Duration>,
    /// `serverInfo.name` in the `initialize` response.
    pub server_name: String,
}

impl Config {
    /// A configuration serving nothing yet, with the TypeScript server's
    /// defaults: no trust in workspace modules, the 150 ms debounce, no
    /// document or parse limit.
    pub fn new(make_instance: MakeInstance) -> Config {
        Config {
            entries: Vec::new(),
            user_entries: Vec::new(),
            workspace_entries: Vec::new(),
            make_instance,
            trust_workspace_modules: false,
            debounce: Duration::from_millis(crate::server::DEBOUNCE_MS),
            max_document_bytes: None,
            parse_deadline: None,
            server_name: "tabnas-lsp".to_string(),
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("entries", &self.entries.len())
            .field("user_entries", &self.user_entries.len())
            .field("workspace_entries", &self.workspace_entries.len())
            .field("trust_workspace_modules", &self.trust_workspace_modules)
            .field("debounce", &self.debounce)
            .field("max_document_bytes", &self.max_document_bytes)
            .field("parse_deadline", &self.parse_deadline)
            .field("server_name", &self.server_name)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_takes_the_typescript_defaults() {
        let entry = Entry::new("@tabnas/x");
        assert_eq!(entry.language_id(), "x");
        assert_eq!(entry.lex_stream(), "clean");
        assert_eq!(entry.plugin_kind(), "grammar");
        assert_eq!(entry.grammar_kind(), "closure");
        assert!(entry.is_enabled());
        assert!(entry.is_routable());
        assert_eq!(entry.load(), Load::Module("@tabnas/x".into()));
        assert_eq!(entry.routing_scope(), None);
        assert_eq!(entry.source, EntrySource::Bundled);
        assert!(!entry.is_workspace());
    }

    #[test]
    fn load_and_scope_read_the_manifest_shapes() {
        let src = r#"{"name": "mydsl", "languageId": "mydsl", "extensions": [".mydsl"],
            "load": {"grammar": "./grammar/mydsl.abnf"},
            "_source": "workspace", "_dir": "/ws/a"}"#;
        let entry: Entry = serde_json::from_str(src).unwrap();
        assert_eq!(entry.load(), Load::Grammar("./grammar/mydsl.abnf".into()));
        assert!(entry.is_workspace());
        // An absent _scope follows the sandbox folder.
        assert_eq!(entry.routing_scope(), Some(Path::new("/ws/a")));

        let src =
            r#"{"name": "s", "load": {"spec": {"rule": {}}}, "_dir": "/ws/a", "_scope": null}"#;
        let entry: Entry = serde_json::from_str(src).unwrap();
        assert!(matches!(entry.load(), Load::Spec(SpecSource::Inline(_))));
        // An explicit null is session-wide.
        assert_eq!(entry.scope, Scope::Session);
        assert_eq!(entry.routing_scope(), None);

        let src = r#"{"name": "f", "load": {"spec": "./g.json"}, "_scope": "/ws/b"}"#;
        let entry: Entry = serde_json::from_str(src).unwrap();
        assert_eq!(
            entry.load(),
            Load::Spec(SpecSource::File("./g.json".into()))
        );
        assert_eq!(entry.routing_scope(), Some(Path::new("/ws/b")));
        let src = r#"{"name": "m", "load": {"module": "@tabnas/toml"}, "pluginKind": "modifier"}"#;
        let entry: Entry = serde_json::from_str(src).unwrap();
        assert_eq!(entry.load(), Load::Module("@tabnas/toml".into()));
        assert!(!entry.is_routable());
    }

    #[test]
    fn a_load_error_prints_its_issues_the_typescript_way() {
        let error = LoadError::with_issues(
            "grammar for x failed the firewall",
            vec![Issue {
                path: "$.ref".into(),
                message: "refused".into(),
            }],
        );
        assert_eq!(
            error.to_string(),
            "grammar for x failed the firewall\n  $.ref: refused"
        );
        assert_eq!(LoadError::new("plain").to_string(), "plain");
        let from_engine: LoadError = GrammarError("bad".into()).into();
        assert_eq!(from_engine.message, "bad");
    }

    #[test]
    fn encodings_round_trip_their_wire_names() {
        for encoding in [PositionEncoding::Utf16, PositionEncoding::Utf8] {
            assert_eq!(
                PositionEncoding::from_wire(encoding.as_str()),
                Some(encoding)
            );
        }
        assert_eq!(PositionEncoding::from_wire("utf-32"), None);
        assert_eq!(PositionEncoding::default(), PositionEncoding::Utf16);
    }

    #[test]
    fn a_diagnostic_serializes_in_the_lsp_shape() {
        let diagnostic = Diagnostic {
            range: Range::new(Position::new(0, 10), Position::new(0, 14)),
            severity: SEVERITY_ERROR,
            code: Some("unexpected".into()),
            source: "tabnas:jsonf".into(),
            message: "m\n\nh".into(),
            code_description: Some(CodeDescription {
                href: format!("{ERROR_REGISTRY}unexpected"),
            }),
        };
        let json = serde_json::to_value(&diagnostic).unwrap();
        assert_eq!(json["range"]["start"]["character"], 10);
        assert_eq!(
            json["codeDescription"]["href"],
            "https://tabnas.dev/errors/unexpected"
        );
        assert_eq!(json["severity"], 1);
        let back: Diagnostic = serde_json::from_value(json).unwrap();
        assert_eq!(back, diagnostic);
    }
}
