// Copyright (c) 2026 Richard Rodger, MIT License

//! Engine-instance management (design §6): one long-lived instance per
//! cache key, with exactly ONE permanent mux subscriber pair installed
//! at creation, serialized parses, rebuild-on-reload and per-grammar
//! quarantine. Mirrors `ts/src/instances.js` (canonical: the cache key,
//! `entryPrefix`, folder-scoped quarantine, `invalidate`) and the
//! `Instances` half of `go/core.go` (the closest model: the mux over an
//! engine whose subscribers are installed for the instance's lifetime,
//! `Parse` with an active collector, `WithParseLock` for continuations).
//!
//! Why the mux: the engine has no unsubscribe API. A subscriber installed
//! per parse would stack up, every parse filling every subscriber ever
//! installed, so the pair is installed ONCE, when the instance is built,
//! and forwards to whatever collector the current parse made active.
//! A parse run with no collector active (a `continuations` query, or a
//! host calling the engine directly) makes the mux a no-op.
//!
//! Parses are SERIALIZED, so a single active slot is the whole demux.
//! TypeScript gets that from its one thread; here, as in Go (`parseMu`),
//! a lock does it: [`Instances::parse`] and [`Instances::with_parse_lock`]
//! hold the mux's gate for the whole engine call, so two threads sharing
//! a cache take turns rather than writing into each other's collector.
//! The gate is re-entrant on the thread holding it, which keeps the
//! canonical nesting (`parse` saves the active collector as `prev` and
//! puts it back) instead of deadlocking where Go's mutex would. The
//! previous collector is put back on unwind too, as the TypeScript
//! `finally` does.
//!
//! A collector must never abort a user's parse: the forwarders guard
//! their own bookkeeping the way Go's `recover()` and the TypeScript
//! `try`/`catch` do, and a grammar whose `MakeInstance` fails (returns an
//! error, or panics: the Rust form of the TypeScript `throw`) is counted
//! toward [`QUARANTINE_LIMIT`]; at the limit the entry is disabled under
//! that key and the server survives. A failing analysis is the caller's
//! to count, through [`Instances::record_failure`], as it is in both
//! canonical servers.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};

use serde_json::{json, Value};
use tabnas::{ParseRecovery, Tabnas};

use crate::trace::TokenPoint;
use crate::types::{Collected, Entry, Load, LoadError, MakeInstance, RuleEvent, SpecSource};

/// After this many failures an entry is quarantined (`QUARANTINE_LIMIT`
/// in `ts/src/instances.js`, `QuarantineLimit` in Go).
pub const QUARANTINE_LIMIT: usize = 3;

/// The one permanent subscriber pair's shared state: the slot the
/// current parse's [`Collected`] sits in while the parse runs. Cloning a
/// `Mux` clones the handle, not the slot, which is how the two engine
/// subscribers and the [`Instances`] that drives them share it.
///
/// The handle also carries the gate that serializes parses across
/// threads ([`Mux::parse`], [`Mux::without_collector`]).
#[derive(Debug, Clone, Default)]
pub struct Mux {
    slot: Arc<Mutex<Option<Collected>>>,
    gate: Arc<Gate>,
}

