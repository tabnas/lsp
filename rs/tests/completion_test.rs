// Completion (design §8) over the shared pure-data grammar: the items at
// the start, in the middle and at the end of a document, in the middle of
// a token (inside a string, a number, a keyword), across lines and over
// multi-byte text, in both position encodings, and the lock the query
// runs under.
//
// Every expected list below is the TypeScript core's answer, items and
// order: `core.completion` in ts/src/core.js over the same grammar, the
// instance built as ts/test/conformance.test.js builds it, run with node
// and transcribed, never written by hand. A mismatch is a defect in this
// port (or an engine divergence to fix in parser), never a table update.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use tabnas::{Options, Tabnas};
use tabnas_lsp::completion::{
    fixed_source, COMPLETION_KIND_KEYWORD, COMPLETION_KIND_OPERATOR, TRIGGER_CHARACTERS,
};
use tabnas_lsp::{
    completion, CompletionItem, Doc, Entry, Instances, MakeInstance, Position, PositionEncoding,
};

fn grammar() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("test")
        .join("fixtures")
        .join("json-grammar.json");
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The instance the pipeline uses: recovery on, the shared grammar.
fn instance() -> Tabnas {
    let mut options = Options::default();
    options.parse.recover.enabled = true;
    let mut parser = Tabnas::with_options(options);
    parser
        .grammar_json(&grammar())
        .expect("json-grammar.json installs");
    parser
}

fn entry() -> Entry {
    let mut entry = Entry::new("jsonf");
    entry.language_id = Some("jsonf".into());
    entry.extensions = vec![".jsonf".into()];
    entry.grammar_kind = Some("data".into());
    entry
}

/// An instance cache over the grammar, and its instance for the entry,
/// with the mux installed.
fn stack() -> (Instances, Arc<Tabnas>, Entry) {
    let make: MakeInstance = Arc::new(|_entry| Ok(instance()));
    let mut instances = Instances::new(make);
    let entry = entry();
    let inst = instances
        .get(&entry, None)
        .expect("the grammar loads")
        .expect("a fresh entry is not quarantined");
    (instances, inst, entry)
}

fn doc(text: &str) -> Doc {
    Doc::new("file:///t.jsonf", "jsonf", 1, text)
}

/// A token completed by its name: `CompletionItemKind.Keyword`.
fn kw(name: &str) -> CompletionItem {
    CompletionItem {
        label: name.into(),
        kind: COMPLETION_KIND_KEYWORD,
        detail: Some(name.into()),
        insert_text: None,
    }
}

/// A fixed token completed by its source: `CompletionItemKind.Operator`.
fn op(source: &str, name: &str) -> CompletionItem {
    CompletionItem {
        label: source.into(),
        kind: COMPLETION_KIND_OPERATOR,
        detail: Some(name.into()),
        insert_text: Some(source.into()),
    }
}

/// What may begin a document: the start rule's openers.
fn openers() -> Vec<CompletionItem> {
    vec![
        kw("#NR"),
        kw("#ST"),
        kw("#VL"),
        op("{", "#OB"),
        op("[", "#OS"),
    ]
}

/// What may follow a colon or a separator: a value, and the closers and
/// separators the pop-closure over the rule stack admits.
fn after_a_colon() -> Vec<CompletionItem> {
    vec![
        kw("#NR"),
        kw("#ST"),
        kw("#VL"),
        op("{", "#OB"),
        op("}", "#CB"),
        op("[", "#OS"),
        op(",", "#CA"),
    ]
}

fn in_a_list() -> Vec<CompletionItem> {
    vec![
        kw("#NR"),
        kw("#ST"),
        kw("#VL"),
        op("{", "#OB"),
        op("[", "#OS"),
        op("]", "#CS"),
        op(",", "#CA"),
    ]
}

/// A cursor, `(line, character)`.
type Cursor = (u32, u32);

/// A case: its name, the document, the cursor (UTF-16), the TypeScript
/// items in order.
type Case = (&'static str, &'static str, Cursor, Vec<CompletionItem>);

