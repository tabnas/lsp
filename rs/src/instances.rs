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
//! Parses are serialized (the protocol layer handles one message at a
//! time), so a single active slot is the whole demux; a parse run with
//! no collector active (a `continuations` query) makes the mux a no-op.
//!
//! A collector must never abort a user's parse: the forwarders guard
//! against a panic in their own bookkeeping the way Go's `recover()`
//! does, and a grammar whose `MakeInstance` fails or whose parse panics
//! is counted toward [`QUARANTINE_LIMIT`]; at the limit the entry is
//! disabled and the server survives.
//!
//! Status: [`Mux`]'s slot handling and [`Instances::new`] are complete;
//! the cache, keys, quarantine, `install` and `parse` are signatures for
//! the instances module agent.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use tabnas::{ParseRecovery, Tabnas};

use crate::types::{Collected, Entry, LoadError, MakeInstance};

/// After this many failures an entry is quarantined (`QUARANTINE_LIMIT`
/// in `ts/src/instances.js`, `QuarantineLimit` in Go).
pub const QUARANTINE_LIMIT: usize = 3;

/// The one permanent subscriber pair's shared state: the slot the
/// current parse's [`Collected`] sits in while the parse runs. Cloning a
/// `Mux` clones the handle, not the slot, which is how the two engine
/// subscribers and the [`Instances`] that drives them share it.
#[derive(Debug, Clone, Default)]
pub struct Mux {
    slot: Arc<Mutex<Option<Collected>>>,
}

impl Mux {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the pair on `parser`: a `subscribe_lex` forwarder that
    /// records every real token event (not the engine's no-token
    /// sentinel) as a [`crate::TokenPoint`], and a `subscribe_rule_done`
    /// forwarder that records the rule's index, name, pass, `forced`,
    /// the alternate's `r`, and the first matched open and close tokens
    /// (`rule.o[0]`, `rule.c[0]`). Both write into the active slot and
    /// do nothing when it is empty. Called exactly once per instance, by
    /// [`Instances::get`].
    #[allow(unused_variables)] // stub
    pub fn install(&self, parser: &mut Tabnas) {
        todo!("instances::Mux::install: the two forwarders, ts/src/instances.js installMux")
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

    /// The slot, for the forwarders. A poisoned lock (a panic while a
    /// forwarder held it) yields the data anyway: the events recorded so
    /// far are sound, and every later parse must not panic in turn.
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Option<Collected>> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
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
    /// id plus its options, module, grammar, dialect and routing scope,
    /// so two entries sharing a language id but loading different
    /// grammars never share an instance. `entryPrefix` in TypeScript.
    #[allow(unused_variables)] // stub
    pub fn entry_prefix(entry: &Entry) -> String {
        todo!("instances::Instances::entry_prefix")
    }

    /// The cache and quarantine key: the prefix plus the folder (or the
    /// entry's sandbox folder). `key` in TypeScript; Go keys by language
    /// id alone, having no folders.
    #[allow(unused_variables)] // stub
    pub fn key(entry: &Entry, folder: Option<&Path>) -> String {
        todo!("instances::Instances::key")
    }

    /// Whether the entry has failed [`QUARANTINE_LIMIT`] times under this
    /// key.
    #[allow(unused_variables)] // stub
    pub fn quarantined(&self, entry: &Entry, folder: Option<&Path>) -> bool {
        todo!("instances::Instances::quarantined")
    }

    /// Count one failure (a failed load or a panicking analysis).
    #[allow(unused_variables)] // stub
    pub fn record_failure(&mut self, entry: &Entry, folder: Option<&Path>) {
        todo!("instances::Instances::record_failure")
    }

    /// The instance for an entry, built through `MakeInstance` (and the
    /// mux installed) on first use. `Ok(None)` when quarantined; a load
    /// failure is counted and returned.
    #[allow(unused_variables)] // stub
    pub fn get(
        &mut self,
        entry: &Entry,
        folder: Option<&Path>,
    ) -> Result<Option<Arc<Tabnas>>, LoadError> {
        todo!("instances::Instances::get")
    }

    /// Grammar hot reload: drop THIS entry's instances under every folder
    /// (matched by prefix, not by language id) and clear their failure
    /// counts, so a reloaded grammar leaves quarantine. Rebuild, never
    /// re-apply: `grammar()` prepends.
    #[allow(unused_variables)] // stub
    pub fn invalidate(&mut self, entry: &Entry) {
        todo!("instances::Instances::invalidate")
    }

    /// One parse with a collector active: begin, `parse_recover`, end,
    /// restore. Returns the recovery result and what the mux collected.
    #[allow(unused_variables)] // stub
    pub fn parse(&self, inst: &Tabnas, src: &str) -> (ParseRecovery, Collected) {
        todo!("instances::Instances::parse")
    }

    /// Run `f` with NO collector active, so the engine calls inside it (a
    /// `continuations` query parses internally) leak no events into any
    /// analysis. `WithParseLock` in Go.
    #[allow(unused_variables)] // stub
    pub fn with_parse_lock<T>(&self, f: impl FnOnce() -> T) -> T {
        todo!("instances::Instances::with_parse_lock")
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
    use super::*;

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
                state: crate::types::RuleEventState::Open,
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
        let make: MakeInstance = Arc::new(|_entry| Ok(Tabnas::new()));
        let instances = Instances::new(make);
        assert!(instances.is_empty());
        assert_eq!(instances.len(), 0);
        assert!(!instances.mux().is_active());
        assert!(format!("{instances:?}").contains("cached: 0"));
    }
}