impl Mux {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the pair on `parser`: a `subscribe_lex` forwarder that
    /// records every real token event (not the engine's no-token
    /// sentinel) as a [`TokenPoint`], and a `subscribe_rule_done`
    /// forwarder that records the rule's index, name, pass, `forced`,
    /// the alternate's `r`, and the first matched open and close tokens
    /// (`rule.o[0]`, `rule.c[0]`). Both write into the active slot and
    /// do nothing when it is empty. Called exactly once per instance, by
    /// [`Instances::get`]; a host installing it on its own parser installs
    /// it once too.
    ///
    /// Each forwarder runs under a panic guard (`installMux`'s
    /// `try`/`catch`, Go's `recover()`): the engine would turn a panic in
    /// a subscriber into an `internal` fatal error and end the user's
    /// parse, and the collector's bookkeeping must never do that.
    pub fn install(&self, parser: &mut Tabnas) {
        let lex = self.clone();
        parser.subscribe_lex(move |token, _rule, _context| {
            // The TypeScript collector skips `sI < 0`, the no-token
            // sentinel; the Rust engine's sentinel is `tin == -1`.
            if token.is_no_token() {
                return;
            }
            let _ = catch_unwind(AssertUnwindSafe(|| {
                // The token is copied only when a parse is collecting.
                if let Some(collected) = lex.lock().as_mut() {
                    collected.lex.push(TokenPoint::of(token));
                }
            }));
        });
        let rules = self.clone();
        parser.subscribe_rule_done(move |rule, _context, done| {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let mut slot = rules.lock();
                let Some(collected) = slot.as_mut() else {
                    return;
                };
                collected.rules.push(RuleEvent {
                    i: rule.i,
                    name: rule.name.as_str().to_string(),
                    state: done.state.into(),
                    forced: done.forced,
                    r: done
                        .alt
                        .as_ref()
                        .map(|alt| alt.r.clone())
                        .unwrap_or_default(),
                    o0: rule.o0().map(TokenPoint::of),
                    c0: rule.c0().map(TokenPoint::of),
                });
            }));
        });
    }

    /// Make a fresh collector active, returning whatever was active
    /// before (restored by the caller after the parse, as the canonical
    /// `parse` does with `prev`).
    pub fn begin(&self) -> Option<Collected> {
        self.lock().replace(Collected::default())
    }

    /// Take the active collector out of the slot, leaving it empty.
    pub fn end(&self) -> Option<Collected> {
        self.lock().take()
    }

    /// Put a collector (or none) in the slot, returning the previous one.
    pub fn restore(&self, previous: Option<Collected>) -> Option<Collected> {
        std::mem::replace(&mut *self.lock(), previous)
    }

    /// Whether a parse is collecting right now.
    pub fn is_active(&self) -> bool {
        self.lock().is_some()
    }

    /// One parse of `src` by `inst` with a fresh collector active, the
    /// previous one (if any) put back afterwards, on unwind too: what the
    /// mux collected is returned with the recovery result. The gate is
    /// held throughout, so a parse on another thread waits its turn; the
    /// engine runs its subscribers on the calling thread, inside
    /// `parse_recover`, so nothing else can write to the slot meanwhile.
    /// `parse` in `ts/src/instances.js`, `Parse` in Go.
    pub fn parse(&self, inst: &Tabnas, src: &str) -> (ParseRecovery, Collected) {
        let _turn = self.gate.enter();
        let active = Activation::new(self, Some(Collected::default()));
        let recovery = inst.parse_recover(src);
        let collected = active.finish().unwrap_or_default();
        (recovery, collected)
    }

    /// Run `f` with NO collector active and the gate held (the previous
    /// collector, if any, is put back afterwards, on unwind too), so the
    /// engine calls inside it leak no events into any analysis and no
    /// other thread's parse runs meanwhile. `WithParseLock` in Go. Calling
    /// [`Mux::parse`] inside `f` is allowed: the gate is re-entrant on
    /// its own thread.
    pub fn without_collector<T>(&self, f: impl FnOnce() -> T) -> T {
        let _turn = self.gate.enter();
        let _inactive = Activation::new(self, None);
        f()
    }

    /// The slot, for the forwarders. A poisoned lock (a panic while a
    /// forwarder held it) yields the data anyway: the events recorded so
    /// far are sound, and every later parse must not panic in turn.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Option<Collected>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A collector (or none) put in the slot for the length of one engine
/// call, with the one it replaced put back when the call is over: by
/// [`Activation::finish`] on the normal path, by `Drop` when the call
/// unwinds. The TypeScript `parse` does this with `try`/`finally`, Go's
/// with `defer`.
struct Activation<'a> {
    mux: &'a Mux,
    previous: Option<Option<Collected>>,
}

impl<'a> Activation<'a> {
    fn new(mux: &'a Mux, collector: Option<Collected>) -> Self {
        let previous = mux.restore(collector);
        Activation {
            mux,
            previous: Some(previous),
        }
    }

    /// Put the previous collector back and return the one this call used.
    fn finish(mut self) -> Option<Collected> {
        let previous = self.previous.take().unwrap_or_default();
        self.mux.restore(previous)
    }
}

impl Drop for Activation<'_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.mux.restore(previous);
        }
    }
}

/// The lock that serializes parses: Go's `parseMu`, re-entrant on the
/// thread that holds it. A thread entering while another holds it waits;
/// the holder entering again (a parse inside [`Mux::without_collector`],
/// the canonical nesting) passes straight through.
#[derive(Debug, Default)]
struct Gate {
    held: Mutex<Holder>,
    released: Condvar,
}

/// Which thread holds the [`Gate`], and how many times it entered.
#[derive(Debug, Default)]
struct Holder {
    thread: Option<ThreadId>,
    depth: usize,
}

impl Gate {
    fn enter(&self) -> Turn<'_> {
        let me = thread::current().id();
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if held.thread != Some(me) {
            while held.thread.is_some() {
                held = self
                    .released
                    .wait(held)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            held.thread = Some(me);
        }
        held.depth += 1;
        Turn { gate: self }
    }
}

/// One entry into the [`Gate`], left when dropped (on unwind too).
struct Turn<'a> {
    gate: &'a Gate,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let mut held = self
            .gate
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        held.depth -= 1;
        if held.depth == 0 {
            held.thread = None;
            self.gate.released.notify_one();
        }
    }
}

/// The instance cache: `Instances` in both canonical ports.
pub struct Instances {
    make: MakeInstance,
    cache: HashMap<String, Arc<Tabnas>>,
    failures: HashMap<String, usize>,
    mux: Mux,
}

impl Instances {
    /// An empty cache over the host's `MakeInstance`.
    pub fn new(make: MakeInstance) -> Self {
        Instances {
            make,
            cache: HashMap::new(),
            failures: HashMap::new(),
            mux: Mux::new(),
        }
    }

