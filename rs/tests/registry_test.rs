// The embedded registry is the committed ts/data/registry.json, so this
// suite holds the crate to that file: it parses, describes itself, and
// classifies the fleet's grammars the way the TypeScript server does.
// Routing over the tiers follows, case for case with the TypeScript
// suites (ts/test/core.test.js, ts/test/registry.test.js).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use tabnas_lsp::{Entry, EntrySource, Registry, Resolution, Router, Via, REGISTRY_JSON};

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rs/ has a parent")
}

#[test]
fn the_embedded_registry_is_the_committed_file() {
    let committed = fs::read_to_string(repo_root().join("ts").join("data").join("registry.json"))
        .expect("ts/data/registry.json is readable");
    assert_eq!(REGISTRY_JSON, committed);
}

#[test]
fn the_bundled_registry_parses_and_describes_itself() {
    let registry = Registry::bundled();
    assert_eq!(registry.count, registry.entries.len());
    assert!(!registry.entries.is_empty());
    assert_eq!(
        registry.generated.as_deref(),
        Some("tools/gen-registry.js"),
        "the file must be the generator's output"
    );
    // Same object every time.
    assert!(std::ptr::eq(registry, Registry::bundled()));
}

#[test]
fn every_entry_has_a_language_id_and_a_lex_stream() {
    let registry = Registry::bundled();
    let mut ids: Vec<&str> = registry.language_ids().collect();
    assert_eq!(ids.len(), registry.entries.len());
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), registry.entries.len(), "language ids are unique");
    for entry in &registry.entries {
        assert!(!entry.language_id().is_empty(), "{}", entry.name);
        assert!(
            matches!(entry.lex_stream(), "clean" | "speculative"),
            "{}: lexStream {}",
            entry.name,
            entry.lex_stream()
        );
    }
}

#[test]
fn the_json_family_is_clean() {
    let registry = Registry::bundled();
    for id in [
        "json", "jsonc", "json5", "jsonl", "jsonic", "csv", "ini", "toml", "xml", "yaml", "zon",
    ] {
        let entry = registry
            .entry(id)
            .unwrap_or_else(|| panic!("no registry entry for {id}"));
        assert!(entry.is_clean(), "{id} is not clean");
        assert!(registry.clean(id), "{id}");
    }
    assert!(!registry.clean("no-such-language"));
    assert!(registry.entry("no-such-language").is_none());
    assert_eq!(registry.overrides("no-such-language"), None);
}

#[test]
fn language_ids_without_a_declared_one_come_from_the_package_name() {
    let registry = Registry::bundled();
    let abnf = registry
        .entry("abnf")
        .expect("@tabnas/abnf is in the registry");
    assert_eq!(abnf.name, "@tabnas/abnf");
    assert_eq!(abnf.language_id(), "abnf");
    assert_eq!(abnf.plugin_kind(), "compiler");
}

// ---------------------------------------------------------------------
// Policy: the editor-collision defaults are the generated file's.

#[test]
fn the_entrenched_incumbents_are_off_and_every_other_language_is_on() {
    // Design §5: a descriptor cannot enable itself, and the collision
    // policy defaults the entrenched ids off. The generator writes it
    // (its OVERRIDES table); the registry reads it, never a copy.
    let registry = Registry::bundled();
    let mut off: Vec<&str> = registry
        .entries
        .iter()
        .filter(|entry| !entry.is_enabled())
        .map(Entry::language_id)
        .collect();
    off.sort_unstable();
    assert_eq!(
        off,
        ["c", "css", "ini", "json", "jsonc", "markdown", "proto", "toml", "xml", "yaml"]
    );
    // So an editor's own JSON support keeps .json, and an unclaimed
    // language routes by its extension.
    let router = Router::new(registry.to_entries(), vec![], vec![]);
    assert_eq!(
        router.resolve("json", "file:///x/a.json"),
        Resolution::none()
    );
    let r = router.resolve("plaintext", "file:///x/a.json5");
    assert_eq!(r.entry.map(Entry::language_id), Some("json5"));
    assert_eq!(r.via, Some(Via::Extension));
}

