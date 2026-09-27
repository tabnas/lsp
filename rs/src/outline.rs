// Copyright (c) 2026 Richard Rodger, MIT License

//! The outline (design §8): `ruleDone` events, forced closes synthesized
//! by recovery included, filtered by rule name and nested by span
//! containment into `DocumentSymbol`s. Mirrors `outline` in
//! `ts/src/core.js` (canonical) and `go/outline.go`.
//!
//! The derivation: an open-pass event whose rule the rules map names
//! (`map` Object, `list` Array by default; an entry's `outlineRules`
//! over that) opens a symbol at its first matched token; the matching
//! close-pass event (same rule index) ends it at ITS first matched
//! token's end, or, for a forced close with no token, at the open
//! token's end. Symbols are sorted by start (wider first on ties) and a
//! stack walk assigns parents. `kind` is 18 for Array, 19 for anything
//! else.
//!
//! Positions are the canonical `{line: rI - 1, character: cI - 1}`. The
//! TypeScript engine's `cI` counts UTF-16 units since its last column
//! reset; the Rust engine's `ci` counts scalar values since the same
//! reset. So a token's character is the wire length of the `ci - 1`
//! scalar values right BEFORE its byte offset, which reproduces the
//! TypeScript value wherever the engine reset the count, a lone `\r`
//! included (measuring the `\n`-delimited line from its start does not;
//! see `Columns` in [`crate::semantic`], which measures the same way).
//! The end is the end token's character plus its source's length in the
//! document's encoding, never less than one: the canonical `cI - 1 +
//! Math.max(1, len)`. Every column is measured in one forward pass over
//! the text, so a document on one long line (minified JSON) costs its
//! length plus a sort, not its length per symbol.
//!
//! A forced close carries no token, so its symbol ends where it began
//! and its nesting span is empty: an unterminated container's symbol
//! survives the broken document, but nothing nests inside it, and the
//! containers opened after it become its siblings. That is what the
//! canonical pipeline produces (`{"a":[1,2` gives an Object and an
//! Array side by side), and the fixture pins it.
//!
//! One bound the canonical pipeline does not have: symbols nested deeper
//! than [`MAX_OUTLINE_DEPTH`] are left out. A document is untrusted input
//! (design §10) and the engine parses containers nested tens of
//! thousands deep, while every derived operation on a symbol tree
//! (serializing it for the client, cloning, comparing, dropping)
//! recurses once per level: unbounded, a hostile document would overflow
//! the server's stack and abort it. The TypeScript server cannot send
//! such an outline either (`JSON.stringify` exceeds the call stack a few
//! thousand levels down: measured with this repo's Node, 1,000 levels
//! serialize and 5,000 do not), so below the bound the two outlines are
//! the same, and above it this one degrades to its top 256 levels where
//! the canonical server's reply fails outright.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::trace::TokenPoint;
use crate::types::{Doc, Entry, Position, PositionEncoding, Range, RuleEvent, RuleEventState};

/// Rule names to symbol labels, before an entry's `outlineRules`.
pub const DEFAULT_OUTLINE_RULES: [(&str, &str); 2] = [("map", "Object"), ("list", "Array")];

/// `SymbolKind.Array`.
pub const SYMBOL_KIND_ARRAY: u32 = 18;
/// `SymbolKind.Object`.
pub const SYMBOL_KIND_OBJECT: u32 = 19;

/// The deepest nesting the outline keeps: a root is at depth one, and a
/// symbol deeper than this is left out with everything inside it (see
/// the module docs). An outline this deep is past any use to a reader.
/// The bound is what keeps every recursive operation on the tree inside
/// a thread's stack: unoptimized, serializing a symbol takes about a
/// kilobyte of stack per level (measured: 1,500 levels fit a 2 MiB
/// thread, 2,000 do not), so 256 levels leave a server thread, or a test
/// thread, room several times over.
pub const MAX_OUTLINE_DEPTH: usize = 256;

/// An LSP hierarchical document symbol. `children` is always present,
/// empty for a leaf, as both canonical ports emit it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: u32,
    pub range: Range,
    pub selection_range: Range,
    #[serde(default)]
    pub children: Vec<DocumentSymbol>,
}

