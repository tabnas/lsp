// Copyright (c) 2026 Richard Rodger, MIT License

//! Grammar loading: the dynamism ladder's L1, L2 and L3 lanes (design
//! §6) and the grammar firewall (design §10). Mirrors `ts/src/loaders.js`
//! (canonical) rule for rule; the Go port has only the L2 lane
//! (`EntryFromSpecJSON` in `go/lsp.go`).
//!
//! An entry's [`Load`](crate::types::Load) says how its grammar arrives:
//!
//! - `Module`, L1: live grammar code. In Rust that is a grammar crate
//!   LINKED into the binary and registered with [`Loader::link`] under
//!   its package name (`@tabnas/toml`; a short name falls back to
//!   `@tabnas/<name>`, as the TypeScript `require` fallback does). The
//!   `fleet` feature's `fleet()` links the bundled grammars. A workspace
//!   entry may load a module only when the host trusts workspace modules.
//! - `Spec`, L2: a serialized `GrammarSpec`, from a file (sandboxed to
//!   the entry's folder, read under [`MAX_GRAMMAR_BYTES`]) or inline,
//!   through [`firewall_spec`], then `GrammarSpec::from_value` and
//!   `Tabnas::grammar`.
//! - `Grammar`, L3: `.abnf`, `.ebnf` or `.gbnf` text, compiled to a pure
//!   spec by its dialect's crate (the `dialects` feature; three crates
//!   because they are three dialects, dispatched by extension), then the
//!   L2 lane.
//!
//! Grammar CODE is trusted like any dependency the host built in;
//! grammar DATA never is. The firewall runs before any engine load, on
//! the JSON as read, and refuses: the prototype-pollution keys
//! (`__proto__`, `constructor`, `prototype`) anywhere in the tree;
//! nesting deeper than [`MAX_GRAMMAR_DEPTH`]; a `ref` bag (live
//! functions are not JSON); `options.plugins`; a builtin schema `v`
//! above the engine's `BUILTIN_SCHEMA_VERSION`; more than
//! [`MAX_GRAMMAR_RULES`] rules or [`MAX_GRAMMAR_ALTS`] alternates in
//! total; and function references (`@name`) that are not `$`-suffixed
//! engine builtins, in options strings (where `@@…`, `@SKIP` and
//! serialized regexes are data) and in alt function positions
//! ([`ALT_FUNC_KEYS`], where every `@`-string is a reference). One
//! decision is the loaders agent's to make and record: the TypeScript
//! scan consults the engine's `BUILTIN_REFS` table, which the Rust
//! engine does not export (`is_builtin_action` is crate-private), so the
//! `@`-ref rule either takes the builtin list from an engine export
//! (a parser change) or accepts `$`-suffixed names and relies on the
//! engine's own load rejecting an unknown one; a Rust instance holds no
//! ref bag unless the host registered one, so nothing outside the
//! engine's builtins can be reached either way.
//!
//! Status: [`Dialect`], the caps and the [`Loader`] registry are
//! complete; the firewall, the sandbox, the read cap, the options
//! merge, the spec install, the dialect compile and
//! [`Loader::make_instance`] are signatures for the loaders module
//! agent, each refusal to be tested.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tabnas::{Options, Tabnas};

use crate::types::{Entry, Issue, LoadError, MakeInstance};

/// Caps bounding grammar LOAD cost (parse cost is bounded separately by
/// the parse deadline). `MAX_GRAMMAR_RULES` is ported from mcp; the
/// others exist because a rule-count cap alone lets one rule carry an
/// arbitrarily large alts array, an arbitrarily deep options tree or an
/// arbitrarily large file.
pub const MAX_GRAMMAR_RULES: usize = 5000;
pub const MAX_GRAMMAR_ALTS: usize = 10_000;
pub const MAX_GRAMMAR_DEPTH: usize = 100;
pub const MAX_GRAMMAR_BYTES: u64 = 1_000_000;

/// Keys refused anywhere in a spec.
pub const FORBIDDEN_KEYS: [&str; 3] = ["__proto__", "constructor", "prototype"];

/// Alt keys whose string values the engine resolves as function
/// references (grammar schema `$defs.alt`); `a` may be an array.
pub const ALT_FUNC_KEYS: [&str; 7] = ["b", "p", "r", "a", "e", "h", "c"];

