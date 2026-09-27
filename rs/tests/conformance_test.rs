// The cross-runtime conformance suite (design §13): every section of
// test/fixtures/lsp-conformance.json, the same fixtures
// ts/test/conformance.test.js and go/conformance_test.go run, against the
// shared pure-data grammar. TS is canonical: a mismatch here is a defect
// in this port, never a fixture update.
//
// One runner per section, mirroring the TypeScript runner case for
// case: `run_analyze` (diagnostic codes in order, the first diagnostic's
// range, the outline's name tree), `run_completions` (sorted item
// labels at a position) and `run_semantic` (error count, decoded tokens,
// delta-encoded data). The analyze and completions runners are written
// against the module signatures and their tests are IGNORED until the
// analyze, completion, instances and documents modules are implemented:
// each module agent removes the `ignore` on the runner its work
// completes. A new fixture section (hover, outline positions) gets a
// runner of its own here.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use tabnas::{Options, Tabnas};
use tabnas_lsp::{
    analyze, completion, encode, highlight, semantic_tokens, Doc, Entry, Highlighter, Instances,
    LexTrace, MakeInstance, Overrides, Position, Range,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rs/ has a parent")
        .join("test")
        .join("fixtures")
}

fn fixture(name: &str) -> String {
    let path = fixtures().join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[derive(Debug, Deserialize)]
struct Suite {
    analyze: Vec<AnalyzeCase>,
    completions: Vec<CompletionCase>,
    semantic: Vec<SemanticCase>,
}

#[derive(Debug, Deserialize)]
struct AnalyzeCase {
    name: String,
    input: String,
    codes: Vec<String>,
    #[serde(default, rename = "firstRange")]
    first_range: Option<Range>,
    outline: Vec<OutlineNode>,
}

/// The outline's name tree, the shape the fixture pins.
#[derive(Debug, Deserialize, PartialEq, Eq)]
struct OutlineNode {
    name: String,
    children: Vec<OutlineNode>,
}

#[derive(Debug, Deserialize)]
struct CompletionCase {
    name: String,
    input: String,
    position: Position,
    labels: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SemanticCase {
    name: String,
    input: String,
    #[serde(default)]
    overrides: Option<Overrides>,
    errors: usize,
    tokens: Vec<(usize, usize, usize, String)>,
    data: Vec<u32>,
}

fn suite() -> Suite {
    serde_json::from_str(&fixture("lsp-conformance.json")).expect("lsp-conformance.json parses")
}

/// The engine instance the pipeline uses: recovery on (design §4), the
/// shared JSON grammar installed. One per parse where a recorder is
/// attached directly, as the recorder stays installed on whatever it is
/// attached to.
fn instance() -> Tabnas {
    let mut options = Options::default();
    options.parse.recover.enabled = true;
    let mut parser = Tabnas::with_options(options);
    parser
        .grammar_json(&fixture("json-grammar.json"))
        .expect("json-grammar.json installs");
    parser
}

/// The shared entry, `jsonf`, as the TypeScript runner normalizes it.
fn entry() -> Entry {
    let mut entry = Entry::new("jsonf");
    entry.language_id = Some("jsonf".into());
    entry.extensions = vec![".jsonf".into()];
    entry.grammar_kind = Some("data".into());
    entry
}

/// The stack the analyze and completion runners share: an instance
/// cache over the fixture grammar, with the mux installed, and the
/// cached instance for the entry (`makeStack` in go/core_test.go).
fn stack() -> (Instances, Arc<Tabnas>, Entry) {
    let make: MakeInstance = Arc::new(|_entry| Ok(instance()));
    let mut instances = Instances::new(make);
    let entry = entry();
    let inst = instances
        .get(&entry, None)
        .expect("the fixture grammar loads")
        .expect("a fresh entry is not quarantined");
    (instances, inst, entry)
}

fn doc(text: &str) -> Doc {
    Doc::new("file:///t.jsonf", "jsonf", 1, text)
}

fn outline_names(symbols: &[tabnas_lsp::DocumentSymbol]) -> Vec<OutlineNode> {
    symbols
        .iter()
        .map(|symbol| OutlineNode {
            name: symbol.name.clone(),
            children: outline_names(&symbol.children),
        })
        .collect()
}

// ---------------------------------------------------------------------
// analyze

fn run_analyze(suite: &Suite) {
    assert!(!suite.analyze.is_empty(), "the analyze section has cases");
    let (instances, inst, entry) = stack();
    for case in &suite.analyze {
        let analysis = analyze(&instances, &inst, &entry, &doc(&case.input));
        let codes: Vec<&str> = analysis
            .diagnostics
            .iter()
            .map(|d| d.code.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(codes, case.codes, "{}: codes", case.name);
        if let Some(first_range) = &case.first_range {
            let first = analysis
                .diagnostics
                .first()
                .unwrap_or_else(|| panic!("{}: no diagnostics to check firstRange", case.name));
            assert_eq!(&first.range, first_range, "{}: firstRange", case.name);
        }
        assert_eq!(
            outline_names(&analysis.outline),
            case.outline,
            "{}: outline",
            case.name
        );
    }
}

#[test]
#[ignore = "analyze runner: enabled by the analyze module agent once analyze, instances and documents are implemented"]
fn the_analyze_section_matches_the_canonical_pipeline() {
    run_analyze(&suite());
}

// ---------------------------------------------------------------------
// completions

fn run_completions(suite: &Suite) {
    assert!(
        !suite.completions.is_empty(),
        "the completions section has cases"
    );
    let (instances, inst, entry) = stack();
    for case in &suite.completions {
        let items = completion(
            Some(&instances),
            &inst,
            &entry,
            &doc(&case.input),
            case.position,
        );
        let mut labels: Vec<String> = items.into_iter().map(|item| item.label).collect();
        labels.sort();
        assert_eq!(labels, case.labels, "{}: labels", case.name);
    }
}

#[test]
fn the_completions_section_matches_the_canonical_pipeline() {
    run_completions(&suite());
}

// ---------------------------------------------------------------------
// semantic

fn run_semantic(suite: &Suite) {
    assert!(
        suite.semantic.len() >= 5,
        "the semantic section has {} cases",
        suite.semantic.len()
    );
    for case in &suite.semantic {
        // The pipeline, step by step: record, parse, reconcile and map.
        let mut parser = instance();
        let trace = LexTrace::install(&mut parser);
        let recovery = parser.parse_recover(&case.input);
        assert!(
            recovery.fatal.is_none(),
            "{}: recovery gave up: {:?}",
            case.name,
            recovery.fatal
        );
        assert_eq!(
            recovery.errors.len(),
            case.errors,
            "{}: error count",
            case.name
        );

        let tokens = semantic_tokens(&trace.events(), case.overrides.as_ref(), &case.input);
        let rows: Vec<(usize, usize, usize, String)> = tokens
            .iter()
            .map(|t| (t.row, t.col, t.len, t.kind.name().to_string()))
            .collect();
        assert_eq!(rows, case.tokens, "{}: tokens", case.name);
        assert_eq!(encode(&tokens), case.data, "{}: data", case.name);
    }
}

#[test]
fn the_semantic_section_matches_the_canonical_pipeline() {
    run_semantic(&suite());
}

// ---------------------------------------------------------------------
// The crate's own use of the semantic section: the host-facing path
// agrees with the pipeline, and one recorder serves every parse.

#[test]
fn the_convenience_path_agrees_with_the_pipeline() {
    let suite = suite();
    for case in &suite.semantic {
        let result = highlight(instance(), &case.input, case.overrides.as_ref());
        let rows: Vec<(usize, usize, usize, String)> = result
            .tokens
            .iter()
            .map(|t| (t.row, t.col, t.len, t.kind.name().to_string()))
            .collect();
        assert_eq!(rows, case.tokens, "{}: highlight tokens", case.name);
        assert_eq!(
            result.errors.len(),
            case.errors,
            "{}: highlight errors",
            case.name
        );
        assert!(!result.partial, "{}: the whole text was lexed", case.name);
        assert_eq!(
            result.spans.len(),
            result.tokens.len(),
            "{}: one span per token",
            case.name
        );
        // Each span covers the text its token describes: the same kind,
        // the same length in scalar values, on the same line, in order.
        let mut previous_end = 0;
        for (span, token) in result.spans.iter().zip(&result.tokens) {
            assert!(span.start >= previous_end, "{}: spans in order", case.name);
            assert!(span.end <= case.input.len(), "{}: span in range", case.name);
            let text = &case.input[span.start..span.end];
            assert_eq!(span.kind, token.kind, "{}: span kind", case.name);
            assert_eq!(
                text.chars().count(),
                token.len_chars,
                "{}: span length of {text:?}",
                case.name
            );
            assert!(
                !text.contains('\n'),
                "{}: a span never crosses a line",
                case.name
            );
            let line = case.input[..span.start].matches('\n').count();
            assert_eq!(line, token.row, "{}: span line of {text:?}", case.name);
            previous_end = span.end;
        }
    }
}

#[test]
fn a_highlighter_reuses_one_parser_across_documents() {
    let suite = suite();
    let mut highlighter = Highlighter::new(instance());
    // Twice over, so a recorder left holding the previous document's
    // events would show up as extra tokens.
    for _ in 0..2 {
        for case in &suite.semantic {
            highlighter.set_overrides(case.overrides.clone());
            let result = highlighter.highlight(&case.input);
            let rows: Vec<(usize, usize, usize, String)> = result
                .tokens
                .iter()
                .map(|t| (t.row, t.col, t.len, t.kind.name().to_string()))
                .collect();
            assert_eq!(rows, case.tokens, "{}: reused highlighter", case.name);
            assert_eq!(result.errors.len(), case.errors, "{}", case.name);
        }
    }
}

#[test]
fn a_highlighter_installs_one_recorder_for_all_its_parses() {
    // The engine has no unsubscribe, so a recorder per parse on one
    // instance would stack up, every parse filling every recorder ever
    // installed (and rebuilding the parser each time). The Highlighter
    // installs once: the subscriber count stays at one, and no parse
    // sees another's events. `highlight` consumes its parser, so there
    // the rule holds by construction.
    let mut highlighter = Highlighter::new(instance());
    let baseline = highlighter.highlight("[1,2]").reconciled.len();
    for _ in 0..50 {
        assert_eq!(highlighter.highlight("[1,2]").reconciled.len(), baseline);
    }
    assert_eq!(highlighter.parser().lex_subscribers.len(), 1);
    assert_eq!(
        highlight(instance(), "[1,2]", None).reconciled.len(),
        baseline
    );
}

#[test]
fn a_fail_fast_parse_is_marked_partial() {
    // Recovery off: the parse stops at the first error, and what was
    // lexed up to it is still highlighted.
    let mut parser = Tabnas::new();
    parser
        .grammar_json(&fixture("json-grammar.json"))
        .expect("json-grammar.json installs");
    let result = highlight(parser, "{\"a\":1 @ 2,\"b\":3}", None);
    assert!(result.partial);
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].code, "unexpected");
    // The token the parse failed on (`@`, lexed as text) was announced
    // before the failure, so it is the last one coloured; nothing after
    // it was lexed at all.
    let kinds: Vec<&str> = result.tokens.iter().map(|t| t.kind.name()).collect();
    assert_eq!(
        kinds,
        ["operator", "string", "operator", "number", "string"]
    );
    assert_eq!(
        result
            .spans
            .last()
            .map(|s| &"{\"a\":1 @ 2,\"b\":3}"[s.start..s.end]),
        Some("@")
    );
    assert!(
        result.reconciled.iter().all(|t| t.si <= 7),
        "nothing past the error"
    );
}
