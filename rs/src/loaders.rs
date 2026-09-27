// Copyright (c) 2026 Richard Rodger, MIT License

//! Grammar loading: the dynamism ladder's L1, L2 and L3 lanes (design
//! §6) and the grammar firewall (design §10). Mirrors `ts/src/loaders.js`
//! (canonical) rule for rule; the Go port has only the L2 lane
//! (`EntryFromSpecJSON` in `go/lsp.go`, [`entry_from_spec_json`] here).
//!
//! An entry's [`Load`] says how its grammar arrives:
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
//! ([`FORBIDDEN_KEYS`]) anywhere in the tree; nesting deeper than
//! [`MAX_GRAMMAR_DEPTH`]; a `ref` bag (live functions are not JSON);
//! `options.plugins`; a builtin schema `v` above the engine's
//! `BUILTIN_SCHEMA_VERSION`; more than [`MAX_GRAMMAR_RULES`] rules or
//! [`MAX_GRAMMAR_ALTS`] alternates in total; and function references
//! (`@name`) that are not `$`-suffixed engine builtins, in options
//! strings (where `@@…`, `@SKIP` and serialized regexes are data) and in
//! alt function positions ([`ALT_FUNC_KEYS`], where every `@`-string is
//! a reference). The limits and the messages are the TypeScript ones.
//!
//! The builtin set is the ENGINE'S, never a copy, as TypeScript takes it
//! from the engine's `BUILTIN_REFS`. The Rust engine does not export
//! that table (its `is_builtin_action` is crate-private), so the
//! firewall asks the engine directly: a `$`-suffixed name is a builtin
//! when a bare engine, which has no function references registered,
//! accepts it as an alternate's action or condition
//! ([`is_builtin_ref`]). That is exactly the canonical set (the thirteen
//! `$` actions and the three `@probePhase…$` conditions), and it moves
//! with the engine.
//!
//! What differs from TypeScript, because Rust has no `require`:
//!
//! - A linked grammar is COMPLETE: its crate's `make()` composes its own
//!   base chain (`tabnas_toml::make()` is `tabnas_jsonic::make()` plus
//!   the TOML plugin), so the host does not re-apply an entry's `base`.
//!   The layers an entry declares (its `base` for a `grammar` entry and
//!   its `stack`) must all be linked, as each must be `require`-able in
//!   TypeScript, and the instance is built by the LAST one's factory.
//! - Entry `options` are applied through the engine's serialized-options
//!   path (the one a spec's `options` take), after the factory, where
//!   TypeScript passes them to the constructor before the plugins run.
//!   That path resolves `@`-references, so entry options pass the same
//!   firewall a spec's options do.
//! - An L1 entry loads the module its `load` names (`load.module`, else
//!   the entry's name). The TypeScript loader names the module in its
//!   trust refusal but builds its stack from the entry's name, so a
//!   `load.module` that differs from the name is ignored there: reported
//!   as a TypeScript defect rather than reproduced.
//! - L3 lowers every dialect's output through the pure-spec lowering.
//!   TypeScript lowers only when the dialect package exports
//!   `toPureSpec`, which `@tabnas/abnf` does and `@tabnas/ebnf` and
//!   `@tabnas/gbnf` do not, so their converted spec keeps an empty
//!   `ref` bag and the firewall refuses every `.ebnf` and `.gbnf` file:
//!   reported as a TypeScript defect rather than reproduced.
//!
//! Hot reload is the server's: a changed grammar file names the entries
//! to rebuild ([`watched_file`], [`crate::registry::Router::reloaded_by`]),
//! and rebuilding is invalidating the cached instance and making it
//! again, since the loader caches nothing and re-reads every file at
//! every make.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde_json::{Map, Value};
use tabnas::grammar::BUILTIN_SCHEMA_VERSION;
use tabnas::{GrammarSpec, Tabnas};

use crate::types::{Entry, Issue, Load, LoadError, MakeInstance, SpecSource};

/// Caps bounding grammar LOAD cost (parse cost is bounded separately by
/// the parse deadline). `MAX_GRAMMAR_RULES` is ported from mcp; the
/// others exist because a rule-count cap alone lets one rule carry an
/// arbitrarily large alts array, an arbitrarily deep options tree (deep
/// enough to overflow the recursive scans) or an arbitrarily large file.
pub const MAX_GRAMMAR_RULES: usize = 5000;
pub const MAX_GRAMMAR_ALTS: usize = 10_000;
pub const MAX_GRAMMAR_DEPTH: usize = 100;
pub const MAX_GRAMMAR_BYTES: u64 = 1_000_000;