/// The three BNF dialects, by grammar-file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dialect {
    Abnf,
    Ebnf,
    Gbnf,
}

impl Dialect {
    pub const ALL: [Dialect; 3] = [Dialect::Abnf, Dialect::Ebnf, Dialect::Gbnf];

    /// The grammar-file extension, `.abnf`.
    pub const fn extension(self) -> &'static str {
        match self {
            Dialect::Abnf => ".abnf",
            Dialect::Ebnf => ".ebnf",
            Dialect::Gbnf => ".gbnf",
        }
    }

    /// The dialect's package, `@tabnas/abnf`, as messages name it.
    pub const fn package(self) -> &'static str {
        match self {
            Dialect::Abnf => "@tabnas/abnf",
            Dialect::Ebnf => "@tabnas/ebnf",
            Dialect::Gbnf => "@tabnas/gbnf",
        }
    }

    /// The dialect a grammar file's extension names (case-insensitive),
    /// or the TypeScript refusal naming the extensions expected.
    pub fn of_file(file: &Path) -> Result<Dialect, LoadError> {
        let ext = file
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| format!(".{}", ext.to_ascii_lowercase()));
        if let Some(ext) = &ext {
            if let Some(dialect) = Dialect::ALL.iter().find(|d| d.extension() == ext) {
                return Ok(*dialect);
            }
        }
        let expected: Vec<&str> = Dialect::ALL.iter().map(|d| d.extension()).collect();
        Err(LoadError::new(format!(
            "unknown grammar dialect {} for {} -- expected one of: {}",
            ext.as_deref().unwrap_or("(no extension)"),
            file.display(),
            expected.join(", ")
        )))
    }
}

/// The full firewall over a candidate spec, as parsed JSON. Empty means
/// accepted; a poisoned tree (forbidden keys, excess depth) returns
/// those findings alone, since nothing below may touch it.
#[allow(unused_variables)] // stub
pub fn firewall_spec(spec: &serde_json::Value) -> Vec<Issue> {
    todo!("loaders::firewall_spec: ts/src/loaders.js firewallSpec")
}

/// Resolve a grammar file named by an entry against its sandbox folder,
/// refusing a path that escapes it lexically or through a symlink (the
/// check runs on REAL paths); a relative path with no folder is refused
/// too. `resolveSandboxed` in TypeScript.
#[allow(unused_variables)] // stub
pub fn resolve_sandboxed(file: &str, base_dir: Option<&Path>) -> Result<PathBuf, LoadError> {
    todo!("loaders::resolve_sandboxed")
}

/// Read a grammar file with [`MAX_GRAMMAR_BYTES`] applied to its size
/// BEFORE the content is parsed or compiled.
#[allow(unused_variables)] // stub
pub fn read_capped(file: &Path, entry: &Entry) -> Result<String, LoadError> {
    todo!("loaders::read_capped")
}

/// The engine options for an entry's instance: the entry's `options`
/// with recovery enabled underneath (a nested merge, so an entry that
/// sets any `parse` option keeps `recover.enabled`), and the entry's
/// `syncGroups` when the options set none. `NewInstance` in Go.
#[allow(unused_variables)] // stub
pub fn instance_options(entry: &Entry) -> Result<Options, LoadError> {
    todo!("loaders::instance_options")
}

/// Firewall a spec, then install it on a fresh instance with `options`
/// (and, for a `grammar` entry with a `base` or a `stack`, the linked
/// grammars it layers on, applied first: composition-aware).
#[allow(unused_variables)] // stub
pub fn install_spec(
    spec: serde_json::Value,
    entry: &Entry,
    options: Options,
    loader: &Loader,
) -> Result<Tabnas, LoadError> {
    todo!("loaders::install_spec")
}