/// The rules map for an entry: the defaults, with the entry's
/// `outlineRules` laid over them.
pub fn outline_rules(entry: Option<&Entry>) -> HashMap<String, String> {
    let mut rules: HashMap<String, String> = DEFAULT_OUTLINE_RULES
        .iter()
        .map(|&(rule, label)| (rule.to_string(), label.to_string()))
        .collect();
    if let Some(overrides) = entry.and_then(|entry| entry.outline_rules.as_ref()) {
        rules.extend(
            overrides
                .iter()
                .map(|(rule, label)| (rule.clone(), label.clone())),
        );
    }
    rules
}

/// The `SymbolKind` for a label: 18 for `Array`, 19 for anything else,
/// as the canonical pipeline assigns them.
pub fn symbol_kind(label: &str) -> u32 {
    if label == "Array" {
        SYMBOL_KIND_ARRAY
    } else {
        SYMBOL_KIND_OBJECT
    }
}

/// Nested symbols from one parse's rule events.
pub fn outline(events: &[RuleEvent], entry: Option<&Entry>, doc: &Doc) -> Vec<DocumentSymbol> {
    let rules = outline_rules(entry);
    let mut open: HashMap<usize, &TokenPoint> = HashMap::new();
    // (label, open token, end token) per symbol, in close order.
    let mut closed: Vec<(&str, &TokenPoint, &TokenPoint)> = Vec::new();
    for event in events {
        let Some(label) = rules.get(&event.name) else {
            continue;
        };
        match event.state {
            RuleEventState::Open => {
                if let Some(o0) = &event.o0 {
                    open.insert(event.i, o0);
                }
            }
            RuleEventState::Close => {
                let Some(o0) = open.remove(&event.i) else {
                    continue;
                };
                // A forced close (recovery popped the rule) has no
                // matched token: the symbol ends at its open token.
                let end = match (&event.c0, event.forced) {
                    (Some(c0), _) => c0,
                    (None, true) => o0,
                    (None, false) => continue,
                };
                closed.push((label, o0, end));
            }
        }
    }

    // Every column in one forward pass: the tokens in offset order.
    let mut sites: Vec<(usize, &TokenPoint)> = Vec::with_capacity(closed.len() * 2);
    for (n, &(_, o0, end)) in closed.iter().enumerate() {
        sites.push((2 * n, o0));
        sites.push((2 * n + 1, end));
    }
    sites.sort_by_key(|&(_, token)| token.si);
    let mut positions = vec![Position::default(); sites.len()];
    let mut columns = Columns::new(doc);
    for &(slot, token) in &sites {
        positions[slot] = columns.position(token);
    }

    let symbols = closed
        .iter()
        .enumerate()
        .map(|(n, &(label, o0, end))| {
            let start = positions[2 * n];
            let end_start = positions[2 * n + 1];
            let len = encoded_len(&end.src, doc.encoding).max(1);
            Spanned {
                symbol: DocumentSymbol {
                    name: label.to_string(),
                    kind: symbol_kind(label),
                    range: Range::new(
                        start,
                        Position::new(
                            end_start.line,
                            end_start.character.saturating_add(wire(len)),
                        ),
                    ),
                    selection_range: Range::empty(start),
                    children: Vec::new(),
                },
                start: o0.si,
                end: end.si,
            }
        })
        .collect();
    nest(symbols)
}

/// A symbol with the byte span it nests by: the open token's offset and
/// the end token's offset (`_span` in TypeScript, `span` in Go). The
/// span compares only with other spans, so the unit (bytes here, UTF-16
/// units in TypeScript) cancels out.
struct Spanned {
    symbol: DocumentSymbol,
    start: usize,
    end: usize,
}