    /// An entry's identity independent of WHERE it is used: the language
    /// id plus its options, what it loads (module, grammar file and its
    /// dialect, spec file or inline spec) and its routing scope, so two
    /// entries sharing a language id but loading different grammars never
    /// share an instance. `entryPrefix` in TypeScript, whose array is
    /// `[options, module, grammar, dialect, _scope]`, read from the
    /// entry's top level; a registry-normalized entry keeps what it loads
    /// under `load` instead, so there the three middle fields are always
    /// null and the prefix names no grammar (its tests pass because they
    /// build entries with a top-level `grammar`). This one reads the
    /// load, as the TypeScript comment intends, and adds the spec, which
    /// nothing else names. An inline spec is named by a hash of its JSON,
    /// so the key stays a key and not a grammar.
    ///
    /// The prefix ends in a space and the key appends the folder, so a
    /// key matches an entry's prefix exactly when it was made for that
    /// entry, whatever the folder ([`Instances::invalidate`]).
    pub fn entry_prefix(entry: &Entry) -> String {
        let (module, grammar, dialect, spec) = match entry.load() {
            Load::Module(name) => (Value::from(name), Value::Null, Value::Null, Value::Null),
            Load::Grammar(file) => {
                let dialect = Path::new(&file)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map_or(Value::Null, |ext| Value::from(ext.to_ascii_lowercase()));
                (Value::Null, Value::from(file), dialect, Value::Null)
            }
            Load::Spec(SpecSource::File(file)) => {
                (Value::Null, Value::Null, Value::Null, Value::from(file))
            }
            Load::Spec(SpecSource::Inline(value)) => {
                let mut hasher = DefaultHasher::new();
                value.to_string().hash(&mut hasher);
                let inline = json!({ "inline": format!("{:016x}", hasher.finish()) });
                (Value::Null, Value::Null, Value::Null, inline)
            }
        };
        let options = entry.options.clone().unwrap_or_else(|| json!({}));
        let scope = entry
            .routing_scope()
            .map_or(Value::Null, |folder| Value::from(path_string(folder)));
        let identity = Value::Array(vec![options, module, grammar, dialect, scope, spec]);
        format!("{} {} ", entry.language_id(), identity)
    }

    /// The cache and quarantine key: the prefix plus the folder (or the
    /// entry's sandbox folder). `key` in TypeScript; Go keys by language
    /// id alone, having no folders.
    pub fn key(entry: &Entry, folder: Option<&Path>) -> String {
        let folder = folder
            .or(entry.dir.as_deref())
            .map(path_string)
            .unwrap_or_default();
        format!("{}{folder}", Self::entry_prefix(entry))
    }

    /// Whether the entry has failed [`QUARANTINE_LIMIT`] times under this
    /// key. Quarantine is keyed as the cache is: one folder's broken
    /// grammar never disables another folder's working one under the same
    /// language id.
    pub fn quarantined(&self, entry: &Entry, folder: Option<&Path>) -> bool {
        self.failures_under(&Self::key(entry, folder)) >= QUARANTINE_LIMIT
    }

    fn failures_under(&self, key: &str) -> usize {
        self.failures.get(key).copied().unwrap_or(0)
    }

    /// Count one failure (a failed load or a panicking analysis).
    pub fn record_failure(&mut self, entry: &Entry, folder: Option<&Path>) {
        *self.failures.entry(Self::key(entry, folder)).or_insert(0) += 1;
    }

    /// How many failures the entry has under this key.
    pub fn failures(&self, entry: &Entry, folder: Option<&Path>) -> usize {
        self.failures_under(&Self::key(entry, folder))
    }

    /// The instance for an entry, built through `MakeInstance` (and the
    /// mux installed) on first use. `Ok(None)` when quarantined; a load
    /// failure is counted and returned, and the entry is tried again on
    /// the next call until the limit. A `MakeInstance` that panics is a
    /// load failure like any other (the TypeScript `makeInstance` throws,
    /// and `get` counts the throw before passing it on): it is counted
    /// and returned as a [`LoadError`] naming the panic, so one bad
    /// grammar cannot take the server down.
    pub fn get(
        &mut self,
        entry: &Entry,
        folder: Option<&Path>,
    ) -> Result<Option<Arc<Tabnas>>, LoadError> {
        let key = Self::key(entry, folder);
        if self.failures_under(&key) >= QUARANTINE_LIMIT {
            return Ok(None);
        }
        if let Some(inst) = self.cache.get(&key) {
            return Ok(Some(Arc::clone(inst)));
        }
        let made = catch_unwind(AssertUnwindSafe(|| (self.make)(entry))).unwrap_or_else(|panic| {
            Err(LoadError::new(format!(
                "grammar for {} panicked while loading: {}",
                entry.language_id(),
                panic_message(panic.as_ref())
            )))
        });
        let mut inst = match made {
            Ok(inst) => inst,
            Err(error) => {
                *self.failures.entry(key).or_insert(0) += 1;
                return Err(error);
            }
        };
        self.mux.install(&mut inst);
        let inst = Arc::new(inst);
        self.cache.insert(key, Arc::clone(&inst));
        Ok(Some(inst))
    }

    /// Grammar hot reload: drop THIS entry's instances under every folder
    /// (matched by prefix, not by language id) and clear their failure
    /// counts, so a reloaded grammar leaves quarantine. Rebuild, never
    /// re-apply: `grammar()` prepends.
    ///
    /// The prefix is this entry across every folder it is cached under,
    /// not every entry sharing its language id: matching on the language
    /// id alone released an unrelated folder's quarantined grammar back
    /// into service whenever another folder's grammar was edited.
    pub fn invalidate(&mut self, entry: &Entry) {
        let prefix = Self::entry_prefix(entry);
        self.cache.retain(|key, _| !key.starts_with(&prefix));
        self.failures.retain(|key, _| !key.starts_with(&prefix));
    }