#[test]
fn capabilities_are_the_entry_fields() {
    let registry = Registry::bundled();
    let csv = registry.entry("csv").unwrap();
    assert_eq!(
        csv.sync_groups.as_deref(),
        Some(&["end".to_string(), "comma".to_string()][..])
    );
    assert_eq!(csv.media_types, ["text/csv"]);
    assert!(csv.is_clean());
    let debug = registry.entry("debug").unwrap();
    assert_eq!(debug.plugin_kind(), "modifier");
    assert!(!debug.is_routable(), "a modifier is never a routing target");
}

// ---------------------------------------------------------------------
// Routing, case for case with ts/test/core.test.js and
// ts/test/registry.test.js.

fn entry(fields: serde_json::Value) -> Entry {
    serde_json::from_value(fields).expect("an entry")
}

fn toml() -> Vec<Entry> {
    vec![entry(
        json!({"name": "@tabnas/toml", "languageId": "toml", "extensions": [".toml"]}),
    )]
}

/// A workspace entry scoped to `dir`, as a folder manifest yields one.
fn ws(language_id: &str, dir: &str, extra: serde_json::Value) -> Entry {
    let mut fields = json!({
        "name": language_id, "languageId": language_id,
        "extensions": [format!(".{language_id}")],
        "_source": "workspace", "_dir": dir,
    });
    for (key, value) in extra.as_object().expect("extra fields") {
        fields[key] = value.clone();
    }
    entry(fields)
}

fn dir_of(resolution: &Resolution<'_>) -> Option<String> {
    resolution
        .entry
        .and_then(|entry| entry.dir.as_ref())
        .map(|dir| dir.to_string_lossy().into_owned())
}

#[test]
fn the_language_id_gates_and_the_extension_falls_back() {
    let router = Router::new(
        vec![
            entry(json!({"name": "@tabnas/toml", "languageId": "toml", "extensions": [".toml"]})),
            entry(
                json!({"name": "@tabnas/hoover", "languageId": "hoover", "pluginKind": "modifier"}),
            ),
            entry(
                json!({"name": "@tabnas/yaml", "languageId": "yaml", "extensions": [".yaml"], "enabled": false}),
            ),
        ],
        vec![],
        vec![],
    );
    let id = |language_id: &str, uri: &str| {
        router
            .resolve(language_id, uri)
            .entry
            .map(|entry| entry.language_id().to_string())
    };
    assert_eq!(id("toml", "file:///x.toml").as_deref(), Some("toml"));
    // A generic client id falls back to extension matching.
    assert_eq!(id("plaintext", "file:///x.toml").as_deref(), Some("toml"));
    // Disabled entries are not routed even by extension.
    assert_eq!(id("plaintext", "file:///x.yaml"), None);
    // Modifiers are never routing targets.
    assert_eq!(id("hoover", "file:///x.hoover"), None);
}

#[test]
fn the_same_language_id_in_two_folders_resolves_by_document_folder() {
    let router = Router::new(
        toml(),
        vec![
            ws("mydsl", "/ws/alpha", json!({})),
            ws("mydsl", "/ws/beta", json!({})),
        ],
        vec![],
    );
    let alpha = router.resolve("mydsl", "file:///ws/alpha/doc.mydsl");
    assert_eq!(dir_of(&alpha).as_deref(), Some("/ws/alpha"));
    assert_eq!(alpha.via, Some(Via::WorkspaceLanguageId));
    let beta = router.resolve("mydsl", "file:///ws/beta/doc.mydsl");
    assert_eq!(dir_of(&beta).as_deref(), Some("/ws/beta"));
    // Sibling-prefix folders do not capture each other's documents.
    assert_eq!(
        router.resolve("mydsl", "file:///ws/alphabet/doc.mydsl"),
        Resolution::none()
    );
}

#[test]
fn the_deepest_matching_folder_wins_for_nested_roots() {
    let router = Router::new(
        vec![],
        vec![
            ws("mydsl", "/ws/app", json!({})),
            ws("mydsl", "/ws/app/vendored", json!({})),
        ],
        vec![],
    );
    let inner = router.resolve("mydsl", "file:///ws/app/vendored/x.mydsl");
    assert_eq!(dir_of(&inner).as_deref(), Some("/ws/app/vendored"));
    let outer = router.resolve("mydsl", "file:///ws/app/x.mydsl");
    assert_eq!(dir_of(&outer).as_deref(), Some("/ws/app"));
}