/// The options scan stops collecting once it holds more than this many
/// findings (`100 < out.length` in TypeScript).
const MAX_OPTIONS_ISSUES: usize = 100;

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
            .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()));
        if let Some(ext) = &ext {
            if let Some(dialect) = Dialect::ALL.iter().find(|d| d.extension() == ext) {
                return Ok(*dialect);
            }
        }
        let expected: Vec<&str> = Dialect::ALL.iter().map(|d| d.extension()).collect();
        Err(LoadError::new(format!(
            "unknown grammar dialect {} for {} \u{2014} expected one of: {}",
            ext.as_deref().unwrap_or("(no extension)"),
            file.display(),
            expected.join(", ")
        )))
    }
}

// ---------------------------------------------------------------------
// The grammar firewall, ported from mcp by way of ts/src/loaders.js
// (design §10). Structural junk beyond these rules is caught loudly by
// the trial load, since the engine rejects malformed specs, so no schema
// pass is duplicated here.

fn issue(path: impl Into<String>, message: impl Into<String>) -> Issue {
    Issue {
        path: path.into(),
        message: message.into(),
    }
}

/// The full firewall over a candidate spec, as parsed JSON. Empty means
/// accepted; a poisoned tree (forbidden keys, excess depth) returns
/// those findings alone, since nothing below may touch it.
pub fn firewall_spec(spec: &Value) -> Vec<Issue> {
    let Value::Object(gs) = spec else {
        return vec![issue(
            "$",
            "grammar must be a JSON object (the serialized GrammarSpec form)",
        )];
    };
    let mut out = Vec::new();
    scan_forbidden_keys(spec, "$", &mut out, 0);
    if !out.is_empty() {
        return out; // poisoned: nothing below touches it
    }

    if gs.contains_key("ref") {
        out.push(issue(
            "$.ref",
            "'ref' is not part of the serialized grammar form: live functions are not JSON. \
             Name $-suffixed engine builtins instead.",
        ));
    }
    if let Some(Value::Object(options)) = gs.get("options") {
        if options.contains_key("plugins") {
            out.push(issue(
                "$.options.plugins",
                "plugins cannot be supplied through a serialized grammar: a plugin is live \
                 code, and this lane accepts only data",
            ));
        }
    }

    // `'number' === typeof gs.v ? gs.v : 1`: anything but a number is 1.
    if let Some(Value::Number(v)) = gs.get("v") {
        if v.as_f64()
            .is_some_and(|v| v > BUILTIN_SCHEMA_VERSION as f64)
        {
            out.push(issue(
                "$.v",
                format!(
                    "grammar declares builtin schema version {}; this engine supports up to {}",
                    js_number(v),
                    BUILTIN_SCHEMA_VERSION
                ),
            ));
        }
    }

    let mut builtins = BuiltinRefs::default();
    if let Some(options @ (Value::Object(_) | Value::Array(_))) = gs.get("options") {
        scan_options_refs(options, "$.options", &mut out, 0, &mut builtins);
    }

    // `Object.keys(gs.rule)`: an object's rules, or an array's indices.
    let rules: Vec<(String, &Value)> = match gs.get("rule") {
        Some(Value::Object(rules)) => rules.iter().map(|(k, v)| (k.clone(), v)).collect(),
        Some(Value::Array(rules)) => rules
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        _ => return out,
    };
    if rules.len() > MAX_GRAMMAR_RULES {
        out.push(issue(
            "$.rule",
            format!(
                "grammar defines {} rules, more than {MAX_GRAMMAR_RULES}",
                rules.len()
            ),
        ));
    }
    // Total alternates are capped as well: one rule can carry an
    // arbitrarily large alts array, and the rule-count cap alone would
    // wave it through.
    let mut alt_count = 0;
    for (rule_name, rule_spec) in rules {
        let Value::Object(rule_spec) = rule_spec else {
            continue;
        };
        for state in ["open", "close"] {
            let alts = alts_of(rule_spec.get(state));
            alt_count += alts.len();
            for (i, alt) in alts.iter().enumerate() {
                scan_alt_refs(
                    alt,
                    &format!("$.rule.{rule_name}.{state}[{i}]"),
                    &mut out,
                    &mut builtins,
                );
            }
        }
        if alt_count > MAX_GRAMMAR_ALTS {
            break;
        }
    }
    if alt_count > MAX_GRAMMAR_ALTS {
        out.push(issue(
            "$.rule",
            format!("grammar defines more than {MAX_GRAMMAR_ALTS} alternates in total"),
        ));
    }
    out
}