    /// One parse with a collector active, serialized with every other
    /// parse through this cache: [`Mux::parse`]. Returns the recovery
    /// result and what the mux collected. `inst` is one this cache built
    /// (or one the host installed the [`Mux`] on); an instance without the
    /// mux parses fine and collects nothing.
    pub fn parse(&self, inst: &Tabnas, src: &str) -> (ParseRecovery, Collected) {
        self.mux.parse(inst, src)
    }

    /// Run `f` with NO collector active and the parse lock held, so the
    /// engine calls inside it (a `continuations` query parses internally)
    /// leak no events into any analysis: [`Mux::without_collector`].
    /// `WithParseLock` in Go.
    pub fn with_parse_lock<T>(&self, f: impl FnOnce() -> T) -> T {
        self.mux.without_collector(f)
    }

    /// The mux the cached instances write through.
    pub fn mux(&self) -> &Mux {
        &self.mux
    }

    /// The host's `MakeInstance`.
    pub fn make_instance(&self) -> &MakeInstance {
        &self.make
    }

    /// How many instances are cached.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

/// A folder as the key spells it.
fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The text a panic was raised with, when it was raised with text.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("a panic without a message")
}

impl std::fmt::Debug for Instances {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instances")
            .field("cached", &self.cache.len())
            .field("failures", &self.failures)
            .field("active", &self.mux.is_active())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tabnas::Options;

    use super::*;
    use crate::types::{RuleEventState, Scope};

    /// The shared pure-data strict-JSON grammar (design §13).
    const JSON_GRAMMAR: &str = include_str!("../../test/fixtures/json-grammar.json");

    /// The engine instance the pipeline uses: recovery on, the shared
    /// JSON grammar installed, no subscribers of its own.
    fn json_instance() -> Tabnas {
        let mut options = Options::default();
        options.parse.recover.enabled = true;
        let mut parser = Tabnas::with_options(options);
        parser
            .grammar_json(JSON_GRAMMAR)
            .expect("json-grammar.json installs");
        parser
    }

    fn json_maker() -> MakeInstance {
        Arc::new(|_entry| Ok(json_instance()))
    }

    /// An entry as the TypeScript tests build one: a language id, the
    /// grammar it loads, its sandbox folder and routing scope.
    fn entry(language_id: &str, spec: &str, dir: &str, scope: Scope) -> Entry {
        let mut entry = Entry::new(language_id);
        entry.language_id = Some(language_id.into());
        entry.options = Some(json!({}));
        entry.load = Some(Load::Spec(SpecSource::File(spec.into())));
        entry.dir = Some(PathBuf::from(dir));
        entry.scope = scope;
        entry
    }

    fn folder(path: &str) -> Option<&Path> {
        Some(Path::new(path))
    }

    #[test]
    fn the_mux_slot_begins_ends_and_restores() {
        let mux = Mux::new();
        assert!(!mux.is_active());
        let previous = mux.begin();
        assert_eq!(previous, None);
        assert!(mux.is_active());
        // A forwarder writes into the active slot.
        mux.lock()
            .as_mut()
            .expect("active")
            .rules
            .push(crate::types::RuleEvent {
                i: 1,
                name: "map".into(),
                state: RuleEventState::Open,
                forced: false,
                r: String::new(),
                o0: None,
                c0: None,
            });
        let collected = mux.end().expect("a collector was active");
        assert_eq!(collected.rules.len(), 1);
        assert!(!mux.is_active());
        // Nested: restoring puts the outer collector back.
        let outer = mux.begin();
        let inner_previous = mux.begin();
        assert!(inner_previous.is_some(), "the outer collector was active");
        mux.restore(inner_previous);
        assert!(mux.is_active());
        assert_eq!(outer, None);
    }

    #[test]
    fn a_new_cache_is_empty() {
        let instances = Instances::new(json_maker());
        assert!(instances.is_empty());
        assert_eq!(instances.len(), 0);
        assert!(!instances.mux().is_active());
        assert!(format!("{instances:?}").contains("cached: 0"));
    }

    #[test]
    fn the_mux_delivers_events_to_the_active_collector_only() {
        // The TypeScript case "the exported Instances class delivers
        // collector events": a mux that never fired shipped once, and
        // analyze() returned empty tokens and outline while the parse
        // succeeded.
        let mut instances = Instances::new(json_maker());
        let entry = entry("jsonf", "g.json", "/ws", Scope::Session);
        let inst = instances
            .get(&entry, None)
            .unwrap()
            .expect("not quarantined");

        let (recovery, collected) = instances.parse(&inst, "{\"a\":[1,2]}");
        assert!(recovery.fatal.is_none());
        assert!(recovery.errors.is_empty());
        assert!(
            !collected.lex.is_empty(),
            "no lex events reached the collector"
        );
        assert!(
            collected.lex.iter().all(|t| t.si <= 11),
            "engine byte offsets"
        );
        assert!(
            !collected.rules.is_empty(),
            "no ruleDone events reached the collector"
        );
        let map_open = collected
            .rules
            .iter()
            .find(|e| e.name == "map" && e.state == RuleEventState::Open)
            .expect("the map rule opens");
        assert_eq!(map_open.o0.as_ref().map(|t| t.src.as_str()), Some("{"));
        let map_close = collected
            .rules
            .iter()
            .find(|e| e.name == "map" && e.state == RuleEventState::Close)
            .expect("the map rule closes");
        assert_eq!(
            map_close.i, map_open.i,
            "open and close carry the rule's index"
        );
        assert_eq!(map_close.c0.as_ref().map(|t| t.src.as_str()), Some("}"));
        assert!(
            collected.rules.iter().all(|e| !e.forced),
            "a valid document forces nothing"
        );
        assert!(
            !instances.mux().is_active(),
            "the slot is empty after the parse"
        );

        // The next parse collects afresh: nothing carries over.
        let (_, again) = instances.parse(&inst, "[]");
        assert!(again.lex.len() < collected.lex.len());
        assert!(again.lex.iter().all(|t| t.si <= 2));

        // With no collector active the pair is a no-op: a direct engine
        // call, or one under the parse lock, leaks nothing anywhere.
        let direct = inst.parse_recover("{\"b\":true}");
        assert!(direct.fatal.is_none());
        let under_lock = instances.with_parse_lock(|| inst.parse_recover("[1]"));
        assert!(under_lock.fatal.is_none());
        assert!(!instances.mux().is_active());
        let (_, after) = instances.parse(&inst, "1");
        assert!(after.lex.iter().all(|t| t.src != "true" && t.src != "b"));
    }