#[test]
fn a_workspace_entry_beats_bundled_inside_its_folder_not_outside() {
    let router = Router::new(
        toml(),
        vec![ws("toml", "/ws/alpha", json!({"extensions": [".toml"]}))],
        vec![],
    );
    let inside = router.resolve("toml", "file:///ws/alpha/x.toml");
    assert_eq!(inside.entry.map(|e| e.source), Some(EntrySource::Workspace));
    let outside = router.resolve("toml", "file:///elsewhere/x.toml");
    assert_eq!(outside.entry.map(|e| e.source), Some(EntrySource::Bundled));
    assert_eq!(outside.via, Some(Via::LanguageId));
}

#[test]
fn the_workspace_extension_fallback_stays_folder_scoped() {
    let router = Router::new(vec![], vec![ws("mydsl", "/ws/alpha", json!({}))], vec![]);
    let inside = router.resolve("plaintext", "file:///ws/alpha/doc.mydsl");
    assert_eq!(dir_of(&inside).as_deref(), Some("/ws/alpha"));
    assert_eq!(inside.via, Some(Via::WorkspaceExtension));
    assert_eq!(
        router.resolve("plaintext", "file:///other/doc.mydsl"),
        Resolution::none()
    );
}

#[test]
fn a_windows_folder_path_matches_its_documents() {
    // fsPathOf yields forward slashes; a Windows folder arrives with
    // backslashes. Compared by the path's shape, never by the platform.
    let router = Router::new(
        vec![],
        vec![ws("mydsl", "c:\\ws\\alpha", json!({}))],
        vec![],
    );
    let r = router.resolve("mydsl", "file:///c%3A/ws/alpha/x.mydsl");
    assert_eq!(r.entry.map(Entry::language_id), Some("mydsl"));
    assert_eq!(
        router.resolve("mydsl", "file:///c%3A/ws/alphabet/x.mydsl"),
        Resolution::none()
    );
}

#[test]
fn a_posix_folder_whose_name_contains_a_backslash_routes_correctly() {
    let router = Router::new(vec![], vec![ws("mydsl", "/work/a\\b", json!({}))], vec![]);
    assert!(router
        .resolve("mydsl", "file:///work/a%5Cb/x.mydsl")
        .entry
        .is_some());
    assert_eq!(
        router.resolve("mydsl", "file:///work/a/b/x.mydsl"),
        Resolution::none()
    );
}

#[test]
fn an_unscoped_entry_routes_everywhere_including_non_file_documents() {
    // `_scope: null` is session-wide; `_dir` is only the sandbox base.
    let router = Router::new(
        toml(),
        vec![ws("mydsl", "/ws/alpha", json!({"_scope": null}))],
        vec![],
    );
    for uri in ["file:///ws/beta/x.mydsl", "untitled:Untitled-1"] {
        let r = router.resolve("mydsl", uri);
        assert_eq!(r.entry.map(Entry::language_id), Some("mydsl"), "{uri}");
    }
}

#[test]
fn a_folder_scoped_entry_beats_an_unscoped_one_inside_its_folder() {
    let global = ws(
        "mydsl",
        "/ws/alpha",
        json!({"_scope": null, "name": "global"}),
    );
    let scoped = ws("mydsl", "/ws/alpha", json!({}));
    let router = Router::new(vec![], vec![global, scoped], vec![]);
    let name = |uri: &str| router.resolve("mydsl", uri).entry.map(|e| e.name.clone());
    assert_eq!(name("file:///ws/alpha/x.mydsl").as_deref(), Some("mydsl"));
    assert_eq!(name("file:///elsewhere/x.mydsl").as_deref(), Some("global"));
}

#[test]
fn non_file_documents_never_match_folder_scoped_entries() {
    let router = Router::new(toml(), vec![ws("mydsl", "/ws/alpha", json!({}))], vec![]);
    assert_eq!(
        router.resolve("mydsl", "untitled:Untitled-1"),
        Resolution::none()
    );
    // Globals still resolve for non-file documents.
    let r = router.resolve("toml", "untitled:Untitled-1.toml");
    assert_eq!(r.entry.map(|e| e.source), Some(EntrySource::Bundled));
}