/// Nest by span containment: sort by start ascending, wider first on
/// ties (a stable sort, as `Array.prototype.sort` is), then a stack walk
/// assigns each symbol the nearest earlier symbol whose span contains
/// it. A symbol that would sit deeper than [`MAX_OUTLINE_DEPTH`] is left
/// out, and so is everything inside it. The tree is then assembled
/// without recursion: a parent always sorts before its children, so a
/// pass from the last symbol to the first finishes every child before
/// the parent that takes it.
fn nest(mut symbols: Vec<Spanned>) -> Vec<DocumentSymbol> {
    symbols.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let n = symbols.len();
    let mut kept = vec![false; n];
    let mut roots: Vec<usize> = Vec::new();
    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut stack: Vec<usize> = Vec::new();
    for (i, s) in symbols.iter().enumerate() {
        while let Some(&top) = stack.last() {
            let t = &symbols[top];
            if t.start <= s.start && s.end <= t.end {
                break;
            }
            stack.pop();
        }
        if stack.len() >= MAX_OUTLINE_DEPTH {
            continue;
        }
        match stack.last() {
            Some(&parent) => kids[parent].push(i),
            None => roots.push(i),
        }
        kept[i] = true;
        stack.push(i);
    }
    let mut slots: Vec<Option<DocumentSymbol>> = symbols
        .into_iter()
        .zip(kept)
        .map(|(s, kept)| kept.then_some(s.symbol))
        .collect();
    for i in (0..n).rev() {
        if kids[i].is_empty() {
            continue;
        }
        let children: Vec<DocumentSymbol> =
            kids[i].iter().filter_map(|&k| slots[k].take()).collect();
        if let Some(symbol) = slots[i].as_mut() {
            symbol.children = children;
        }
    }
    roots.iter().filter_map(|&r| slots[r].take()).collect()
}

/// The length of a token's source in the document's wire encoding, the
/// unit the end column is counted in.
fn encoded_len(text: &str, encoding: PositionEncoding) -> usize {
    match encoding {
        PositionEncoding::Utf16 => text.chars().map(char::len_utf16).sum(),
        PositionEncoding::Utf8 => text.len(),
    }
}

fn wire(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A byte offset in the text, with the scalar values and the wire units
/// before it.
#[derive(Clone, Copy, Default)]
struct Cursor {
    byte: usize,
    chars: usize,
    units: usize,
}

/// Token positions as wire positions, measured as the module docs say:
/// line `ri - 1` (within the text's lines), character the wire length
/// of the `ci - 1` scalar values before the token's byte offset (all of
/// them when fewer precede it).
///
/// Two cursors walk the text forwards, one at the token and one at the
/// start of the window its column measures, so tokens visited in offset
/// order cost one pass over the text. A token visited out of order is
/// measured by walking back from it: exact, but proportional to its
/// column.
struct Columns<'a> {
    doc: &'a Doc,
    lines: usize,
    /// At the current token's start.
    at: Cursor,
    /// At the start of the window the current token's column measures;
    /// never past `at`.
    from: Cursor,
}

impl<'a> Columns<'a> {
    fn new(doc: &'a Doc) -> Self {
        Columns {
            doc,
            lines: doc.line_starts().len(),
            at: Cursor::default(),
            from: Cursor::default(),
        }
    }

    fn units(&self, c: char) -> usize {
        match self.doc.encoding {
            PositionEncoding::Utf16 => c.len_utf16(),
            PositionEncoding::Utf8 => c.len_utf8(),
        }
    }

    fn position(&mut self, token: &TokenPoint) -> Position {
        let line = token.ri.saturating_sub(1).min(self.lines - 1);
        Position::new(wire(line), wire(self.column(token.si, token.ci)))
    }

    fn column(&mut self, si: usize, ci: usize) -> usize {
        let text = self.doc.text.as_str();
        let mut si = si.min(text.len());
        while !text.is_char_boundary(si) {
            si -= 1;
        }
        let want = ci.saturating_sub(1);
        if si < self.at.byte {
            return self.walk_back(si, want);
        }
        for c in text[self.at.byte..si].chars() {
            self.at.chars += 1;
            self.at.units += self.units(c);
        }
        self.at.byte = si;
        let start = self.at.chars.saturating_sub(want);
        if start < self.from.chars {
            return self.walk_back(si, want);
        }
        let mut rest = text[self.from.byte..].chars();
        while self.from.chars < start {
            let Some(c) = rest.next() else { break };
            self.from.byte += c.len_utf8();
            self.from.chars += 1;
            self.from.units += self.units(c);
        }
        self.at.units - self.from.units
    }

