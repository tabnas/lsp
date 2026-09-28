// The instance cache as a host uses it (design §6): a `MakeInstance` of
// the host's own, one instance per entry with the mux installed once,
// parses that collect only while they run, quarantine after
// QUARANTINE_LIMIT failed loads, and invalidation on reload. The
// canonical tests are ts/test/instances.test.js and the instance cases
// of ts/test/core.test.js.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tabnas::{Options, Tabnas};
use tabnas_lsp::instances::QUARANTINE_LIMIT;
use tabnas_lsp::{Entry, Instances, Load, LoadError, MakeInstance, RuleEventState, SpecSource};

fn fixture(name: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rs/ has a parent")
        .join("test")
        .join("fixtures")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn instance() -> Tabnas {
    let mut options = Options::default();
    options.parse.recover.enabled = true;
    let mut parser = Tabnas::with_options(options);
    parser
        .grammar_json(&fixture("json-grammar.json"))
        .expect("json-grammar.json installs");
    parser
}

fn entry(language_id: &str, spec: &str) -> Entry {
    let mut entry = Entry::new(language_id);
    entry.language_id = Some(language_id.into());
    entry.load = Some(Load::Spec(SpecSource::File(spec.into())));
    entry
}

#[test]
fn a_host_supplies_its_parser_and_gets_the_collected_events_back() {
    let made = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&made);
    let make: MakeInstance = Arc::new(move |entry| {
        assert_eq!(entry.language_id(), "jsonf");
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(instance())
    });
    let mut instances = Instances::new(make);
    let entry = entry("jsonf", "json-grammar.json");
    let inst = instances
        .get(&entry, None)
        .unwrap()
        .expect("not quarantined");
    assert_eq!(made.load(Ordering::SeqCst), 1);

    let (recovery, collected) = instances.parse(&inst, "{\"a\":[1,2],\"b\":{}}");
    assert!(recovery.fatal.is_none());
    assert!(recovery.errors.is_empty());
    assert!(recovery.value.is_some());
    // Every token of the document was announced, with engine positions.
    let sources: Vec<&str> = collected.lex.iter().map(|t| t.src.as_str()).collect();
    assert!(sources.contains(&"{") && sources.contains(&"[") && sources.contains(&"\"a\""));
    assert!(collected.lex.iter().all(|t| t.ri == 1));
    // Two maps and one list opened and closed, none forced.
    let closes = |name: &str| {
        collected
            .rules
            .iter()
            .filter(|e| e.name == name && e.state == RuleEventState::Close && e.c0.is_some())
            .count()
    };
    assert_eq!(closes("map"), 2);
    assert_eq!(closes("list"), 1);
    assert!(collected.rules.iter().all(|e| !e.forced));

    // A second document through the same instance collects only itself.
    let (_, second) = instances.parse(&inst, "[true]");
    assert!(second.lex.iter().all(|t| t.src != "{"));
    assert_eq!(made.load(Ordering::SeqCst), 1, "the instance is cached");
    assert_eq!(instances.len(), 1);
}

#[test]
fn a_failing_grammar_is_quarantined_and_released_by_reload() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let make: MakeInstance = Arc::new(move |_entry| {
        counter.fetch_add(1, Ordering::SeqCst);
        Err(LoadError::new("bad grammar"))
    });
    let mut instances = Instances::new(make);
    let bad = entry("bad", "bad.json");
    let folder = Some(Path::new("/ws/a"));
    for _ in 0..10 {
        let _ = instances.get(&bad, folder);
    }
    assert!(instances.quarantined(&bad, folder));
    assert_eq!(attempts.load(Ordering::SeqCst), QUARANTINE_LIMIT);
    assert!(matches!(instances.get(&bad, folder), Ok(None)));
    // The same entry in another folder has its own count.
    assert!(!instances.quarantined(&bad, Some(Path::new("/ws/b"))));
    // Reloading the grammar gives it another chance.
    instances.invalidate(&bad);
    assert!(!instances.quarantined(&bad, folder));
    assert!(instances.get(&bad, folder).is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), QUARANTINE_LIMIT + 1);
}

#[test]
fn the_parse_lock_keeps_engine_calls_out_of_the_collector() {
    let make: MakeInstance = Arc::new(|_entry| Ok(instance()));
    let mut instances = Instances::new(make);
    let inst = instances
        .get(&entry("jsonf", "json-grammar.json"), None)
        .unwrap()
        .unwrap();
    // A continuations query parses internally; run under the lock it
    // leaves nothing behind for the next analysis to pick up.
    let continuations = instances.with_parse_lock(|| inst.continuations("{\"a\""));
    assert!(!continuations.tokens.is_empty(), "the query answered");
    assert!(!instances.mux().is_active());
    let (_, collected) = instances.parse(&inst, "1");
    assert!(collected.lex.iter().all(|t| t.src != "\"a\""));
}