#[test]
fn an_explicit_folder_scope_routes_apart_from_the_sandbox_folder() {
    let router = Router::new(
        vec![],
        vec![ws("mydsl", "/sandbox", json!({"_scope": "/ws/served"}))],
        vec![],
    );
    assert!(router
        .resolve("mydsl", "file:///ws/served/x.mydsl")
        .entry
        .is_some());
    assert_eq!(
        router.resolve("mydsl", "file:///sandbox/x.mydsl"),
        Resolution::none()
    );
}

#[test]
fn a_disabled_or_modifier_workspace_entry_is_never_a_target() {
    let router = Router::new(
        toml(),
        vec![
            ws(
                "toml",
                "/ws",
                json!({"enabled": false, "extensions": [".toml"]}),
            ),
            ws("mydsl", "/ws", json!({"pluginKind": "modifier"})),
        ],
        vec![],
    );
    let r = router.resolve("toml", "file:///ws/x.toml");
    assert_eq!(r.entry.map(|e| e.source), Some(EntrySource::Bundled));
    assert_eq!(
        router.resolve("mydsl", "file:///ws/x.mydsl"),
        Resolution::none()
    );
}

// ---------------------------------------------------------------------
// Media types, for hosts.

#[test]
fn a_host_that_knows_the_media_type_routes_by_it_last() {
    let router = Router::new(Registry::bundled().to_entries(), vec![], vec![]);
    // Nothing else claims the document: the media type does.
    let r = router.resolve_with_media_type("plaintext", "file:///x/data", Some("text/csv"));
    assert_eq!(r.entry.map(Entry::language_id), Some("csv"));
    assert_eq!(r.via, Some(Via::MediaType));
    // The language id and the extension come first.
    let r = router.resolve_with_media_type("zon", "file:///x/data", Some("text/csv"));
    assert_eq!(r.via, Some(Via::LanguageId));
    let r = router.resolve_with_media_type("plaintext", "file:///x/a.zon", Some("text/csv"));
    assert_eq!(r.entry.map(Entry::language_id), Some("zon"));
    // Policy holds here too: application/json names a disabled entry.
    assert_eq!(
        router.resolve_with_media_type("plaintext", "file:///x/data", Some("application/json")),
        Resolution::none()
    );
    assert_eq!(
        router.resolve_with_media_type("plaintext", "file:///x/data", None),
        Resolution::none()
    );
}

// ---------------------------------------------------------------------
// Hot reload's entry points.

#[test]
fn a_changed_grammar_file_names_the_entries_to_rebuild() {
    let spec = ws("spec", "/ws/a", json!({"load": {"spec": "./g/spec.json"}}));
    let grammar = ws("gram", "/ws/a", json!({"load": {"grammar": "my.abnf"}}));
    let inline = ws("inline", "/ws/a", json!({"load": {"spec": {}}}));
    let mut router = Router::new(toml(), vec![spec, grammar, inline], vec![]);
    fn names(router: &Router, changed: &[&str]) -> Vec<String> {
        let changed: Vec<PathBuf> = changed.iter().map(PathBuf::from).collect();
        router
            .reloaded_by(&changed)
            .into_iter()
            .map(|entry| entry.language_id().to_string())
            .collect()
    }
    assert_eq!(names(&router, &["/ws/a/g/spec.json"]), ["spec"]);
    assert_eq!(
        names(&router, &["/ws/a/my.abnf", "/ws/a/g/spec.json"]),
        ["spec", "gram"]
    );
    assert!(names(&router, &["/ws/a/other.json"]).is_empty());
    // A changed manifest replaces the workspace tier; the globals stay.
    router.set_workspace(vec![ws("fresh", "/ws/b", json!({}))]);
    assert_eq!(router.workspace().len(), 1);
    assert_eq!(router.global().len(), 1);
    assert!(router
        .resolve("fresh", "file:///ws/b/x.fresh")
        .entry
        .is_some());
    assert!(names(&router, &["/ws/a/g/spec.json"]).is_empty());
}