    /// The wire length of up to `want` scalar values before the char
    /// boundary `si`.
    fn walk_back(&self, si: usize, want: usize) -> usize {
        self.doc.text[..si]
            .chars()
            .rev()
            .take(want)
            .map(|c| self.units(c))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::TokenPoint;

    fn token(name: &str, si: usize, ri: usize, ci: usize, src: &str) -> TokenPoint {
        TokenPoint {
            name: name.into(),
            si,
            ri,
            ci,
            len: src.len(),
            src: src.into(),
        }
    }

    fn open(i: usize, name: &str, o0: TokenPoint) -> RuleEvent {
        RuleEvent {
            i,
            name: name.into(),
            state: RuleEventState::Open,
            forced: false,
            r: String::new(),
            o0: Some(o0),
            c0: None,
        }
    }

    fn close(i: usize, name: &str, c0: Option<TokenPoint>, forced: bool) -> RuleEvent {
        RuleEvent {
            i,
            name: name.into(),
            state: RuleEventState::Close,
            forced,
            r: String::new(),
            o0: None,
            c0,
        }
    }

    fn doc(text: &str) -> Doc {
        Doc::new("file:///t.jsonf", "jsonf", 1, text)
    }

    /// A name tree three levels deep: each root's name with its
    /// children's names and theirs.
    type Names = Vec<(String, Vec<(String, Vec<String>)>)>;

    /// The name tree, the shape the fixture pins.
    fn names(symbols: &[DocumentSymbol]) -> Names {
        symbols
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    s.children
                        .iter()
                        .map(|c| {
                            (
                                c.name.clone(),
                                c.children.iter().map(|g| g.name.clone()).collect(),
                            )
                        })
                        .collect(),
                )
            })
            .collect()
    }

    fn range(l0: u32, c0: u32, l1: u32, c1: u32) -> Range {
        Range::new(Position::new(l0, c0), Position::new(l1, c1))
    }

    #[test]
    fn the_rules_map_takes_the_entry_overrides() {
        let defaults = outline_rules(None);
        assert_eq!(defaults.get("map").map(String::as_str), Some("Object"));
        assert_eq!(defaults.get("list").map(String::as_str), Some("Array"));
        let mut entry = Entry::new("x");
        entry.outline_rules = Some(HashMap::from([
            ("map".to_string(), "Table".to_string()),
            ("section".to_string(), "Section".to_string()),
        ]));
        let rules = outline_rules(Some(&entry));
        assert_eq!(rules.get("map").map(String::as_str), Some("Table"));
        assert_eq!(rules.get("list").map(String::as_str), Some("Array"));
        assert_eq!(rules.get("section").map(String::as_str), Some("Section"));
        assert_eq!(symbol_kind("Array"), SYMBOL_KIND_ARRAY);
        assert_eq!(symbol_kind("Object"), SYMBOL_KIND_OBJECT);
        assert_eq!(symbol_kind("Table"), SYMBOL_KIND_OBJECT);
    }

    #[test]
    fn a_symbol_serializes_with_its_children_present() {
        let symbol = DocumentSymbol {
            name: "Object".into(),
            kind: SYMBOL_KIND_OBJECT,
            range: Range::default(),
            selection_range: Range::default(),
            children: Vec::new(),
        };
        let json = serde_json::to_value(&symbol).unwrap();
        assert_eq!(json["kind"], 19);
        assert!(json["children"].is_array());
        assert!(json.get("selectionRange").is_some());
    }

    #[test]
    fn a_container_nests_by_span_with_the_canonical_ranges() {
        // {"a":[1,2]} as the engine reports it: the map opens on `{`
        // (rule 1) and the list on `[` (rule 4); each closes on its
        // bracket. The ranges are the TypeScript pipeline's for this
        // document: Object 0..11 around Array 5..10, selection at the
        // start.
        let text = r#"{"a":[1,2]}"#;
        let events = vec![
            open(1, "map", token("#OB", 0, 1, 1, "{")),
            open(2, "pair", token("#ST", 1, 1, 2, "\"a\"")),
            open(4, "list", token("#OS", 5, 1, 6, "[")),
            open(5, "elem", token("#NR", 6, 1, 7, "1")),
            close(5, "elem", Some(token("#CA", 7, 1, 8, ",")), false),
            open(6, "elem", token("#NR", 8, 1, 9, "2")),
            close(6, "elem", None, false),
            close(4, "list", Some(token("#CS", 9, 1, 10, "]")), false),
            close(2, "pair", None, false),
            close(1, "map", Some(token("#CB", 10, 1, 11, "}")), false),
        ];
        let symbols = outline(&events, None, &doc(text));
        assert_eq!(
            names(&symbols),
            vec![("Object".to_string(), vec![("Array".to_string(), vec![])])]
        );
        let object = &symbols[0];
        assert_eq!(object.kind, SYMBOL_KIND_OBJECT);
        assert_eq!(object.range, range(0, 0, 0, 11));
        assert_eq!(object.selection_range, Range::empty(Position::new(0, 0)));
        let array = &object.children[0];
        assert_eq!(array.kind, SYMBOL_KIND_ARRAY);
        assert_eq!(array.range, range(0, 5, 0, 10));
        assert_eq!(array.selection_range, Range::empty(Position::new(0, 5)));
    }

    #[test]
    fn a_forced_close_ends_at_the_open_token_and_nests_nothing() {
        // {"a":[1,2 : recovery force-pops the list and the map at end of
        // source, with no close token for either. Each symbol ends at
        // its own open token, so the Array (span 5..5) is not inside the
        // Object (span 0..0): the canonical pipeline lists them side by
        // side, Object 0..1 and Array 5..6.
        let text = r#"{"a":[1,2"#;
        let events = vec![
            open(1, "map", token("#OB", 0, 1, 1, "{")),
            open(4, "list", token("#OS", 5, 1, 6, "[")),
            open(6, "elem", token("#NR", 8, 1, 9, "2")),
            close(6, "elem", None, true),
            close(4, "list", None, true),
            close(1, "map", None, true),
        ];
        let symbols = outline(&events, None, &doc(text));
        assert_eq!(
            names(&symbols),
            vec![
                ("Object".to_string(), vec![]),
                ("Array".to_string(), vec![])
            ]
        );
        assert_eq!(symbols[0].range, range(0, 0, 0, 1));
        assert_eq!(symbols[1].range, range(0, 5, 0, 6));

        // [[1,2],[3 : the inner completed Array (1..6) does not nest in
        // the force-popped outer one (span 0..0) either; three roots in
        // document order, as TypeScript produces.
        let text = "[[1,2],[3";
        let events = vec![
            open(1, "list", token("#OS", 0, 1, 1, "[")),
            open(2, "list", token("#OS", 1, 1, 2, "[")),
            close(2, "list", Some(token("#CS", 5, 1, 6, "]")), false),
            open(3, "list", token("#OS", 7, 1, 8, "[")),
            close(3, "list", None, true),
            close(1, "list", None, true),
        ];
        let symbols = outline(&events, None, &doc(text));
        let ranges: Vec<Range> = symbols.iter().map(|s| s.range).collect();
        assert_eq!(
            ranges,
            vec![range(0, 0, 0, 1), range(0, 1, 0, 6), range(0, 7, 0, 8)]
        );
        assert!(symbols.iter().all(|s| s.children.is_empty()));
    }

    #[test]
    fn a_close_without_a_token_that_was_not_forced_yields_no_symbol() {
        // A structural rule that closed on no token without recovery's
        // hand (an implicit close) has no end to give; the canonical
        // pipeline skips it. Its open stays consumed, so a later close
        // for the same index is ignored too.
        let events = vec![
            open(1, "map", token("#OB", 0, 1, 1, "{")),
            close(1, "map", None, false),
            close(1, "map", Some(token("#CB", 1, 1, 2, "}")), false),
        ];
        assert!(outline(&events, None, &doc("{}")).is_empty());
        // A close with no open, and an open with no token, are ignored.
        let events = vec![
            close(9, "map", Some(token("#CB", 1, 1, 2, "}")), false),
            RuleEvent {
                o0: None,
                ..open(1, "map", token("#OB", 0, 1, 1, "{"))
            },
            close(1, "map", Some(token("#CB", 1, 1, 2, "}")), false),
        ];
        assert!(outline(&events, None, &doc("{}")).is_empty());
    }

    #[test]
    fn only_the_rules_map_names_symbols() {
        // `val` and `pair` fire for every element; only `map` and `list`
        // are structural by default, and an entry's `outlineRules` can
        // rename and extend the map.
        let text = r#"{"a":1}"#;
        let events = vec![
            open(1, "map", token("#OB", 0, 1, 1, "{")),
            open(2, "pair", token("#ST", 1, 1, 2, "\"a\"")),
            open(3, "val", token("#NR", 5, 1, 6, "1")),
            close(3, "val", None, false),
            close(2, "pair", Some(token("#CB", 6, 1, 7, "}")), false),
            close(1, "map", Some(token("#CB", 6, 1, 7, "}")), false),
        ];
        let symbols = outline(&events, None, &doc(text));
        assert_eq!(names(&symbols), vec![("Object".to_string(), vec![])]);

        let mut entry = Entry::new("x");
        entry.outline_rules = Some(HashMap::from([
            ("map".to_string(), "Table".to_string()),
            ("pair".to_string(), "Pair".to_string()),
        ]));
        let symbols = outline(&events, Some(&entry), &doc(text));
        assert_eq!(
            names(&symbols),
            vec![("Table".to_string(), vec![("Pair".to_string(), vec![])])]
        );
        assert_eq!(symbols[0].kind, SYMBOL_KIND_OBJECT);
        assert_eq!(symbols[0].children[0].range, range(0, 1, 0, 7));
    }

    #[test]
    fn deep_nesting_and_ties_follow_the_canonical_order() {
        // {"a":{"b":[1,{"c":2}]},"d":[[3]]}: four levels, two roots'
        // worth of children, the TypeScript ranges.
        let text = r#"{"a":{"b":[1,{"c":2}]},"d":[[3]]}"#;
        let events = vec![
            open(1, "map", token("#OB", 0, 1, 1, "{")),
            open(2, "map", token("#OB", 5, 1, 6, "{")),
            open(3, "list", token("#OS", 10, 1, 11, "[")),
            open(4, "map", token("#OB", 13, 1, 14, "{")),
            close(4, "map", Some(token("#CB", 19, 1, 20, "}")), false),
            close(3, "list", Some(token("#CS", 20, 1, 21, "]")), false),
            close(2, "map", Some(token("#CB", 21, 1, 22, "}")), false),
            open(5, "list", token("#OS", 27, 1, 28, "[")),
            open(6, "list", token("#OS", 28, 1, 29, "[")),
            close(6, "list", Some(token("#CS", 30, 1, 31, "]")), false),
            close(5, "list", Some(token("#CS", 31, 1, 32, "]")), false),
            close(1, "map", Some(token("#CB", 32, 1, 33, "}")), false),
        ];
        let symbols = outline(&events, None, &doc(text));
        assert_eq!(symbols.len(), 1);
        let root = &symbols[0];
        assert_eq!(root.range, range(0, 0, 0, 33));
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].name, "Object");
        assert_eq!(root.children[0].range, range(0, 5, 0, 22));
        assert_eq!(root.children[0].children[0].range, range(0, 10, 0, 21));
        assert_eq!(
            root.children[0].children[0].children[0].range,
            range(0, 13, 0, 20)
        );
        assert_eq!(root.children[1].name, "Array");
        assert_eq!(root.children[1].range, range(0, 27, 0, 32));
        assert_eq!(root.children[1].children[0].range, range(0, 28, 0, 31));

        // Events arriving out of document order still nest by span: the
        // sort is on the spans, not on arrival. Each open still precedes
        // its close, as it must for a symbol to exist.
        let reordered = vec![
            events[7].clone(),
            events[8].clone(),
            events[9].clone(),
            events[10].clone(),
            events[0].clone(),
            events[1].clone(),
            events[2].clone(),
            events[3].clone(),
            events[4].clone(),
            events[5].clone(),
            events[6].clone(),
            events[11].clone(),
        ];
        assert_eq!(
            names(&outline(&reordered, None, &doc(text))),
            names(&symbols)
        );

        // Two symbols with the same start and different ends: the wider
        // one is the parent (an outline rule for both `map` and the
        // `pair` that opens on the same token would produce this).
        let same_start = vec![
            open(1, "map", token("#OB", 0, 1, 1, "{")),
            open(2, "list", token("#OB", 0, 1, 1, "{")),
            close(2, "list", Some(token("#CA", 3, 1, 4, ",")), false),
            close(1, "map", Some(token("#CB", 6, 1, 7, "}")), false),
        ];
        let symbols = outline(&same_start, None, &doc("{1,2,3}"));
        assert_eq!(
            names(&symbols),
            vec![("Object".to_string(), vec![("Array".to_string(), vec![])])]
        );
    }

    #[test]
    fn multi_line_and_multi_byte_ranges_are_wire_positions() {
        // {"a":\n[1,\n2]\n} : the Array opens on line 1 and closes on
        // line 2; the Object closes on line 3. TypeScript: Object
        // (0,0)..(3,1), Array (1,0)..(2,2).
        let text = "{\"a\":\n[1,\n2]\n}";
        let events = vec![
            open(1, "map", token("#OB", 0, 1, 1, "{")),
            open(4, "list", token("#OS", 6, 2, 1, "[")),
            close(4, "list", Some(token("#CS", 11, 3, 2, "]")), false),
            close(1, "map", Some(token("#CB", 13, 4, 1, "}")), false),
        ];
        let symbols = outline(&events, None, &doc(text));
        assert_eq!(symbols[0].range, range(0, 0, 3, 1));
        assert_eq!(symbols[0].children[0].range, range(1, 0, 2, 2));

        // An astral character before the container: the inner `[` is
        // engine column 6, five scalar values in, and they are six UTF-16
        // units; the `]`s end one unit past theirs. TypeScript: outer
        // (0,0)..(0,9), inner (0,6)..(0,8). In UTF-8 the same positions
        // are bytes.
        let text = "[\"\u{1D11E}\",[]]";
        let events = vec![
            open(1, "list", token("#OS", 0, 1, 1, "[")),
            open(2, "list", token("#OS", 8, 1, 6, "[")),
            close(2, "list", Some(token("#CS", 9, 1, 7, "]")), false),
            close(1, "list", Some(token("#CS", 10, 1, 8, "]")), false),
        ];
        let symbols = outline(&events, None, &doc(text));
        assert_eq!(symbols[0].range, range(0, 0, 0, 9));
        assert_eq!(symbols[0].children[0].range, range(0, 6, 0, 8));
        let bytes = doc(text).with_encoding(PositionEncoding::Utf8);
        let symbols = outline(&events, None, &bytes);
        assert_eq!(symbols[0].range, range(0, 0, 0, 11));
        assert_eq!(symbols[0].children[0].range, range(0, 8, 0, 10));

        // After a lone `\r` the engine counts columns again from one, on
        // the same line: `[1,\r[2]]` puts the inner list at engine column
        // 1, which is character 0, as TypeScript reports it.
        let text = "[1,\r[2]]";
        let events = vec![
            open(1, "list", token("#OS", 0, 1, 1, "[")),
            open(2, "list", token("#OS", 4, 1, 1, "[")),
            close(2, "list", Some(token("#CS", 6, 1, 3, "]")), false),
            close(1, "list", Some(token("#CS", 7, 1, 4, "]")), false),
        ];
        let symbols = outline(&events, None, &doc(text));
        assert_eq!(symbols[0].range, range(0, 0, 0, 4));
        assert_eq!(symbols[0].children[0].range, range(0, 0, 0, 3));

        // A multi-unit end token counts every unit, and an empty one
        // still ends one past its start.
        let events = vec![
            open(1, "map", token("KW_begin", 0, 1, 1, "begin")),
            close(
                1,
                "map",
                Some(token("KW_end", 8, 1, 9, "\u{1D11E}nd")),
                false,
            ),
        ];
        let symbols = outline(&events, None, &doc("begin 1 \u{1D11E}nd"));
        assert_eq!(symbols[0].range, range(0, 0, 0, 12));
        let events = vec![
            open(1, "map", token("#OB", 0, 1, 1, "{")),
            close(1, "map", Some(token("#ZZ", 1, 1, 2, "")), false),
        ];
        let symbols = outline(&events, None, &doc("{"));
        assert_eq!(symbols[0].range, range(0, 0, 0, 2));
    }

    #[test]
    fn nesting_is_bounded_and_a_long_line_is_measured_once() {
        // MAX_OUTLINE_DEPTH + 500 lists, each inside the previous, on one
        // line, then a second root after them: the chain is cut at the
        // bound, the second root survives, and the columns cost one pass
        // over the line (a walk from each token's line start would be
        // quadratic here).
        let depth = MAX_OUTLINE_DEPTH + 500;
        let text = "[".repeat(depth) + &"]".repeat(depth) + "[]";
        let mut events = Vec::with_capacity(depth * 2 + 2);
        for i in 0..depth {
            events.push(open(i, "list", token("#OS", i, 1, i + 1, "[")));
        }
        for i in (0..depth).rev() {
            let si = 2 * depth - 1 - i;
            events.push(close(
                i,
                "list",
                Some(token("#CS", si, 1, si + 1, "]")),
                false,
            ));
        }
        let at = 2 * depth;
        events.push(open(depth, "list", token("#OS", at, 1, at + 1, "[")));
        events.push(close(
            depth,
            "list",
            Some(token("#CS", at + 1, 1, at + 2, "]")),
            false,
        ));
        let symbols = outline(&events, None, &doc(&text));
        assert_eq!(symbols.len(), 2);
        let mut level = &symbols[0];
        let mut counted = 1;
        while let Some(child) = level.children.first() {
            assert_eq!(level.children.len(), 1);
            level = child;
            counted += 1;
        }
        assert_eq!(counted, MAX_OUTLINE_DEPTH);
        // The deepest kept symbol has its own range, not a cut one.
        let d = MAX_OUTLINE_DEPTH as u32;
        assert_eq!(level.range, range(0, d - 1, 0, 2 * depth as u32 - d + 1));
        assert_eq!(symbols[1].range, range(0, at as u32, 0, at as u32 + 2));
        // What the bound is for: every derived operation on the tree
        // recurses once per level, and at the bound each one fits in a
        // test thread's stack (2 MiB, unoptimized).
        let json = serde_json::to_string(&symbols).expect("the outline serializes");
        assert!(json.starts_with("[{\"name\":\"Array\""));
        let copy = symbols.clone();
        assert_eq!(copy, symbols);
        assert!(!format!("{copy:?}").is_empty());
    }

    /// The column of each token by walking back from it: the definition
    /// the one-pass measure must agree with.
    fn walked(doc: &Doc, si: usize, ci: usize) -> usize {
        let mut si = si.min(doc.text.len());
        while !doc.text.is_char_boundary(si) {
            si -= 1;
        }
        let back = doc.text[..si].chars().rev().take(ci.saturating_sub(1));
        match doc.encoding {
            PositionEncoding::Utf16 => back.map(char::len_utf16).sum(),
            PositionEncoding::Utf8 => back.map(char::len_utf8).sum(),
        }
    }

    #[test]
    fn one_pass_columns_agree_with_walking_back_in_any_order() {
        let text = "a\u{1D11E}\u{e9}\r\nb\u{65e5}\r c\n\u{1F600}\u{1D11E}xyz\r\rq";
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        // A deterministic spread of sites: every boundary with columns
        // that fit, overshoot (more than precede the token) and point
        // inside a character, visited sorted, reversed and shuffled.
        let mut sites: Vec<(usize, usize)> = Vec::new();
        for (n, &si) in boundaries.iter().enumerate() {
            for ci in [1, 2, 3, n + 1, n + 5] {
                sites.push((si, ci));
            }
            sites.push((si + 1, 2));
        }
        sites.push((text.len() + 10, 3));
        let mut seed = 7usize;
        let mut shuffled = sites.clone();
        for i in (1..shuffled.len()).rev() {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345) % (1 << 31);
            shuffled.swap(i, seed % (i + 1));
        }
        let mut sorted = sites.clone();
        sorted.sort();
        let reversed: Vec<(usize, usize)> = sorted.iter().rev().copied().collect();
        for encoding in [PositionEncoding::Utf16, PositionEncoding::Utf8] {
            let doc = doc(text).with_encoding(encoding);
            for order in [&sorted, &reversed, &shuffled] {
                let mut columns = Columns::new(&doc);
                for &(si, ci) in order.iter() {
                    assert_eq!(
                        columns.column(si, ci),
                        walked(&doc, si, ci),
                        "{encoding:?} si {si} ci {ci}"
                    );
                }
            }
        }
    }
}