/// A JSON number as JavaScript prints it where the two can differ on a
/// plausible input: an integral float (`6.0`) prints as an integer, and
/// negative zero as `0`.
fn js_number(number: &serde_json::Number) -> String {
    match number.as_f64() {
        Some(value)
            if !number.is_u64()
                && !number.is_i64()
                && value.is_finite()
                && value.fract() == 0.0
                && value.abs() < 1e21 =>
        {
            // `+ 0.0` turns -0 into 0, as `String(-0)` does.
            format!("{:.0}", value + 0.0)
        }
        _ => number.to_string(),
    }
}

/// Every key and index of the tree, depth first, refusing the
/// prototype-pollution keys and any nesting past [`MAX_GRAMMAR_DEPTH`].
fn scan_forbidden_keys(value: &Value, path: &str, out: &mut Vec<Issue>, depth: usize) {
    if !matches!(value, Value::Object(_) | Value::Array(_)) {
        return;
    }
    if depth > MAX_GRAMMAR_DEPTH {
        out.push(issue(
            path,
            format!(
                "grammar nesting deeper than {MAX_GRAMMAR_DEPTH} levels: refused (no real \
                 GrammarSpec is this deep, and the scans must not be recursed off the stack)"
            ),
        ));
        return;
    }
    match value {
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                scan_forbidden_keys(item, &format!("{path}[{i}]"), out, depth + 1);
            }
        }
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}.{key}");
                if FORBIDDEN_KEYS.contains(&key.as_str()) {
                    out.push(issue(
                        child_path,
                        format!(
                            "forbidden key '{key}': refused to prevent prototype pollution (a \
                             serialized grammar is data, never a route to Object.prototype)"
                        ),
                    ));
                    continue;
                }
                scan_forbidden_keys(child, &child_path, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn bad_ref(reference: &str, path: impl Into<String>) -> Issue {
    issue(
        path,
        format!(
            "unknown function reference '{reference}': a serialized grammar may only name \
             $-suffixed engine builtins"
        ),
    )
}

/// `/^@[A-Za-z_$][\w$.-]*$/`: an options string the engine would resolve
/// as a function reference. `@@…` (an escaped literal) and the
/// serialized regexes `@/re/flags` and `@~/re/flags` never match it.
fn ref_shaped(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next() == Some('@')
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.' | '-'))
}

/// Options strings: `@@…` (escaped literal), `@SKIP` (merge sentinel)
/// and `@/re/flags` / `@~/re/flags` (serialized RegExps) are data;
/// `$`-suffixed builtins pass; any other ref-shaped `@name` would be
/// resolved from a ref bag this lane refuses to accept.
fn scan_options_refs(
    value: &Value,
    path: &str,
    out: &mut Vec<Issue>,
    depth: usize,
    builtins: &mut BuiltinRefs,
) {
    if depth > MAX_GRAMMAR_DEPTH || out.len() > MAX_OPTIONS_ISSUES {
        return;
    }
    match value {
        Value::String(text) => {
            // The canonical order tests the data forms and the builtins
            // before the shape; every data form fails the shape anyway,
            // so testing the shape first gives the same findings and
            // asks the engine only about names that could be references.
            if !ref_shaped(text) || text == "@SKIP" || builtins.is_builtin(text) {
                return;
            }
            out.push(bad_ref(text, path));
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                scan_options_refs(item, &format!("{path}[{i}]"), out, depth + 1, builtins);
            }
        }
        Value::Object(map) => {
            for (key, child) in map {
                scan_options_refs(child, &format!("{path}.{key}"), out, depth + 1, builtins);
            }
        }
        _ => {}
    }
}