/// Compile BNF-dialect grammar text to a pure spec: the dialect from the
/// file's extension, `builtins: true` explicitly (the default conversion
/// is closure mode and does not serialize), then the pure lowering that
/// strips compiler marks, stamps `v` and carries `meta.provenance`.
/// Without the `dialects` feature every dialect is refused as not
/// compiled in.
#[allow(unused_variables)] // stub
pub fn compile_grammar_text(file: &Path, src: &str) -> Result<serde_json::Value, LoadError> {
    let dialect = Dialect::of_file(file)?;
    #[cfg(not(feature = "dialects"))]
    {
        Err(LoadError::new(format!(
            "grammar dialect package not compiled in: {} (needed to compile {}): \
             build tabnas-lsp with the `dialects` feature",
            dialect.package(),
            file.display()
        )))
    }
    #[cfg(feature = "dialects")]
    {
        todo!("loaders::compile_grammar_text: {dialect:?} through its crate's convert and to_pure_spec")
    }
}

/// The versions of the dialect compilers built in.
#[cfg(feature = "dialects")]
pub fn dialect_versions() -> [(Dialect, &'static str); 3] {
    [
        (Dialect::Abnf, tabnas_abnf::VERSION),
        (Dialect::Ebnf, tabnas_ebnf::VERSION),
        (Dialect::Gbnf, tabnas_gbnf::VERSION),
    ]
}

/// A linked grammar: builds a bare instance with the grammar installed
/// (a fleet crate's `make()`); the loader applies the entry's options
/// afterwards.
pub type Factory = Arc<dyn Fn() -> Tabnas + Send + Sync>;

/// The binary's `MakeInstance` (`makeLoader` in TypeScript): the linked
/// grammars, the trust setting, and the dispatch on an entry's `load`.
#[derive(Clone, Default)]
pub struct Loader {
    linked: HashMap<String, Factory>,
    trust_workspace_modules: bool,
}

impl Loader {
    /// A loader with no grammars linked: it serves L2 and L3 entries
    /// only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a linked grammar under its package name (`@tabnas/json`)
    /// or any name entries will use in `load.module`.
    pub fn link(&mut self, name: impl Into<String>, factory: Factory) -> &mut Self {
        self.linked.insert(name.into(), factory);
        self
    }

    /// The linked grammar registered under `name`, or, for a short name,
    /// under `@tabnas/<name>`.
    pub fn linked(&self, name: &str) -> Option<&Factory> {
        self.linked.get(name).or_else(|| {
            if name.starts_with('@') {
                None
            } else {
                self.linked.get(&format!("@tabnas/{name}"))
            }
        })
    }

    /// The names every linked grammar is registered under, sorted.
    pub fn linked_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.linked.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Allow workspace entries to load modules (`trustWorkspaceModules`).
    pub fn set_trust_workspace_modules(&mut self, trust: bool) -> &mut Self {
        self.trust_workspace_modules = trust;
        self
    }

    pub fn trusts_workspace_modules(&self) -> bool {
        self.trust_workspace_modules
    }

    /// Build the instance for an entry: dispatch on [`Entry::load`], the
    /// three lanes above, with recovery enabled and the entry's options
    /// applied. A workspace entry's module load is refused unless
    /// trusted; an unlinked module is refused by name.
    #[allow(unused_variables)] // stub
    pub fn make_instance(&self, entry: &Entry) -> Result<Tabnas, LoadError> {
        todo!("loaders::Loader::make_instance: ts/src/loaders.js makeLoader")
    }

    /// This loader as the callback [`crate::Instances`] and
    /// [`crate::Config`] take.
    pub fn into_make_instance(self) -> MakeInstance {
        Arc::new(move |entry| self.make_instance(entry))
    }
}

impl std::fmt::Debug for Loader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loader")
            .field("linked", &self.linked_names())
            .field("trust_workspace_modules", &self.trust_workspace_modules)
            .finish()
    }
}

/// An entry plus its `MakeInstance` for a serialized `GrammarSpec`: the
/// L2 lane on its own, as a generated or embedded single-language server
/// uses it (`EntryFromSpecJSON` in `go/lsp.go`). The spec goes through
/// the firewall at every make.
#[allow(unused_variables)] // stub
pub fn entry_from_spec_json(
    language_id: &str,
    extensions: &[&str],
    spec: &str,
) -> (Entry, MakeInstance) {
    todo!("loaders::entry_from_spec_json")
}

