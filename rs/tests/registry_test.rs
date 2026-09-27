// The embedded registry is the committed ts/data/registry.json, so this
// suite holds the crate to that file: it parses, describes itself, and
// classifies the fleet's grammars the way the TypeScript server does.

use std::fs;
use std::path::Path;

use tabnas_lsp::{Registry, REGISTRY_JSON};

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
