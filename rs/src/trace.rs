// Copyright (c) 2026 Richard Rodger, MIT License

//! The lex trace: what the engine announces to a `subscribe_lex`
//! subscriber, recorded, and reconciled into the token list the parse
//! used.
//!
//! The subscriber stream is a trace of lexer ACTIVITY, not a token list.
//! Under negotiated lexing a speculative recut fires an event that a
//! later failure retracts by re-announcing the restored token, and a
//! recovering parse announces the bad token it skipped. The documented
//! consumer contract (engine `ts/doc/api.md`, `tn.sub`) is: process the
//! events in order, keep the newest per source position, and let each
//! kept token's span shadow older events inside `[si, si + len)`.
//! [`reconcile`] is that contract; `ts/src/core.js` `reconcile` and
//! `go/semantic.go` `reconcile` are the same function.
//!
//! Positions are the Rust engine's: `si` is a UTF-8 BYTE offset into the
//! source and `len` a byte length, where the TypeScript engine counts
//! UTF-16 units and Go counts bytes too. The contract is identical; only
//! the unit differs, and it cancels out because the shadowing arithmetic
//! only ever compares one event's offsets with another's.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use tabnas::{Tabnas, Token};

/// The position slice of one token event, in engine units, plus the
/// token's name and source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenPoint {
    /// The token name (`#ST`, `#NR`, a grammar's `KW_if`, ...).
    pub name: String,
    /// 0-based UTF-8 byte offset of the token in the source.
    pub si: usize,
    /// 1-based row.
    pub ri: usize,
    /// 1-based column, in Unicode scalar values.
    pub ci: usize,
    /// UTF-8 byte length of `src`, as the engine reports it.
    pub len: usize,
    /// The token's source text.
    pub src: String,
}

impl TokenPoint {
    /// The slice of a token this trace keeps.
    pub fn of(token: &Token) -> TokenPoint {
        TokenPoint {
            name: token.name.as_str().to_string(),
            si: token.site.si,
            ri: token.site.ri,
            ci: token.site.ci,
            len: token.len,
            src: token.src.as_str().to_string(),
        }
    }

    /// The byte length this event claims when reconciling: never less
    /// than one, so that a zero-length event (end of source, an empty
    /// bad token) still occupies its position and shadows nothing
    /// beyond it. Mirrors `Math.max(1, t.len)` and `srcLenBytes`.
    pub fn span(&self) -> usize {
        self.len.max(1)
    }
}

/// A recorder for the engine's lex events, shared between the parser it
/// is installed on and the caller reading it back.
///
/// The engine has no unsubscribe API, so the recorder stays installed on
/// the parser for that parser's lifetime. That is why [`install`] takes
/// the parser by `&mut` and hands back a handle: parse, then read the
/// handle. Reuse the same parser and handle for the next parse (clearing
/// or taking the events in between) rather than installing a second
/// recorder, which would leave the first one recording into a buffer
/// nobody reads. [`crate::Highlighter`] does exactly that.
///
/// [`install`]: LexTrace::install
#[derive(Debug, Clone, Default)]
pub struct LexTrace {
    events: Arc<Mutex<Vec<TokenPoint>>>,
}

impl LexTrace {
    /// An empty, uninstalled trace. [`attach`](LexTrace::attach) it to a
    /// parser, or use [`install`](LexTrace::install), which does both.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a new recorder on `parser` and return its handle.
    pub fn install(parser: &mut Tabnas) -> Self {
        let trace = Self::new();
        trace.attach(parser);
        trace
    }

    /// Install this trace's recorder on `parser`. Every lex event whose
    /// token is real (not the engine's no-token sentinel) is recorded,
    /// ignored trivia and retractions included: the reconciliation needs
    /// all of them.
    pub fn attach(&self, parser: &mut Tabnas) {
        let recorder = self.clone();
        parser.subscribe_lex(move |token, _rule, _context| recorder.record(token));
    }