/// The bundled grammars, linked in: every registry-routed grammar with a
/// Rust port, registered under its package name. The binary's L0 tier.
#[cfg(feature = "fleet")]
pub fn fleet() -> Loader {
    let mut loader = Loader::new();
    loader
        .link("@tabnas/csv", Arc::new(tabnas_csv::make))
        .link("@tabnas/feed", Arc::new(tabnas_feed::make))
        .link("@tabnas/ini", Arc::new(tabnas_ini::make))
        .link("@tabnas/json", Arc::new(tabnas_json::make))
        .link("@tabnas/json5", Arc::new(tabnas_json5::make))
        .link("@tabnas/jsonc", Arc::new(tabnas_jsonc::make))
        .link("@tabnas/jsonic", Arc::new(tabnas_jsonic::make))
        .link("@tabnas/jsonl", Arc::new(tabnas_jsonl::make))
        .link("@tabnas/toml", Arc::new(tabnas_toml::make))
        .link("@tabnas/xml", Arc::new(tabnas_xml::make))
        .link("@tabnas/yaml", Arc::new(tabnas_yaml::make))
        .link("@tabnas/zon", Arc::new(tabnas_zon::make));
    loader
}

/// The language ids `fleet()` links, in registry order.
#[cfg(feature = "fleet")]
pub const FLEET_LANGUAGE_IDS: [&str; 12] = [
    "csv", "feed", "ini", "json", "json5", "jsonc", "jsonic", "jsonl", "toml", "xml", "yaml", "zon",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialects_dispatch_on_the_extension() {
        assert_eq!(
            Dialect::of_file(Path::new("g.abnf")).unwrap(),
            Dialect::Abnf
        );
        assert_eq!(
            Dialect::of_file(Path::new("dir/G.EBNF")).unwrap(),
            Dialect::Ebnf
        );
        assert_eq!(
            Dialect::of_file(Path::new("g.gbnf")).unwrap(),
            Dialect::Gbnf
        );
        let error = Dialect::of_file(Path::new("g.bnf")).unwrap_err();
        assert!(error.message.contains(".bnf"), "{error}");
        assert!(error.message.contains(".abnf, .ebnf, .gbnf"), "{error}");
        let error = Dialect::of_file(Path::new("grammar")).unwrap_err();
        assert!(error.message.contains("(no extension)"), "{error}");
        assert_eq!(Dialect::Gbnf.package(), "@tabnas/gbnf");
    }

    #[test]
    fn linked_grammars_resolve_by_package_or_short_name() {
        let mut loader = Loader::new();
        assert!(loader.linked("json").is_none());
        loader.link("@tabnas/json", Arc::new(Tabnas::new));
        assert!(loader.linked("@tabnas/json").is_some());
        assert!(loader.linked("json").is_some());
        assert!(loader.linked("@json").is_none());
        assert_eq!(loader.linked_names(), ["@tabnas/json"]);
        assert!(!loader.trusts_workspace_modules());
        loader.set_trust_workspace_modules(true);
        assert!(loader.trusts_workspace_modules());
        assert!(format!("{loader:?}").contains("@tabnas/json"));
    }

    #[test]
    fn without_the_dialects_feature_a_grammar_file_is_refused_as_not_compiled_in() {
        if cfg!(feature = "dialects") {
            return;
        }
        let error = compile_grammar_text(Path::new("g.abnf"), "").unwrap_err();
        assert!(error.message.contains("@tabnas/abnf"), "{error}");
        assert!(error.message.contains("dialects"), "{error}");
        // An unknown extension is refused before the feature is consulted.
        let error = compile_grammar_text(Path::new("g.txt"), "").unwrap_err();
        assert!(error.message.contains("unknown grammar dialect"), "{error}");
    }

    #[cfg(feature = "fleet")]
    #[test]
    fn the_fleet_links_every_listed_grammar() {
        let loader = fleet();
        for id in FLEET_LANGUAGE_IDS {
            assert!(loader.linked(id).is_some(), "{id} is not linked");
        }
        assert_eq!(loader.linked_names().len(), FLEET_LANGUAGE_IDS.len());
        // A linked factory builds a parser with that grammar installed.
        let parser = loader.linked("json").unwrap()();
        assert!(parser.parse("[1,2]").is_ok());
    }

    #[cfg(feature = "dialects")]
    #[test]
    fn the_dialect_compilers_report_their_versions() {
        for (dialect, version) in dialect_versions() {
            assert!(!version.is_empty(), "{dialect:?}");
        }
    }
}