/// In alt function positions EVERY `@`-string is a reference, so the
/// rule is strict: builtin or refused.
fn scan_alt_refs(alt: &Value, path: &str, out: &mut Vec<Issue>, builtins: &mut BuiltinRefs) {
    let Value::Object(alt) = alt else {
        return;
    };
    for key in ALT_FUNC_KEYS {
        match alt.get(key) {
            Some(Value::String(reference)) if reference.starts_with('@') => {
                if !builtins.is_builtin(reference) {
                    out.push(bad_ref(reference, format!("{path}.{key}")));
                }
            }
            Some(Value::Array(items)) if key == "a" => {
                for (i, item) in items.iter().enumerate() {
                    if let Value::String(reference) = item {
                        if reference.starts_with('@') && !builtins.is_builtin(reference) {
                            out.push(bad_ref(reference, format!("{path}.a[{i}]")));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// A rule state's alternates: the list itself, or an `{alts: [...]}`
/// wrapper's; anything else has none.
fn alts_of(state: Option<&Value>) -> &[Value] {
    match state {
        Some(Value::Array(alts)) => alts,
        Some(Value::Object(wrapper)) => match wrapper.get("alts") {
            Some(Value::Array(alts)) => alts,
            _ => &[],
        },
        _ => &[],
    }
}

/// The answers one firewall run has had from the engine, so a name used
/// a thousand times is asked about once.
#[derive(Default)]
struct BuiltinRefs {
    memo: HashMap<String, bool>,
}

impl BuiltinRefs {
    fn is_builtin(&mut self, name: &str) -> bool {
        if let Some(&known) = self.memo.get(name) {
            return known;
        }
        let known = is_builtin_ref(name);
        self.memo.insert(name.to_string(), known);
        known
    }
}

/// Whether `name` is one of the engine's builtin function references
/// (`isBuiltin` in `ts/src/loaders.js`: `$`-suffixed and in the engine's
/// `BUILTIN_REFS`). Answered by the engine: a bare instance, with no
/// references registered, accepts the name as an alternate's action or
/// as its condition. Accepted names are remembered for the process;
/// they are the engine's builtins, so that set is bounded.
pub fn is_builtin_ref(name: &str) -> bool {
    if !name.starts_with('@') || !name.ends_with('$') {
        return false;
    }
    static ACCEPTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let accepted = ACCEPTED.get_or_init(Mutex::default);
    if accepted
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(name)
    {
        return true;
    }
    let known = ["a", "c"]
        .iter()
        .any(|slot| bare_engine_accepts(slot, name));
    if known {
        accepted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_string());
    }
    known
}

/// Whether a bare engine installs a one-alternate rule naming `name` in
/// the alt slot `slot`.
fn bare_engine_accepts(slot: &str, name: &str) -> bool {
    let mut alt = Map::new();
    alt.insert(slot.to_string(), Value::String(name.to_string()));
    let mut rule = Map::new();
    rule.insert("open".into(), Value::Array(vec![Value::Object(alt)]));
    let mut rules = Map::new();
    rules.insert("tabnas_lsp_builtin_probe".into(), Value::Object(rule));
    let mut document = Map::new();
    document.insert("rule".into(), Value::Object(rules));
    GrammarSpec::from_value(Value::Object(document))
        .is_ok_and(|spec| Tabnas::new().grammar(&spec).is_ok())
}

// ---------------------------------------------------------------------
// Path resolution, workspace-sandboxed. A workspace manifest names files
// relative to its own folder and may not reach outside it: document-
// controlled paths are attack surface (design §10). The check runs on
// REAL paths: a lexical prefix test alone accepts `grammar.json` that is
// a symlink out of the workspace, and the read then follows the link.

/// `path.resolve(base, path)`: `path` against `base` (an absolute `path`
/// stands alone), with `.` and `..` removed lexically, no symlink
/// followed.
fn lexical_resolve(base: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

/// A folder made absolute against the working directory, as
/// `path.resolve(dir)` makes it.
fn absolute(dir: &Path) -> PathBuf {
    match std::env::current_dir() {
        Ok(cwd) => lexical_resolve(&cwd, dir),
        Err(_) => lexical_resolve(Path::new(""), dir),
    }
}

/// Resolve a grammar file named by an entry against its sandbox folder,
/// refusing a path that escapes it lexically or through a symlink (the
/// check runs on REAL paths); a relative path with no folder is refused
/// too. `resolveSandboxed` in TypeScript. Returns the real path.
pub fn resolve_sandboxed(file: &str, base_dir: Option<&Path>) -> Result<PathBuf, LoadError> {
    let Some(base_dir) = base_dir else {
        return Err(LoadError::new(format!(
            "grammar file paths need a base directory: {file}"
        )));
    };
    let base = fs::canonicalize(absolute(base_dir)).map_err(|error| {
        LoadError::new(format!(
            "workspace folder not readable: {}: {error}",
            base_dir.display()
        ))
    })?;
    let abs = lexical_resolve(&base, Path::new(file));
    // Lexical gate first, so a plainly escaping RELATIVE path is refused
    // with the clear message even when its target does not exist.
    // `Path::starts_with` compares whole components: a sibling folder
    // sharing a prefix (`project-evil`) is not inside `project`.
    if !abs.starts_with(&base) {
        return Err(LoadError::new(format!(
            "grammar file escapes its workspace folder: {file} (from {})",
            base.display()
        )));
    }
    let real = fs::canonicalize(&abs)
        .map_err(|error| LoadError::new(format!("grammar file not readable: {file}: {error}")))?;
    if !real.starts_with(&base) {
        return Err(LoadError::new(format!(
            "grammar file escapes its workspace folder (via symlink): {file} -> {}",
            real.display()
        )));
    }
    Ok(real)
}

/// The file a workspace entry's grammar comes from, as the server
/// matches changed files against it for hot reload: the `grammar` or
/// the `spec` path, resolved lexically from the entry's folder
/// (`path.resolve(entry._dir, file)`); `None` for an entry that is not
/// from the workspace, has no folder, or loads no file.
pub fn watched_file(entry: &Entry) -> Option<PathBuf> {
    if !entry.is_workspace() {
        return None;
    }
    let dir = entry.dir.as_deref()?;
    let file = match entry.load() {
        Load::Grammar(file) | Load::Spec(SpecSource::File(file)) => file,
        _ => return None,
    };
    if file.is_empty() {
        return None;
    }
    Some(lexical_resolve(&absolute(dir), Path::new(&file)))
}

fn too_large(entry: &Entry, size: u64) -> LoadError {
    LoadError::new(format!(
        "grammar file for {} is {size} bytes, larger than {MAX_GRAMMAR_BYTES}",
        entry.language_id()
    ))
}

/// Read a grammar file with [`MAX_GRAMMAR_BYTES`] applied BEFORE the
/// content is parsed or compiled: to its size, and to what is read, so a
/// file that grows between the two is bounded too. Text that is not
/// UTF-8 is decoded with replacement characters, as Node's `utf8`
/// decoding does.
pub fn read_capped(file: &Path, entry: &Entry) -> Result<String, LoadError> {
    let unreadable = |error: std::io::Error| {
        LoadError::new(format!(
            "grammar file not readable: {}: {error}",
            file.display()
        ))
    };
    let size = fs::metadata(file).map_err(unreadable)?.len();
    if size > MAX_GRAMMAR_BYTES {
        return Err(too_large(entry, size));
    }
    let mut bytes = Vec::new();
    fs::File::open(file)
        .and_then(|handle| handle.take(MAX_GRAMMAR_BYTES + 1).read_to_end(&mut bytes))
        .map_err(unreadable)?;
    if bytes.len() as u64 > MAX_GRAMMAR_BYTES {
        return Err(LoadError::new(format!(
            "grammar file for {} grew past {MAX_GRAMMAR_BYTES} bytes while it was read",
            entry.language_id()
        )));
    }
    Ok(match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    })
}

// ---------------------------------------------------------------------
// Options

fn object_or_empty(value: Option<&Value>) -> Map<String, Value> {
    match value {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

/// The engine options for an entry's instance, as the serialized options
/// document: the entry's `options` with recovery enabled underneath (a
/// nested merge, so an entry that sets any `parse` option keeps
/// `recover.enabled`, and an entry's own `recover` settings win), and
/// the entry's `syncGroups` when its `recover` declares none. `tnOpts`
/// in `makeLoader`; `NewInstance` in Go.
pub fn instance_options(entry: &Entry) -> Value {
    let mut options = object_or_empty(entry.options.as_ref());
    let mut parse = object_or_empty(options.get("parse"));
    let entry_recover = object_or_empty(parse.get("recover"));
    let declares_groups = entry_recover.contains_key("syncGroups");
    let mut recover = Map::new();
    recover.insert("enabled".into(), Value::Bool(true));
    // Object.assign: an entry key replaces a default in its place.
    recover.extend(entry_recover);
    if !declares_groups {
        if let Some(groups) = &entry.sync_groups {
            recover.insert(
                "syncGroups".into(),
                Value::Array(groups.iter().cloned().map(Value::String).collect()),
            );
        }
    }
    parse.insert("recover".into(), Value::Object(recover));
    options.insert("parse".into(), Value::Object(parse));
    Value::Object(options)
}

/// Apply an options document (from [`instance_options`]) to an instance
/// through the engine's serialized-options path, after the firewall:
/// that path resolves `@`-references against whatever the instance has
/// registered, so entry options are held to the rules a spec's options
/// are.
pub fn apply_options(parser: &mut Tabnas, options: &Value, entry: &Entry) -> Result<(), LoadError> {
    let mut document = Map::new();
    document.insert("options".into(), options.clone());
    let document = Value::Object(document);
    let issues = firewall_spec(&document);
    if !issues.is_empty() {
        return Err(LoadError::with_issues(
            format!("options for {} failed the firewall", entry.language_id()),
            issues,
        ));
    }
    let spec = GrammarSpec::from_value(document)?;
    parser.grammar(&spec)?;
    Ok(())
}

/// Firewall a spec, then install it on the instance its entry composes
/// (a linked base or stack, else a bare engine) with `options` applied.
/// `installSpec` in `makeLoader`: composition-aware, so a spec layering
/// on a base grammar validates and loads against that declared stack,
/// not a bare engine.
pub fn install_spec(
    spec: Value,
    entry: &Entry,
    options: &Value,
    loader: &Loader,
) -> Result<Tabnas, LoadError> {
    let issues = firewall_spec(&spec);
    if !issues.is_empty() {
        return Err(LoadError::with_issues(
            format!("grammar for {} failed the firewall", entry.language_id()),
            issues,
        ));
    }
    let mut layers: Vec<&str> = Vec::new();
    if entry.plugin_kind() == "grammar" {
        layers.extend(entry.base.as_deref().filter(|base| !base.is_empty()));
    }
    layers.extend(entry.stack.iter().flatten().map(String::as_str));
    let mut parser = loader.composed(&layers)?;
    apply_options(&mut parser, options, entry)?;
    let spec = GrammarSpec::from_value(spec)?;
    parser.grammar(&spec)?;
    Ok(parser)
}

// ---------------------------------------------------------------------
// L3: dialect compile.

/// Compile BNF-dialect grammar text to a pure spec: the dialect from the
/// file's extension, `builtins: true` explicitly (the default conversion
/// is closure mode and does not serialize), then the pure lowering that
/// strips compiler marks, stamps `v` and carries `meta.provenance`.
/// Without the `dialects` feature every dialect is refused as not
/// compiled in.
pub fn compile_grammar_text(file: &Path, src: &str) -> Result<Value, LoadError> {
    let dialect = Dialect::of_file(file)?;
    #[cfg(not(feature = "dialects"))]
    {
        let _ = src;
        Err(LoadError::new(format!(
            "grammar dialect package not compiled in: {} (needed to compile {}): \
             build tabnas-lsp with the `dialects` feature",
            dialect.package(),
            file.display()
        )))
    }
    #[cfg(feature = "dialects")]
    {
        let convert = tabnas_abnf::AbnfConvertOptions {
            builtins: true,
            ..Default::default()
        };
        let converted = match dialect {
            Dialect::Abnf => {
                tabnas_abnf::abnf_convert(src, Some(&convert)).map_err(|e| e.to_string())
            }
            Dialect::Ebnf => {
                tabnas_ebnf::ebnf_convert(src, Some(&convert)).map_err(|e| e.to_string())
            }
            Dialect::Gbnf => {
                tabnas_gbnf::gbnf_convert(src, Some(&tabnas_gbnf::GbnfConvertOptions::new(convert)))
                    .map_err(|e| e.to_string())
            }
        }
        .map_err(LoadError::new)?;
        // One lowering for all three: they share the compiler, and so its
        // spec type and `to_pure_spec`.
        tabnas_abnf::to_pure_spec(&converted).map_err(|error| LoadError::new(error.to_string()))
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

// ---------------------------------------------------------------------
// The loader.

/// A linked grammar: builds an instance with the grammar installed,
/// complete with its own base chain (a fleet crate's `make()`); the
/// loader applies the entry's options afterwards.
pub type Factory = Arc<dyn Fn() -> Tabnas + Send + Sync>;

/// The binary's `MakeInstance` (`makeLoader` in TypeScript): the linked
/// grammars, the trust setting, and the dispatch on an entry's `load`.
#[derive(Clone, Default)]
pub struct Loader {
    linked: HashMap<String, Factory>,
    /// Shared by every clone, and read at make time: the server learns
    /// `trustWorkspaceModules` at `initialize`, after the loader it
    /// handed out was built (the TypeScript loader reads `opts.trust`
    /// late for the same reason).
    trust_workspace_modules: Arc<AtomicBool>,
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
    /// The setting is shared with every clone of this loader, including
    /// one already turned into a [`MakeInstance`].
    pub fn set_trust_workspace_modules(&mut self, trust: bool) -> &mut Self {
        self.trust_workspace_modules.store(trust, Ordering::SeqCst);
        self
    }

    pub fn trusts_workspace_modules(&self) -> bool {
        self.trust_workspace_modules.load(Ordering::SeqCst)
    }

    /// The shared trust setting itself, for a server that grants trust
    /// at `initialize` to a loader it no longer holds.
    pub fn trust_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.trust_workspace_modules)
    }

    /// The instance a list of layers composes: a bare engine for none,
    /// else the LAST layer's factory, every layer linked (each is
    /// `require`d in TypeScript, so one this server lacks is refused by
    /// name).
    fn composed(&self, layers: &[&str]) -> Result<Tabnas, LoadError> {
        let mut factory = None;
        for name in layers {
            factory = Some(self.linked(name).ok_or_else(|| not_linked(name))?);
        }
        Ok(factory.map_or_else(Tabnas::new, |factory| factory()))
    }

    /// L1: a linked module. A workspace entry is refused unless the host
    /// trusts workspace modules.
    fn module(&self, entry: &Entry, name: &str, options: &Value) -> Result<Tabnas, LoadError> {
        if entry.is_workspace() && !self.trusts_workspace_modules() {
            return Err(LoadError::new(format!(
                "workspace entry {} loads module {name}, which runs code from the workspace. \
                 Refused: set trustWorkspaceModules in initializationOptions to allow it.",
                entry.language_id()
            )));
        }
        // `entry.stack || buildStack(entry)`: an explicit stack, else the
        // base (for a grammar entry) under the module.
        let layers: Vec<&str> = match &entry.stack {
            Some(stack) => stack.iter().map(String::as_str).collect(),
            None => {
                let mut layers = Vec::new();
                if entry.plugin_kind() == "grammar" {
                    layers.extend(entry.base.as_deref().filter(|base| !base.is_empty()));
                }
                layers.push(name);
                layers
            }
        };
        let mut parser = self.composed(&layers)?;
        apply_options(&mut parser, options, entry)?;
        Ok(parser)
    }

    /// Build the instance for an entry: dispatch on [`Entry::load`], the
    /// three lanes above, with recovery enabled and the entry's options
    /// applied. A workspace entry's module load is refused unless
    /// trusted; an unlinked module is refused by name. Every call reads
    /// its files afresh, so rebuilding after a file change (hot reload)
    /// is calling this again.
    pub fn make_instance(&self, entry: &Entry) -> Result<Tabnas, LoadError> {
        let options = instance_options(entry);
        match entry.load() {
            // `null != load.spec` fails for an explicit null, and the
            // canonical dispatch falls through to the entry's module.
            Load::Spec(SpecSource::Inline(Value::Null)) => {
                self.module(entry, &entry.name, &options)
            }
            Load::Spec(SpecSource::Inline(spec)) => install_spec(spec, entry, &options, self),
            Load::Spec(SpecSource::File(file)) => {
                let path = resolve_sandboxed(&file, entry.dir.as_deref())?;
                let text = read_capped(&path, entry)?;
                let spec = serde_json::from_str(&text).map_err(|error| {
                    LoadError::new(format!("grammar file {file} is not valid JSON: {error}"))
                })?;
                install_spec(spec, entry, &options, self)
            }
            Load::Grammar(file) => {
                let path = resolve_sandboxed(&file, entry.dir.as_deref())?;
                let src = read_capped(&path, entry)?;
                let spec = compile_grammar_text(&path, &src)?;
                install_spec(spec, entry, &options, self)
            }
            Load::Module(module) => {
                let name = if module.is_empty() {
                    entry.name.as_str()
                } else {
                    module.as_str()
                };
                self.module(entry, name, &options)
            }
        }
    }

    /// This loader as the callback [`crate::Instances`] and
    /// [`crate::Config`] take.
    pub fn into_make_instance(self) -> MakeInstance {
        Arc::new(move |entry| self.make_instance(entry))
    }
}

fn not_linked(name: &str) -> LoadError {
    LoadError::new(format!(
        "grammar module {name} is not linked into this server: a Rust server serves the \
         grammars built into it (the `fleet` feature, or a host's Loader::link)"
    ))
}

impl std::fmt::Debug for Loader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loader")
            .field("linked", &self.linked_names())
            .field("trust_workspace_modules", &self.trusts_workspace_modules())
            .finish()
    }
}

/// An entry plus its `MakeInstance` for a serialized `GrammarSpec`: the
/// L2 lane on its own, as a generated or embedded single-language server
/// uses it (`EntryFromSpecJSON` in `go/lsp.go`). The entry is an enabled
/// `grammar` with a clean lex stream, its `load` the spec inline when it
/// parses; the spec goes through the firewall at every make, with the
/// options of the entry the make is called with.
pub fn entry_from_spec_json(
    language_id: &str,
    extensions: &[&str],
    spec: &str,
) -> (Entry, MakeInstance) {
    let mut entry = Entry::new(language_id);
    entry.language_id = Some(language_id.to_string());
    entry.extensions = extensions.iter().map(|x| x.to_string()).collect();
    entry.plugin_kind = Some("grammar".into());
    entry.lex_stream = Some("clean".into());
    entry.enabled = Some(true);
    let parsed: Result<Value, String> = serde_json::from_str(spec).map_err(|e| e.to_string());
    if let Ok(value) = &parsed {
        entry.load = Some(Load::Spec(SpecSource::Inline(value.clone())));
    }
    let loader = Loader::new();
    let make: MakeInstance = Arc::new(move |entry: &Entry| {
        let spec = parsed.clone().map_err(|error| {
            LoadError::new(format!("grammar for {}: {error}", entry.language_id()))
        })?;
        install_spec(spec, entry, &instance_options(entry), &loader)
    });
    (entry, make)
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
        assert_eq!(
            error.message,
            "unknown grammar dialect .bnf for g.bnf \u{2014} expected one of: .abnf, .ebnf, .gbnf"
        );
        let error = Dialect::of_file(Path::new("grammar")).unwrap_err();
        assert!(error.message.contains("(no extension)"), "{error}");
        assert_eq!(Dialect::Gbnf.package(), "@tabnas/gbnf");
    }

    #[test]
    fn ref_shape_is_the_typescript_pattern() {
        for yes in ["@a", "@_x", "@$", "@custom", "@a.b-c$", "@node$", "@A9"] {
            assert!(ref_shaped(yes), "{yes}");
        }
        for no in [
            "", "@", "a", "@@x", "@/re/i", "@~/re/", "@1x", "@-x", "@a b", "@a\n", "@\u{e9}",
        ] {
            assert!(!ref_shaped(no), "{no:?}");
        }
    }

    #[test]
    fn a_schema_version_prints_the_way_javascript_prints_it() {
        let number = |src: &str| serde_json::from_str::<serde_json::Number>(src).unwrap();
        assert_eq!(js_number(&number("999")), "999");
        assert_eq!(js_number(&number("6.0")), "6");
        assert_eq!(js_number(&number("6.5")), "6.5");
        assert_eq!(js_number(&number("-0.0")), "0");
    }

    #[test]
    fn the_engine_answers_which_refs_are_builtin() {
        // The canonical BUILTIN_REFS: thirteen actions, three conditions.
        for name in [
            "@node$",
            "@capture$",
            "@bubble$",
            "@fold$",
            "@probeInit$",
            "@probeDecide$",
            "@probePhase0$",
            "@probePhase1$",
            "@probePhase2$",
            "@object$",
            "@array$",
            "@reset$",
            "@key$",
            "@setval$",
            "@push$",
            "@value$",
        ] {
            assert!(is_builtin_ref(name), "{name} is an engine builtin");
        }
        // Names the Rust engine resolves internally but TypeScript does not
        // list, since they are not `$`-suffixed, and names nobody knows.
        for name in [
            "@map-bo",
            "@pairkey",
            "@evil$",
            "@node",
            "node$",
            "@",
            "@$",
            "@probePhase3$",
        ] {
            assert!(!is_builtin_ref(name), "{name} is not a builtin");
        }
    }

    #[test]
    fn lexical_resolution_is_path_resolve() {
        let base = Path::new("/ws/a");
        assert_eq!(
            lexical_resolve(base, Path::new("g.json")),
            Path::new("/ws/a/g.json")
        );
        assert_eq!(
            lexical_resolve(base, Path::new("./x/../g")),
            Path::new("/ws/a/g")
        );
        assert_eq!(
            lexical_resolve(base, Path::new("../../../..")),
            Path::new("/")
        );
        assert_eq!(
            lexical_resolve(base, Path::new("/etc/x")),
            Path::new("/etc/x")
        );
    }
}