    fn record(&self, token: &Token) {
        // The TypeScript collector skips `sI < 0`, which is the no-token
        // sentinel; the Rust engine's sentinel is `tin == -1` with a zero
        // site, so the guard is on identity rather than position.
        if token.is_no_token() {
            return;
        }
        self.lock().push(TokenPoint::of(token));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<TokenPoint>> {
        // A panic while the lock was held (which the engine turns into
        // an `internal` error) must not turn every later parse into a
        // panic of its own; the events recorded so far are still sound.
        self.events.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A copy of the events recorded so far, in announcement order.
    pub fn events(&self) -> Vec<TokenPoint> {
        self.lock().clone()
    }

    /// The events recorded so far, leaving the trace empty.
    pub fn take(&self) -> Vec<TokenPoint> {
        std::mem::take(&mut *self.lock())
    }

    /// Forget every recorded event.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// The number of events recorded so far.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

/// Reconstruct the final token list from a lex trace, per the documented
/// contract: iterate newest-first, a claimed byte span `[si, si + span)`
/// shadows any older event starting inside it; the survivors are
/// returned sorted by position.
///
/// The claimed spans are kept as their union, disjoint intervals keyed
/// by start, so each event costs one ordered lookup (the interval with
/// the greatest start at or before it, if that reaches past it) rather
/// than a scan of every span claimed so far: N events reconcile in
/// `O(N log N)`. The scan was quadratic, and an ordinary in-order trace
/// is its worst case, since no claimed span ever matches an older event
/// there; it took seconds at 80k tokens and would take tens of minutes
/// on a large minified document.
pub fn reconcile(events: &[TokenPoint]) -> Vec<TokenPoint> {
    let mut claimed: BTreeMap<usize, usize> = BTreeMap::new();
    let mut out: Vec<TokenPoint> = Vec::new();
    for event in events.iter().rev() {
        let shadowed = claimed
            .range(..=event.si)
            .next_back()
            .is_some_and(|(_, &end)| event.si < end);
        if shadowed {
            continue;
        }
        claim(
            &mut claimed,
            event.si,
            event.si.saturating_add(event.span()),
        );
        out.push(event.clone());
    }
    out.sort_by_key(|event| event.si);
    out
}

/// Add `[start, end)` to the union of claimed spans, absorbing every
/// interval it touches so the map stays disjoint. `start` is never inside
/// an existing interval (the caller checked), so the one before it can
/// only touch it.
fn claim(claimed: &mut BTreeMap<usize, usize>, mut start: usize, mut end: usize) {
    if let Some((&s, &e)) = claimed.range(..=start).next_back() {
        if e >= start {
            claimed.remove(&s);
            start = s;
            end = end.max(e);
        }
    }
    while let Some((&s, &e)) = claimed.range(start..).next() {
        if s > end {
            break;
        }
        claimed.remove(&s);
        end = end.max(e);
    }
    claimed.insert(start, end);
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn point(name: &str, si: usize, src: &str) -> TokenPoint {
        TokenPoint {
            name: name.into(),
            si,
            ri: 1,
            ci: si.saturating_add(1),
            len: src.len(),
            src: src.into(),
        }
    }

    #[test]
    fn a_plain_trace_reconciles_to_itself_in_order() {
        let events = vec![
            point("#OB", 0, "{"),
            point("#ST", 1, "\"a\""),
            point("#CB", 4, "}"),
        ];
        assert_eq!(reconcile(&events), events);
    }

    #[test]
    fn the_newest_event_per_position_wins() {
        // A speculative recut of "ab" into "a" that a later failure
        // retracts by re-announcing the restored "ab" token.
        let events = vec![
            point("#LONG", 0, "ab"),
            point("#SHORT", 0, "a"),
            point("#B", 1, "b"),
            point("#LONG", 0, "ab"),
            point("#C", 2, "c"),
        ];
        let names: Vec<String> = reconcile(&events).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["#LONG", "#C"]);
    }

    #[test]
    fn a_kept_span_shadows_older_events_inside_it_but_not_after_it() {
        let events = vec![
            point("#A", 0, "a"),
            point("#B", 1, "b"),
            point("#C", 2, "c"),
            point("#AB", 0, "ab"),
        ];
        let names: Vec<String> = reconcile(&events).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["#AB", "#C"]);
    }

    #[test]
    fn a_zero_length_event_claims_one_byte() {
        let end = TokenPoint {
            name: "#ZZ".into(),
            si: 3,
            ri: 1,
            ci: 4,
            len: 0,
            src: String::new(),
        };
        assert_eq!(end.span(), 1);
        // Announced twice, kept once; the earlier real token survives.
        let events = vec![point("#NR", 0, "123"), end.clone(), end.clone()];
        assert_eq!(reconcile(&events), vec![point("#NR", 0, "123"), end]);
    }

    #[test]
    fn claimed_spans_shadow_as_a_union() {
        // Newest-first: `#C` claims [3, 4), then `#AB` claims [0, 5)
        // around it. `#D` at 4 is inside `#AB`, so it is shadowed even
        // though the claimed span nearest before it, `#C`, ends at 4: the
        // spans shadow as a union, not one at a time.
        let events = vec![
            point("#D", 4, "e"),
            point("#AB", 0, "abcde"),
            point("#C", 3, "d"),
        ];
        let names: Vec<String> = reconcile(&events).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["#AB", "#C"]);
        // Touching spans merge without closing the position after them.
        let events = vec![
            point("#GAP", 2, "c"),
            point("#A", 0, "a"),
            point("#B", 1, "b"),
            point("#E", 4, "e"),
        ];
        let names: Vec<String> = reconcile(&events).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["#A", "#B", "#GAP", "#E"]);
    }

    #[test]
    fn a_span_at_the_end_of_the_address_space_does_not_overflow() {
        // Only a hand-made event can sit there; it is kept, not a panic.
        let events = vec![point("#X", usize::MAX, "x"), point("#Y", 0, "y")];
        let sis: Vec<usize> = reconcile(&events).iter().map(|t| t.si).collect();
        assert_eq!(sis, [0, usize::MAX]);
    }

    #[test]
    fn a_long_trace_reconciles_in_one_pass() {
        // 200k in-order events, every one kept. The span scan this
        // replaces took 1.6 s at 80k in release and grows with the
        // square; the bound is generous and still far below it.
        let n = 200_000;
        let events: Vec<TokenPoint> = (0..n).map(|i| point("#NR", i, "1")).collect();
        let started = Instant::now();
        let out = reconcile(&events);
        let elapsed = started.elapsed();
        assert_eq!(out, events);
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }

    #[test]
    fn the_result_is_sorted_by_position() {
        let events = vec![point("#B", 2, "b"), point("#A", 0, "a")];
        let sis: Vec<usize> = reconcile(&events).iter().map(|t| t.si).collect();
        assert_eq!(sis, [0, 2]);
    }

    #[test]
    fn the_trace_records_takes_and_clears() {
        let trace = LexTrace::new();
        assert!(trace.is_empty());
        trace.lock().push(point("#NR", 0, "1"));
        assert_eq!(trace.len(), 1);
        assert_eq!(trace.events().len(), 1);
        assert_eq!(trace.take().len(), 1);
        assert!(trace.is_empty());
        trace.lock().push(point("#NR", 0, "1"));
        trace.clear();
        assert!(trace.is_empty());
    }
}