    #[test]
    fn a_broken_document_still_reaches_the_collector() {
        let mut instances = Instances::new(json_maker());
        let entry = entry("jsonf", "g.json", "/ws", Scope::Session);
        let inst = instances.get(&entry, None).unwrap().unwrap();
        let (recovery, collected) = instances.parse(&inst, "{\"a\":[1,");
        assert!(!recovery.errors.is_empty() || recovery.fatal.is_some());
        assert!(!collected.lex.is_empty());
        assert!(collected.rules.iter().any(|e| e.name == "map"));
    }

    #[test]
    fn exactly_one_subscriber_pair_is_installed_per_instance() {
        let made = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&made);
        let make: MakeInstance = Arc::new(move |_entry| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(json_instance())
        });
        let mut instances = Instances::new(make);
        let entry = entry("jsonf", "g.json", "/ws", Scope::Session);
        let first = instances.get(&entry, None).unwrap().unwrap();
        let (_, once) = instances.parse(&first, "[1,2]");
        for _ in 0..3 {
            let again = instances.get(&entry, None).unwrap().unwrap();
            assert!(
                Arc::ptr_eq(&first, &again),
                "a second get rebuilt instead of hitting cache"
            );
        }
        assert_eq!(made.load(Ordering::SeqCst), 1);
        assert_eq!(instances.len(), 1);
        // The pair is the instance's only subscribers, however often it
        // is fetched or parsed with, and every event is recorded once.
        assert_eq!(first.lex_subscribers.len(), 1);
        assert_eq!(first.rule_done_subscribers.len(), 1);
        let (_, twice) = instances.parse(&first, "[1,2]");
        assert_eq!(twice, once, "a repeated parse records the same events once");
    }

    #[test]
    fn a_parse_restores_the_collector_that_was_active() {
        // The canonical `parse` keeps `prev` and puts it back: an outer
        // collector sees none of the inner parse's events.
        let mut instances = Instances::new(json_maker());
        let entry = entry("jsonf", "g.json", "/ws", Scope::Session);
        let inst = instances.get(&entry, None).unwrap().unwrap();
        assert_eq!(instances.mux().begin(), None);
        let (_, inner) = instances.parse(&inst, "[1]");
        assert!(!inner.lex.is_empty());
        let outer = instances.mux().end().expect("the outer collector is back");
        assert!(
            outer.lex.is_empty(),
            "the outer collector saw the inner parse"
        );
        assert!(outer.rules.is_empty());
        // `with_parse_lock` likewise restores an active collector.
        instances.mux().begin();
        instances.with_parse_lock(|| inst.parse_recover("[2]"));
        let outer = instances.mux().end().expect("the collector is back");
        assert!(
            outer.lex.is_empty(),
            "the locked parse leaked into the collector"
        );
    }

    #[test]
    fn an_unscoped_and_a_folder_scoped_entry_do_not_share_a_cache_slot() {
        // The TypeScript test of the same name: same language id, same
        // options, same _dir, different grammars (a session-wide
        // initializationOptions language and a folder manifest entry in
        // the folder that happens to be its _dir). Keyed on (languageId,
        // options, dir) they collided, and whichever document arrived
        // first decided which grammar served BOTH routes.
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&log);
        let make: MakeInstance = Arc::new(move |entry| {
            if let Some(Load::Spec(SpecSource::File(file))) = &entry.load {
                seen.lock().unwrap().push(file.clone());
            }
            Ok(json_instance())
        });
        let mut instances = Instances::new(make);
        let from_init = entry("mydsl", "init.json", "/ws/a", Scope::Session);
        let from_manifest = entry(
            "mydsl",
            "manifest.json",
            "/ws/a",
            Scope::Folder(PathBuf::from("/ws/a")),
        );

        let a = instances.get(&from_init, folder("/ws/a")).unwrap().unwrap();
        let b = instances
            .get(&from_manifest, folder("/ws/a"))
            .unwrap()
            .unwrap();
        assert!(
            !Arc::ptr_eq(&a, &b),
            "the scoped entry got the unscoped grammar"
        );
        assert_eq!(*log.lock().unwrap(), ["init.json", "manifest.json"]);

        // ...and each still caches on its own key.
        let again = instances.get(&from_init, folder("/ws/a")).unwrap().unwrap();
        assert!(Arc::ptr_eq(&a, &again));
        assert_eq!(
            log.lock().unwrap().len(),
            2,
            "a second get rebuilt instead of hitting cache"
        );
        assert_eq!(instances.len(), 2);
    }

    #[test]
    fn the_prefix_names_the_grammar_and_the_key_adds_the_folder() {
        let base = entry("mydsl", "a.json", "/ws/a", Scope::Session);
        let prefix = Instances::entry_prefix(&base);
        assert!(prefix.starts_with("mydsl ["), "{prefix}");
        assert!(prefix.ends_with("] "), "{prefix}");
        assert_eq!(Instances::entry_prefix(&base.clone()), prefix);

        // Each part of the identity splits the key.
        let mut other_spec = base.clone();
        other_spec.load = Some(Load::Spec(SpecSource::File("b.json".into())));
        assert_ne!(Instances::entry_prefix(&other_spec), prefix);
        let mut other_options = base.clone();
        other_options.options = Some(json!({"x": 1}));
        assert_ne!(Instances::entry_prefix(&other_options), prefix);
        let mut other_scope = base.clone();
        other_scope.scope = Scope::Folder(PathBuf::from("/ws/a"));
        assert_ne!(Instances::entry_prefix(&other_scope), prefix);
        let mut module = base.clone();
        module.load = Some(Load::Module("@tabnas/toml".into()));
        assert!(Instances::entry_prefix(&module).contains("\"@tabnas/toml\""));
        let mut grammar = base.clone();
        grammar.load = Some(Load::Grammar("./g/my.ABNF".into()));
        let grammar_prefix = Instances::entry_prefix(&grammar);
        assert!(grammar_prefix.contains("\"./g/my.ABNF\""));
        assert!(
            grammar_prefix.contains("\"abnf\""),
            "the dialect: {grammar_prefix}"
        );
        let mut inline = base.clone();
        inline.load = Some(Load::Spec(SpecSource::Inline(json!({"rule": {"val": {}}}))));
        let inline_prefix = Instances::entry_prefix(&inline);
        assert!(inline_prefix.contains("\"inline\""), "{inline_prefix}");
        assert!(
            !inline_prefix.contains("rule"),
            "an inline spec is hashed, not spelled out"
        );
        let mut other_inline = inline.clone();
        other_inline.load = Some(Load::Spec(SpecSource::Inline(json!({"rule": {"map": {}}}))));
        assert_ne!(Instances::entry_prefix(&other_inline), inline_prefix);
        assert_eq!(Instances::entry_prefix(&inline.clone()), inline_prefix);

        // A declared empty options object and none are the same identity.
        let mut no_options = base.clone();
        no_options.options = None;
        assert_eq!(Instances::entry_prefix(&no_options), prefix);
        // A Dir scope is the sandbox folder; a folder scope spelling the
        // same folder is the same identity.
        let mut dir_scoped = base.clone();
        dir_scoped.scope = Scope::Dir;
        let mut folder_scoped = base.clone();
        folder_scoped.scope = Scope::Folder(PathBuf::from("/ws/a"));
        assert_eq!(
            Instances::entry_prefix(&dir_scoped),
            Instances::entry_prefix(&folder_scoped)
        );

        // The key appends the folder, else the sandbox folder, else nothing.
        assert_eq!(
            Instances::key(&base, folder("/ws/b")),
            format!("{prefix}/ws/b")
        );
        assert_eq!(Instances::key(&base, None), format!("{prefix}/ws/a"));
        let mut no_dir = base.clone();
        no_dir.dir = None;
        assert_eq!(Instances::key(&no_dir, None), prefix);
    }

    #[test]
    fn quarantine_disables_a_repeatedly_failing_grammar() {
        // The TypeScript case of the same name: three failures, then
        // `get` answers null without trying again.
        let tried = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&tried);
        let make: MakeInstance = Arc::new(move |_entry| {
            counter.fetch_add(1, Ordering::SeqCst);
            Err(LoadError::new("boom"))
        });
        let mut instances = Instances::new(make);
        let bad = entry("bad", "bad.json", "/ws", Scope::Session);
        for i in 1..=QUARANTINE_LIMIT {
            assert!(
                !instances.quarantined(&bad, None),
                "quarantined after {} failures",
                i - 1
            );
            let error = match instances.get(&bad, None) {
                Err(error) => error,
                Ok(_) => panic!("the load fails"),
            };
            assert_eq!(error.message, "boom");
            assert_eq!(instances.failures(&bad, None), i);
        }
        assert!(instances.quarantined(&bad, None));
        for _ in 0..7 {
            assert!(
                matches!(instances.get(&bad, None), Ok(None)),
                "quarantined after repeated failures"
            );
        }
        assert_eq!(
            tried.load(Ordering::SeqCst),
            QUARANTINE_LIMIT,
            "a quarantined entry is not retried"
        );
        assert_eq!(instances.failures(&bad, None), QUARANTINE_LIMIT);
        assert!(instances.is_empty());
        assert!(format!("{instances:?}").contains("failures"));
    }

    #[test]
    fn a_recorded_failure_counts_like_a_failed_load() {
        // A panicking analysis is counted by the caller.
        let mut instances = Instances::new(json_maker());
        let entry = entry("jsonf", "g.json", "/ws/a", Scope::Session);
        let inst = instances.get(&entry, folder("/ws/a")).unwrap().unwrap();
        for _ in 0..QUARANTINE_LIMIT {
            instances.record_failure(&entry, folder("/ws/a"));
        }
        assert!(instances.quarantined(&entry, folder("/ws/a")));
        assert!(matches!(instances.get(&entry, folder("/ws/a")), Ok(None)));
        // Another folder's instance of the same entry is unaffected.
        assert!(!instances.quarantined(&entry, folder("/ws/b")));
        let other = instances.get(&entry, folder("/ws/b")).unwrap().unwrap();
        assert!(!Arc::ptr_eq(&inst, &other));
    }

    #[test]
    fn invalidating_one_entry_leaves_another_folder_quarantine_intact() {
        // The TypeScript test of the same name: quarantine is per (entry,
        // folder), and hot-reload used to clear every failure whose key
        // began with the language id, releasing a DIFFERENT folder's
        // quarantined grammar back into service.
        let broken = entry(
            "mydsl",
            "broken.json",
            "/ws/a",
            Scope::Folder(PathBuf::from("/ws/a")),
        );
        let healthy = entry(
            "mydsl",
            "healthy.json",
            "/ws/b",
            Scope::Folder(PathBuf::from("/ws/b")),
        );
        let make: MakeInstance = Arc::new(|entry| match &entry.load {
            Some(Load::Spec(SpecSource::File(file))) if file == "broken.json" => {
                Err(LoadError::new("bad grammar"))
            }
            _ => Ok(json_instance()),
        });
        let mut instances = Instances::new(make);

        // Drive the broken one into quarantine.
        for _ in 0..10 {
            let _ = instances.get(&broken, folder("/ws/a"));
        }
        assert!(
            instances.quarantined(&broken, folder("/ws/a")),
            "never quarantined"
        );
        assert!(instances.get(&healthy, folder("/ws/b")).unwrap().is_some());

        // An unrelated grammar changes in the OTHER folder.
        instances.invalidate(&healthy);
        assert!(
            instances.quarantined(&broken, folder("/ws/a")),
            "an unrelated folder's reload released the quarantined grammar"
        );
        assert!(
            instances.is_empty(),
            "the healthy instance was dropped for rebuild"
        );
    }

    #[test]
    fn invalidating_an_entry_clears_its_own_quarantine_and_instances() {
        // The converse, so the narrowing above cannot be "fixed" by never
        // clearing anything: reloading the grammar that failed must still
        // give it another chance.
        let broken = entry(
            "mydsl",
            "broken.json",
            "/ws/a",
            Scope::Folder(PathBuf::from("/ws/a")),
        );
        let make: MakeInstance = Arc::new(|_entry| Err(LoadError::new("bad grammar")));
        let mut instances = Instances::new(make);
        for _ in 0..10 {
            let _ = instances.get(&broken, folder("/ws/a"));
        }
        assert!(instances.quarantined(&broken, folder("/ws/a")));
        instances.invalidate(&broken);
        assert!(
            !instances.quarantined(&broken, folder("/ws/a")),
            "reloading the failing grammar did not clear its own quarantine"
        );
        assert_eq!(instances.failures(&broken, folder("/ws/a")), 0);

        // The same entry cached under several folders: every one is
        // dropped, and the next get rebuilds rather than re-applying.
        let made = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&made);
        let make: MakeInstance = Arc::new(move |_entry| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(json_instance())
        });
        let mut instances = Instances::new(make);
        let entry = entry("jsonf", "g.json", "/ws/a", Scope::Session);
        let a = instances.get(&entry, folder("/ws/a")).unwrap().unwrap();
        let b = instances.get(&entry, folder("/ws/b")).unwrap().unwrap();
        assert_eq!(instances.len(), 2);
        instances.invalidate(&entry);
        assert!(instances.is_empty());
        let a2 = instances.get(&entry, folder("/ws/a")).unwrap().unwrap();
        assert!(
            !Arc::ptr_eq(&a, &a2),
            "a reloaded grammar is a new instance"
        );
        assert!(!Arc::ptr_eq(&b, &a2));
        assert_eq!(made.load(Ordering::SeqCst), 3);
        // The old instance still parses (a caller may hold it), still
        // with its one pair, and the new one has exactly one too.
        assert_eq!(a.lex_subscribers.len(), 1);
        assert_eq!(a2.lex_subscribers.len(), 1);
        assert_eq!(a2.rule_done_subscribers.len(), 1);
    }

    #[test]
    fn a_panicking_make_instance_counts_toward_quarantine() {
        // The TypeScript `get` counts a throwing `makeInstance` before
        // passing the throw on; a panic is the Rust throw, and it must
        // not unwind through the server.
        let tried = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&tried);
        let make: MakeInstance = Arc::new(move |_entry| -> Result<Tabnas, LoadError> {
            counter.fetch_add(1, Ordering::SeqCst);
            panic!("the grammar crate is broken")
        });
        let mut instances = Instances::new(make);
        let bad = entry("bad", "bad.json", "/ws", Scope::Session);
        for i in 1..=QUARANTINE_LIMIT {
            let error = match instances.get(&bad, None) {
                Err(error) => error,
                Ok(_) => panic!("a panicking load is a failed load"),
            };
            assert!(
                error.message.contains("the grammar crate is broken"),
                "{}",
                error.message
            );
            assert!(error.message.contains("bad"), "{}", error.message);
            assert_eq!(instances.failures(&bad, None), i);
        }
        assert!(matches!(instances.get(&bad, None), Ok(None)));
        assert_eq!(tried.load(Ordering::SeqCst), QUARANTINE_LIMIT);
        // A panic raised with a formatted message is named too.
        let make: MakeInstance = Arc::new(|entry| -> Result<Tabnas, LoadError> {
            panic!("no grammar for {}", entry.language_id())
        });
        let mut instances = Instances::new(make);
        let error = instances.get(&bad, None).err().expect("a failed load");
        assert!(
            error.message.ends_with("no grammar for bad"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_panic_under_the_parse_lock_restores_the_collector_and_the_gate() {
        let mut instances = Instances::new(json_maker());
        let entry = entry("jsonf", "g.json", "/ws", Scope::Session);
        let inst = instances.get(&entry, None).unwrap().unwrap();
        assert_eq!(instances.mux().begin(), None);
        let unwound = catch_unwind(AssertUnwindSafe(|| {
            instances.with_parse_lock::<()>(|| panic!("a query failed"))
        }));
        assert!(unwound.is_err());
        // The collector that was active is back (TypeScript's `finally`)...
        let outer = instances.mux().end().expect("the outer collector is back");
        assert!(outer.lex.is_empty());
        // ...and the gate was left: another thread can parse.
        std::thread::scope(|scope| {
            let (_, collected) = scope
                .spawn(|| instances.parse(&inst, "[1]"))
                .join()
                .expect("the other thread parsed");
            assert!(!collected.lex.is_empty());
        });
    }

    #[test]
    fn a_parse_inside_the_parse_lock_collects_and_does_not_deadlock() {
        // The gate is re-entrant on its own thread: the canonical nesting
        // (`prev` saved and put back) works where Go's mutex would block.
        let mut instances = Instances::new(json_maker());
        let entry = entry("jsonf", "g.json", "/ws", Scope::Session);
        let inst = instances.get(&entry, None).unwrap().unwrap();
        let (recovery, collected) = instances.with_parse_lock(|| {
            assert!(!instances.mux().is_active());
            let inner = instances.parse(&inst, "[true]");
            assert!(
                !instances.mux().is_active(),
                "the lock's empty slot is back"
            );
            inner
        });
        assert!(recovery.errors.is_empty());
        assert!(collected.lex.iter().any(|t| t.src == "true"));
    }

    #[test]
    fn parses_on_two_threads_take_turns_and_collect_only_their_own_events() {
        // TypeScript serializes on its one thread and Go with `parseMu`;
        // a shared cache here serializes through the mux's gate. Without
        // it one thread's `begin` replaces the other's collector
        // mid-parse and the events land in the wrong analysis.
        let mut instances = Instances::new(json_maker());
        let entry = entry("jsonf", "g.json", "/ws", Scope::Session);
        let inst = instances.get(&entry, None).unwrap().unwrap();
        let instances = &instances;
        let rounds = 40;
        std::thread::scope(|scope| {
            let workers: Vec<_> = ["true", "false", "null"]
                .into_iter()
                .map(|word| {
                    let inst = Arc::clone(&inst);
                    scope.spawn(move || {
                        let src = format!("[{}]", vec![word; 50].join(","));
                        for _ in 0..rounds {
                            let (recovery, collected) = instances.parse(&inst, &src);
                            assert!(recovery.errors.is_empty());
                            let words = collected.lex.iter().filter(|t| t.src == word).count();
                            assert!(words >= 50, "{word}: {words} of its own tokens");
                            assert!(
                                collected.lex.iter().all(|t| {
                                    t.src == word
                                        || !["true", "false", "null"].contains(&t.src.as_str())
                                }),
                                "{word}: another thread's tokens were collected"
                            );
                            let lists = collected
                                .rules
                                .iter()
                                .filter(|e| e.name == "list" && e.state == RuleEventState::Close)
                                .count();
                            assert_eq!(lists, 1, "{word}: another thread's rules");
                        }
                    })
                })
                .collect();
            for worker in workers {
                worker.join().expect("a worker failed");
            }
        });
        assert!(!instances.mux().is_active());
    }

    #[test]
    fn a_poisoned_slot_does_not_stop_later_parses() {
        // A panic while the slot is held poisons its lock; the mux reads
        // through the poison so every later parse still collects.
        let mut instances = Instances::new(json_maker());
        let entry = entry("jsonf", "g.json", "/ws", Scope::Session);
        let inst = instances.get(&entry, None).unwrap().unwrap();
        let mux = instances.mux().clone();
        let poisoned = std::thread::spawn(move || {
            let _held = mux.slot.lock().unwrap();
            panic!("poison the slot");
        })
        .join();
        assert!(poisoned.is_err());
        assert!(instances.mux().slot.is_poisoned());
        let (recovery, collected) = instances.parse(&inst, "{\"a\":1}");
        assert!(recovery.errors.is_empty());
        assert!(!collected.lex.is_empty());
        assert!(!collected.rules.is_empty());
    }
}