/// A case in both encodings: the document, the cursor in UTF-16 units,
/// the same cursor in UTF-8 bytes, and what is expected there.
type Encoded<T> = (&'static str, Cursor, Cursor, T);

fn check(instances: Option<&Instances>, inst: &Tabnas, entry: &Entry, cases: &[Case]) {
    for (name, input, (line, character), want) in cases {
        let got = completion(
            instances,
            inst,
            entry,
            &doc(input),
            Position::new(*line, *character),
        );
        assert_eq!(&got, want, "{name}: {input:?} at {line}:{character}");
    }
}

fn run(cases: &[Case]) {
    let (instances, inst, entry) = stack();
    check(Some(&instances), &inst, &entry, cases);
}

#[test]
fn at_the_start_of_a_document_the_start_rule_openers() {
    run(&[
        ("an empty document", "", (0, 0), openers()),
        ("before an object", "{\"a\":1}", (0, 0), openers()),
        ("before a scalar", "1", (0, 0), openers()),
        // A line past the last is the last line, in both stores.
        ("a line past the end", "[1,", (5, 0), openers()),
    ]);
}

#[test]
fn in_the_middle_of_a_document_what_the_prefix_admits() {
    let value = vec![kw("#NR"), kw("#ST"), kw("#VL")];
    run(&[
        ("after an opening brace", "{\"a\":1}", (0, 1), value.clone()),
        (
            "after a value",
            "{\"a\":1,\"b\":2}",
            (0, 6),
            vec![op("}", "#CB"), op(",", "#CA")],
        ),
        (
            "after a separator",
            "{\"a\":1,\"b\":2}",
            (0, 7),
            value.clone(),
        ),
        ("after whitespace", "[1, ", (0, 4), in_a_list()),
        (
            "on a later line",
            "{\n  \"a\": 1,\n  \"b\": 2\n}",
            (2, 2),
            value,
        ),
    ]);
}

#[test]
fn at_the_end_of_a_document() {
    let (instances, inst, entry) = stack();
    // A complete document: the engine answers with end-of-source alone,
    // a sentinel, so there is nothing to offer.
    assert_eq!(inst.continuations("{\"a\":1}").tokens, ["#ZZ"]);
    check(
        Some(&instances),
        &inst,
        &entry,
        &[
            ("a complete object", "{\"a\":1}", (0, 7), vec![]),
            ("a complete scalar", "1", (0, 1), vec![]),
            (
                "an unclosed object",
                "{\"a\":1",
                (0, 6),
                vec![op("}", "#CB"), op(",", "#CA")],
            ),
            ("after a colon", "{\n  \"a\": ", (1, 7), after_a_colon()),
            (
                "after a bad token",
                "[1 @",
                (0, 4),
                vec![op("]", "#CS"), op(",", "#CA")],
            ),
            ("trailing closers", "}}", (0, 2), openers()),
        ],
    );
}

#[test]
fn inside_a_string_the_prefix_ends_in_the_string() {
    // A cursor inside a string cuts the prefix inside it, so the engine
    // is asked about an unterminated string, and the engines disagree on
    // the answer (see the registered divergence below). What this port
    // owns is the cut: the prefix is the text before the cursor, in
    // either encoding, never rounded out to the token's end.
    let cases: [Encoded<&str>; 4] = [
        ("[\"abc\"]", (0, 3), (0, 3), "[\"a"),
        ("{\"abc\":1}", (0, 3), (0, 3), "{\"a"),
        ("[\"abc\"]", (0, 2), (0, 2), "[\""),
        ("[\"\u{1F600}\u{1F600}\"]", (0, 4), (0, 6), "[\"\u{1F600}"),
    ];
    for (input, utf16, utf8, prefix) in cases {
        for (encoding, (line, character)) in [
            (PositionEncoding::Utf16, utf16),
            (PositionEncoding::Utf8, utf8),
        ] {
            let doc = doc(input).with_encoding(encoding);
            let at = doc.offset_at(Position::new(line, character));
            assert_eq!(&doc.text[..at], prefix, "{input:?} at {line}:{character}");
        }
    }
}

/// A case registered as an engine divergence: its name, the document,
/// the cursor (UTF-16), the TypeScript items, the Rust items.
type Divergent = (
    &'static str,
    &'static str,
    Cursor,
    Vec<CompletionItem>,
    Vec<CompletionItem>,
);

#[test]
fn inside_a_string_or_comment_is_a_registered_engine_divergence() {
    // An ENGINE divergence, parser's to repair, not this crate's: after a
    // prefix ending in an unterminated string (or block comment: the
    // random sweep, tests/parity_sweep.rs, found every one of its
    // completion differences to be one of the two) the lexer fails, and
    // the three engines' `continuations` answer three ways. TypeScript
    // (canonical) offers the start rule's openers, whatever the context;
    // Rust offers the failing rule's tokens, a key or a close brace in an
    // object and a value or a close square in a list; Go another reading
    // again (`{"a`: `#NR #ST #VL`; `["a`: those, `{`, `[`, `]` and `,`).
    // The TypeScript answers are pinned beside the Rust ones, so a repair
    // in either engine fails here, and the case then moves to
    // `inside_a_string_the_prefix_ends_in_the_string` with the canonical
    // items. Completion itself maps whatever the engine answers.
    let in_an_object = vec![kw("#NR"), kw("#ST"), kw("#VL"), op("}", "#CB")];
    let in_a_list_string_comment = || {
        vec![
            kw("#NR"),
            kw("#ST"),
            kw("#VL"),
            op("{", "#OB"),
            op("[", "#OS"),
            op("]", "#CS"),
        ]
    };
    let in_a_list_string = in_a_list_string_comment();
    let cases: Vec<Divergent> = vec![
        (
            "inside a list's string",
            "[\"abc\"]",
            (0, 3),
            openers(),
            in_a_list_string.clone(),
        ),
        (
            "just after the quote",
            "[\"abc\"]",
            (0, 2),
            openers(),
            in_a_list_string.clone(),
        ),
        (
            "between two astral characters",
            "[\"\u{1F600}\u{1F600}\"]",
            (0, 4),
            openers(),
            in_a_list_string,
        ),
        (
            "inside a key",
            "{\"abc\":1}",
            (0, 3),
            openers(),
            in_an_object.clone(),
        ),
        (
            "inside a nested key",
            "{\"k\":{\"n\":1}}",
            (0, 7),
            openers(),
            in_an_object.clone(),
        ),
        (
            "inside a key on a later line",
            "{\n  \"a\": 1\n}",
            (1, 3),
            openers(),
            in_an_object.clone(),
        ),
        (
            "inside an object's block comment",
            "{/* x */\"a\":1}",
            (0, 3),
            openers(),
            in_an_object,
        ),
        (
            "inside a list's block comment",
            "[/* x */1]",
            (0, 3),
            openers(),
            in_a_list_string_comment(),
        ),
    ];
    let (instances, inst, entry) = stack();
    for (name, input, (line, character), typescript, rust) in cases {
        let got = completion(
            Some(&instances),
            &inst,
            &entry,
            &doc(input),
            Position::new(line, character),
        );
        assert_ne!(
            got, typescript,
            "{name}: the engines agree now; move the case to the canonical tables"
        );
        assert_eq!(got, rust, "{name}: the Rust engine's answer changed");
    }
}

#[test]
fn in_the_middle_of_a_token_the_first_part_is_the_prefix() {
    run(&[
        // `[1` of `[123]`: a number, so a closer or a separator.
        (
            "inside a number",
            "[123]",
            (0, 2),
            vec![op("]", "#CS"), op(",", "#CA")],
        ),
        // `[tr` of `[true]`: `tr` lexes as text the grammar does not
        // take, and the engine offers what the list admits there.
        ("inside a keyword", "[true]", (0, 3), in_a_list()),
    ]);
}

#[test]
fn positions_count_the_negotiated_encoding() {
    // The same cursors in UTF-16 units (the default) and UTF-8 bytes:
    // the prefix, and so the items, are the same.
    let (instances, inst, entry) = stack();
    let cases: [Encoded<Vec<CompletionItem>>; 6] = [
        ("{\"é\":", (0, 5), (0, 6), after_a_colon()),
        ("{\"\u{1F600}\":", (0, 6), (0, 8), after_a_colon()),
        (
            "{\"\u{1F600}\":1,",
            (0, 8),
            (0, 10),
            vec![kw("#NR"), kw("#ST"), kw("#VL")],
        ),
        ("[1,\"é\",", (0, 7), (0, 8), in_a_list()),
        (
            "[\"\u{1F600}\",",
            (0, 5),
            (0, 7),
            vec![op("]", "#CS"), op(",", "#CA")],
        ),
        ("[1,\r\n2]", (1, 0), (1, 0), in_a_list()),
    ];
    for (input, utf16, utf8, want) in cases {
        for (encoding, (line, character)) in [
            (PositionEncoding::Utf16, utf16),
            (PositionEncoding::Utf8, utf8),
        ] {
            let doc = doc(input).with_encoding(encoding);
            let got = completion(
                Some(&instances),
                &inst,
                &entry,
                &doc,
                Position::new(line, character),
            );
            assert_eq!(got, want, "{input:?} at {line}:{character} in {encoding}");
        }
    }
}

#[test]
fn the_conformance_cases_in_canonical_order() {
    // The fixture compares sorted labels; the order is pinned here, where
    // the TypeScript answer is in hand: the engine's, ascending token
    // identity.
    run(&[
        ("after a key", "{\"a\"", (0, 4), vec![op(":", "#CL")]),
        ("after a colon", "{\"a\":", (0, 5), after_a_colon()),
        ("after a separator", "[1,", (0, 3), in_a_list()),
    ]);
}

#[test]
fn without_a_cache_the_items_are_the_same() {
    let inst = instance();
    let entry = entry();
    check(
        None,
        &inst,
        &entry,
        &[
            ("start", "", (0, 0), openers()),
            ("after a colon", "{\"a\":", (0, 5), after_a_colon()),
            (
                "inside a number",
                "[123]",
                (0, 2),
                vec![op("]", "#CS"), op(",", "#CA")],
            ),
            ("end", "{\"a\":1}", (0, 7), vec![]),
        ],
    );
}

#[test]
fn the_query_leaks_nothing_into_an_active_collector() {
    // `continuations` parses internally, on the instance whose mux is
    // installed. A completion arriving while an analysis collects must
    // not write into that analysis: under the lock the slot is empty for
    // the query and put back afterwards.
    let (instances, inst, entry) = stack();
    let before = instances.mux().begin();
    assert!(before.is_none());
    let items = completion(
        Some(&instances),
        &inst,
        &entry,
        &doc("{\"a\":[1,2"),
        Position::new(0, 9),
    );
    assert!(!items.is_empty());
    assert!(
        instances.mux().is_active(),
        "the collector is put back after the query"
    );
    let collected = instances.mux().end().expect("the collector survives");
    assert!(collected.lex.is_empty(), "lex events leaked: {collected:?}");
    assert!(collected.rules.is_empty(), "rule events leaked");

    // Without the lock the same query writes into whatever is active:
    // that is what the lock is for.
    instances.mux().begin();
    completion(
        None,
        &inst,
        &entry,
        &doc("{\"a\":[1,2"),
        Position::new(0, 9),
    );
    let leaked = instances.mux().end().expect("the collector survives");
    assert!(!leaked.lex.is_empty());
}

#[test]
fn a_parse_after_a_completion_collects_as_usual() {
    let (instances, inst, entry) = stack();
    completion(
        Some(&instances),
        &inst,
        &entry,
        &doc("[1,"),
        Position::new(0, 3),
    );
    let (recovery, collected) = instances.parse(&inst, "[1,2]");
    assert!(recovery.errors.is_empty());
    assert!(!collected.lex.is_empty());
    assert!(!collected.rules.is_empty());
}

#[test]
fn fixed_sources_resolve_by_token_name_and_by_source() {
    let inst = instance();
    assert_eq!(fixed_source(&inst, "#CL").as_deref(), Some(":"));
    assert_eq!(fixed_source(&inst, "#OB").as_deref(), Some("{"));
    assert_eq!(fixed_source(&inst, "#CS").as_deref(), Some("]"));
    // `inst.token(ref)` in TypeScript reads a fixed source first.
    assert_eq!(fixed_source(&inst, ":").as_deref(), Some(":"));
    // A token matched by a lexer, not a fixed literal, has none.
    assert_eq!(fixed_source(&inst, "#NR"), None);
    assert_eq!(fixed_source(&inst, "#ST"), None);
    // A name the grammar does not know has none, and is not registered.
    let tokens = inst.options.tokens.len();
    assert_eq!(fixed_source(&inst, "#NOPE"), None);
    assert_eq!(inst.options.tokens.len(), tokens);
}

#[test]
fn a_panicking_grammar_answers_and_releases_the_lock() {
    // A grammar callback that panics on every value. The TypeScript
    // engine lets the throw reach the core, whose `catch` answers with no
    // items; the Rust engine catches the panic inside `continuations` and
    // answers with the start rule's openers. Either way completion
    // returns, and the lock and the collector slot are released: the
    // next parse through the same cache, of a sound grammar, collects.
    let make: MakeInstance = Arc::new(|entry| {
        let mut inst = instance();
        if entry.language_id() == "broken" {
            inst.rules
                .get_mut("val")
                .expect("the grammar has a val rule")
                .add_bo(|_, _| panic!("a grammar callback panicked"));
        }
        Ok(inst)
    });
    let mut instances = Instances::new(make);
    let mut broken = entry();
    broken.language_id = Some("broken".into());
    let broken_inst = instances.get(&broken, None).unwrap().unwrap();
    let sound = entry();
    let sound_inst = instances.get(&sound, None).unwrap().unwrap();

    let items = completion(
        Some(&instances),
        &broken_inst,
        &broken,
        &doc("[1,"),
        Position::new(0, 3),
    );
    assert_eq!(items, openers());
    assert!(!instances.mux().is_active());
    let (recovery, collected) = instances.parse(&sound_inst, "[1,2]");
    assert!(recovery.errors.is_empty());
    assert!(!collected.lex.is_empty(), "the next parse collects");
}

#[test]
fn the_trigger_characters_are_the_canonical_five() {
    assert_eq!(TRIGGER_CHARACTERS, [":", ",", "{", "[", "\""]);
}
