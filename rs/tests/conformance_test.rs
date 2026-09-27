// The cross-runtime conformance suite (design §13): the `semantic`
// section of test/fixtures/lsp-conformance.json, the same fixtures
// ts/test/conformance.test.js and go/conformance_test.go run, against the
// shared pure-data grammar. TS is canonical: a mismatch here is a defect
// in this port, never a fixture update.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tabnas::{Options, Tabnas};
use tabnas_lsp::{encode, highlight, semantic_tokens, Highlighter, LexTrace, Overrides};

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
    semantic: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    input: String,
    #[serde(default)]
    overrides: Option<Overrides>,
    errors: usize,
    tokens: Vec<(usize, usize, usize, String)>,
    data: Vec<u32>,
}

/// The engine instance the pipeline uses: recovery on (design §4), the
/// shared JSON grammar installed. One per parse, as the recorder stays
/// installed on whatever it is attached to.
fn instance() -> Tabnas {
    let mut options = Options::default();
    options.parse.recover.enabled = true;
    let mut parser = Tabnas::with_options(options);
    parser
        .grammar_json(&fixture("json-grammar.json"))
        .expect("json-grammar.json installs");
    parser
}

fn suite() -> Suite {
    serde_json::from_str(&fixture("lsp-conformance.json")).expect("lsp-conformance.json parses")
}

#[test]
fn the_semantic_section_matches_the_canonical_pipeline() {
    let suite = suite();
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
fn the_convenience_path_agrees_with_the_pipeline() {
    let suite = suite();
    for case in &suite.semantic {
        let mut parser = instance();
        let result = highlight(&mut parser, &case.input, case.overrides.as_ref());
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
fn a_fail_fast_parse_is_marked_partial() {
    // Recovery off: the parse stops at the first error, and what was
    // lexed up to it is still highlighted.
    let mut parser = Tabnas::new();
    parser
        .grammar_json(&fixture("json-grammar.json"))
        .expect("json-grammar.json installs");
    let result = highlight(&mut parser, "{\"a\":1 @ 2,\"b\":3}", None);
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
