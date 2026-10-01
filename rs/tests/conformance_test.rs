// The cross-runtime conformance suite (design §13): every section of
// test/fixtures/lsp-conformance.json, the same fixtures
// ts/test/conformance.test.js and go/conformance_test.go run, against the
// shared pure-data grammar. TS is canonical: a mismatch here is a defect
// in this port, never a fixture update.
//
// One runner per section, mirroring the TypeScript runner case for
// case: `run_analyze` (diagnostic codes in order, the first diagnostic's
// range, the outline's name tree), `run_completions` (sorted item
// labels at a position), `run_outlines` (the whole symbol tree, ranges
// included), `run_semantic` (error count, decoded tokens,
// delta-encoded data) and `run_traces` (decoded tokens and data of a
// recorded lex trace, fed to the pipeline with no parse). A new fixture
// section gets a runner of its own here, and in the other two runtimes'
// runners in the same change.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use tabnas::{Options, Tabnas};
use tabnas_lsp::analyze::semantic_tokens_of;
use tabnas_lsp::{
    analyze, completion, encode, highlight, semantic_tokens, Doc, DocumentSymbol, Entry,
    Highlighter, Instances, LexTrace, MakeInstance, Overrides, Position, Range, TokenPoint,
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
    outlines: Vec<OutlinesCase>,
    semantic: Vec<SemanticCase>,
    traces: Vec<TraceCase>,
}

#[derive(Debug, Deserialize)]
struct OutlinesCase {
    name: String,
    input: String,
    symbols: Vec<DocumentSymbol>,
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

/// A recorded lex trace: `events` are `[name, si, ri, ci, len, src]`
/// rows, recorded from the grammar `recorded` names.
#[derive(Debug, Deserialize)]
struct TraceCase {
    name: String,
    input: String,
    #[serde(default)]
    overrides: Option<Overrides>,
    events: Vec<(String, usize, usize, usize, usize, String)>,
    tokens: Vec<(usize, usize, usize, String)>,
    data: Vec<u32>,
}

impl TraceCase {
    fn events(&self) -> Vec<TokenPoint> {
        self.events
            .iter()
            .map(|(name, si, ri, ci, len, src)| TokenPoint {
                name: name.clone(),
                si: *si,
                ri: *ri,
                ci: *ci,
                len: *len,
                src: src.clone(),
            })
            .collect()
    }
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

fn outline_names(symbols: &[DocumentSymbol]) -> Vec<OutlineNode> {
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
        // Every fixture document recovers to the end: recovery reports
        // the errors and the analysis is not a failure.
        assert!(!analysis.failed, "{}: the analysis failed", case.name);
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
// outlines

fn run_outlines(suite: &Suite) {
    assert!(!suite.outlines.is_empty(), "the outlines section has cases");
    let (instances, inst, entry) = stack();
    for case in &suite.outlines {
        let analysis = analyze(&instances, &inst, &entry, &doc(&case.input));
        assert_eq!(analysis.outline, case.symbols, "{}: symbols", case.name);
    }
}

#[test]
fn the_outlines_section_matches_the_canonical_pipeline() {
    run_outlines(&suite());
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

/// The same section through `analyze`, the way the Go runner executes
/// it: the entry carries the case's overrides, the mux collects the lex
/// trace, and the analysis's `data` is the fixture's.
#[test]
fn the_semantic_section_matches_through_analyze() {
    let suite = suite();
    let (instances, inst, entry) = stack();
    for case in &suite.semantic {
        let mut entry = entry.clone();
        entry.semantic_tokens = case.overrides.clone();
        let analysis = analyze(&instances, &inst, &entry, &doc(&case.input));
        assert_eq!(
            analysis.errors.len(),
            case.errors,
            "{}: error count",
            case.name
        );
        let tokens = analysis
            .semantic_tokens
            .as_ref()
            .unwrap_or_else(|| panic!("{}: no semantic tokens for a clean entry", case.name));
        assert_eq!(tokens.data, case.data, "{}: data", case.name);
        let rows: Vec<(usize, usize, usize, String)> = tokens
            .tokens
            .iter()
            .map(|t| (t.row, t.col, t.len, t.kind.name().to_string()))
            .collect();
        assert_eq!(rows, case.tokens, "{}: tokens", case.name);
    }
}

// ---------------------------------------------------------------------
// traces

fn run_traces(suite: &Suite) {
    assert!(
        suite.traces.len() >= 2,
        "the traces section has {} cases",
        suite.traces.len()
    );
    for case in &suite.traces {
        let tokens = semantic_tokens(&case.events(), case.overrides.as_ref(), &case.input);
        let rows: Vec<(usize, usize, usize, String)> = tokens
            .iter()
            .map(|t| (t.row, t.col, t.len, t.kind.name().to_string()))
            .collect();
        assert_eq!(rows, case.tokens, "{}: tokens", case.name);
        assert_eq!(encode(&tokens), case.data, "{}: data", case.name);
    }
}

#[test]
fn the_traces_section_matches_the_canonical_pipeline() {
    run_traces(&suite());
}

/// The same section through `analyze`'s own step from events to tokens,
/// with the case's overrides on the entry.
#[test]
fn the_traces_section_matches_through_semantic_tokens_of() {
    let suite = suite();
    for case in &suite.traces {
        let mut entry = entry();
        entry.semantic_tokens = case.overrides.clone();
        let tokens = semantic_tokens_of(&case.events(), &entry, &doc(&case.input));
        assert_eq!(tokens.data, case.data, "{}: data", case.name);
        let rows: Vec<(usize, usize, usize, String)> = tokens
            .tokens
            .iter()
            .map(|t| (t.row, t.col, t.len, t.kind.name().to_string()))
            .collect();
        assert_eq!(rows, case.tokens, "{}: tokens", case.name);
    }
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
