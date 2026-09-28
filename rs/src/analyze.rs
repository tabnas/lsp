// Copyright (c) 2026 Richard Rodger, MIT License

//! The parse pipeline (design §8): ONE parse per change, with the mux
//! collecting, yields diagnostics, semantic tokens and outline together.
//! Protocol-free: [`crate::server`] is a thin front-end over this module,
//! and a Rust host calls it directly.
//!
//! Mirrors `analyze` and `diagnostics` in `ts/src/core.js` (canonical)
//! and `Analyze` / `Diagnostics` in `go/core.go`. The engine call is
//! `parse_recover` (multi-error diagnostics: every recovered error, the
//! one the parse ended on included) through [`Instances::parse`], which
//! runs the parse with the collector active.
//!
//! Diagnostics are built from the engine error's SERIALIZED form
//! (`code`, `message`, `hint`, `row`, `col`, `pos`, `len`: the same
//! JSON both canonical runtimes consume), so the three ports cannot
//! disagree about codes or the `len` unit; the range comes from
//! [`Doc::range_from`]. Semantic tokens are served only for `lexStream:
//! clean` entries, in the document's negotiated encoding; the outline is
//! derived from the rule events whatever the entry.
//!
//! The `analyze` section of `test/fixtures/lsp-conformance.json` (codes
//! in order, the first diagnostic's range, the outline's name tree) is
//! the contract; `rs/tests/conformance_test.rs` `run_analyze` executes
//! it, and the tests here pin the messages, ranges, outlines and token
//! data the TypeScript pipeline produces for multi-error, multi-line and
//! multi-byte documents (each value measured by running `ts/src/core.js`
//! on the same text and grammar).
//!
//! Where TypeScript's `analyze` THROWS, this one cannot: the grammar's
//! own code throwing something other than a `TabnasError` escapes the
//! canonical function, and its server counts that toward the grammar's
//! quarantine instead of publishing. The Rust engine catches the panic
//! and reports it as an `internal` error that ends the parse, so it
//! arrives here as a failed analysis whose last error has that code, as
//! in the Go port; a server that wants the canonical quarantine count
//! reads it there.

use serde::Deserialize;
use tabnas::{ParseRecovery, Tabnas, TabnasError, Value};

use crate::instances::Instances;
use crate::outline::{outline, DocumentSymbol};
use crate::semantic::{encode, segments, Segment, SemanticToken};
use crate::trace::{reconcile, TokenPoint};
use crate::types::{
    CodeDescription, Diagnostic, Doc, Entry, PositionEncoding, ERROR_REGISTRY, SEVERITY_ERROR,
};

/// The semantic tokens of one analysis: the delta-encoded `data` the
/// server sends (over the fixed [`crate::LEGEND`]), counted in the
/// document's encoding, and the decoded tokens beside it for hosts, in
/// UTF-16 units and scalar values as [`SemanticToken`] defines them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SemanticTokens {
    pub data: Vec<u32>,
    pub tokens: Vec<SemanticToken>,
}

/// Everything one parse yields (`analyze`'s return in TypeScript,
/// `Analysis` in Go).
#[derive(Debug, Clone)]
pub struct Analysis {
    /// The parsed value, when the parse produced one: the whole value,
    /// or what recovery kept of a broken document.
    pub value: Option<Value>,
    /// The engine's errors, in order: the recovered ones, then the one
    /// the parse ended on if it ended on one.
    pub errors: Vec<TabnasError>,
    /// The parse raised its terminal error instead of returning a
    /// recovery result: recovery is off on this instance (fail-fast), the
    /// engine caught a panic and reported it as `internal`, or a
    /// `parser.start` hook failed. TypeScript's `failed`: diagnostics are
    /// served, structural results are not cached. With recovery on, a
    /// parse that gives up still returns its errors and is not failed.
    pub failed: bool,
    pub diagnostics: Vec<Diagnostic>,
    /// `None` for an entry whose lex stream is not clean.
    pub semantic_tokens: Option<SemanticTokens>,
    pub outline: Vec<DocumentSymbol>,
    /// The reconciled lex trace: every token the parse used, mapped to a
    /// legend entry or not, for hover and for hosts.
    pub reconciled: Vec<TokenPoint>,
}

/// Run one parse of `doc` with `inst` (built for `entry`, with recovery
/// enabled) and derive every artifact.
pub fn analyze(instances: &Instances, inst: &Tabnas, entry: &Entry, doc: &Doc) -> Analysis {
    let (recovery, collected) = instances.parse(inst, &doc.text);
    let failed = recovery.fatal.is_some();
    let (value, errors) = errors_of(recovery);
    let diagnostics = diagnostics(&errors, Some(entry), doc);
    let reconciled = reconcile(&collected.lex);
    let semantic_tokens = entry
        .is_clean()
        .then(|| semantic_tokens_of_reconciled(&reconciled, entry, doc));
    let outline = outline(&collected.rules, Some(entry), doc);
    Analysis {
        value,
        errors,
        failed,
        diagnostics,
        semantic_tokens,
        outline,
        reconciled,
    }
}

/// The value and the errors of a recovery result, in diagnostic order.
///
/// With recovery on, the engine lists the error a parse ended on among
/// the recovered ones already, and sets `fatal` only for an error raised
/// outside the parse loop (a caught panic, reported as `internal`, or a
/// failing `parser.start` hook), with no errors beside it; the canonical
/// `analyze` makes such an error its one diagnostic (`errors = [e]`).
/// With recovery off, `fatal` is the fail-fast error and the list
/// repeats it. So the terminal error joins the list only when it is not
/// there already, and the same document never yields the same
/// diagnostic twice.
fn errors_of(recovery: ParseRecovery) -> (Option<Value>, Vec<TabnasError>) {
    let ParseRecovery {
        value,
        mut errors,
        fatal,
    } = recovery;
    if let Some(fatal) = fatal {
        if errors.last() != Some(&fatal) {
            errors.push(fatal);
        }
    }
    (value, errors)
}

/// The slice of the engine's structured diagnostic this pipeline
/// consumes (`TabnasError`'s `Serialize`: the same shape TypeScript's
/// `JSON.stringify(err)` and Go's `MarshalJSON` emit, which is what keeps
/// the three ports honest about codes and the `len` unit).
#[derive(Debug, Deserialize)]
struct EngineDiagnostic {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    hint: String,
    #[serde(default)]
    row: usize,
    #[serde(default)]
    col: usize,
    #[serde(default)]
    pos: usize,
    #[serde(default)]
    len: usize,
}

/// Engine errors to LSP diagnostics: severity 1, the engine's `code`,
/// `source` `tabnas:<languageId>` (`tabnas` with no entry), the message
/// with the hint after a blank line, a `codeDescription` into the error
/// registry for every code but `unknown`, the range through the
/// document text. An error that does not serialize is skipped, as the
/// canonical pipeline skips it.
pub fn diagnostics(errors: &[TabnasError], entry: Option<&Entry>, doc: &Doc) -> Vec<Diagnostic> {
    let source = match entry {
        Some(entry) => format!("tabnas:{}", entry.language_id()),
        None => "tabnas".to_string(),
    };
    errors
        .iter()
        .filter_map(|error| {
            let json = serde_json::to_value(error).ok()?;
            let d: EngineDiagnostic = serde_json::from_value(json).ok()?;
            let mut message = d.message;
            if !d.hint.is_empty() {
                message.push_str("\n\n");
                message.push_str(&d.hint);
            }
            let code_description =
                (!d.code.is_empty() && d.code != "unknown").then(|| CodeDescription {
                    href: format!("{ERROR_REGISTRY}{}", d.code),
                });
            Some(Diagnostic {
                range: doc.range_from(d.row, d.col, d.pos, d.len),
                severity: SEVERITY_ERROR,
                code: (!d.code.is_empty()).then_some(d.code),
                source: source.clone(),
                message,
                code_description,
            })
        })
        .collect()
}

/// The semantic tokens of one parse's lex events for an entry: the
/// trace reconciled, mapped through the entry's overrides and encoded
/// ([`crate::semantic::semantic_tokens`] and [`crate::semantic::encode`]).
pub fn semantic_tokens_of(lex: &[TokenPoint], entry: &Entry, doc: &Doc) -> SemanticTokens {
    semantic_tokens_of_reconciled(&reconcile(lex), entry, doc)
}

/// [`semantic_tokens_of`] for a trace that is already reconciled (an
/// [`Analysis::reconciled`], a `Highlight`'s).
///
/// The tokens are the UTF-16 form the semantic module defines. `data`
/// is counted in the document's encoding: UTF-16 units as the canonical
/// server sends them, or, for a client that negotiated UTF-8, the byte
/// column and length of each line-local segment, so a non-ASCII line
/// highlights where the client puts its characters.
pub fn semantic_tokens_of_reconciled(
    reconciled: &[TokenPoint],
    entry: &Entry,
    doc: &Doc,
) -> SemanticTokens {
    let segments = segments(reconciled, entry.overrides(), &doc.text);
    let tokens: Vec<SemanticToken> = segments.iter().map(Segment::token).collect();
    let data = match doc.encoding {
        PositionEncoding::Utf16 => encode(&tokens),
        PositionEncoding::Utf8 => encode(&byte_tokens(&segments, &doc.text)),
    };
    SemanticTokens { data, tokens }
}

/// The segments with their column and length in bytes, for the UTF-8
/// wire encoding. A segment's column measures the `col_chars` scalar
/// values right before it (the engine's column since its last reset, as
/// the UTF-16 one does); its length is its byte span. These are
/// `SemanticToken`s in shape only, built to be delta-encoded.
fn byte_tokens(segments: &[Segment], text: &str) -> Vec<SemanticToken> {
    segments
        .iter()
        .map(|segment| {
            let mut start = segment.start.min(text.len());
            while !text.is_char_boundary(start) {
                start -= 1;
            }
            let col = text[..start]
                .chars()
                .rev()
                .take(segment.col_chars)
                .map(char::len_utf8)
                .sum();
            SemanticToken {
                row: segment.row,
                col,
                len: segment.end.saturating_sub(start).max(1),
                col_chars: segment.col_chars,
                len_chars: segment.len_chars,
                kind: segment.kind,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tabnas::Options;

    use super::*;
    use crate::types::{MakeInstance, Position, Range};

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test/fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The shared pure-data JSON grammar with recovery on, the stack
    /// both canonical test suites build (`makeStack`).
    fn instance() -> Tabnas {
        let mut options = Options::default();
        options.parse.recover.enabled = true;
        let mut parser = Tabnas::with_options(options);
        parser
            .grammar_json(&fixture("json-grammar.json"))
            .expect("json-grammar.json installs");
        parser
    }

    fn entry() -> Entry {
        let mut entry = Entry::new("jsonf");
        entry.language_id = Some("jsonf".into());
        entry.grammar_kind = Some("data".into());
        entry
    }

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

    fn range(l0: u32, c0: u32, l1: u32, c1: u32) -> Range {
        Range::new(Position::new(l0, c0), Position::new(l1, c1))
    }

    fn json(analysis: &Analysis) -> Option<serde_json::Value> {
        analysis.value.as_ref().map(Value::to_json)
    }

    fn codes(analysis: &Analysis) -> Vec<&str> {
        analysis
            .diagnostics
            .iter()
            .map(|d| d.code.as_deref().unwrap_or(""))
            .collect()
    }

    fn names(symbols: &[DocumentSymbol]) -> Vec<(&str, Vec<&str>)> {
        symbols
            .iter()
            .map(|s| {
                (
                    s.name.as_str(),
                    s.children.iter().map(|c| c.name.as_str()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn a_clean_parse_yields_no_diagnostics_and_every_structure() {
        let (instances, inst, entry) = stack();
        let a = analyze(&instances, &inst, &entry, &doc(r#"{"a":[1,2]}"#));
        assert!(!a.failed);
        assert!(a.errors.is_empty());
        assert!(a.diagnostics.is_empty());
        // The engine's numbers are f64, as JavaScript's are.
        assert_eq!(json(&a), Some(serde_json::json!({"a": [1.0, 2.0]})));
        let tokens = a
            .semantic_tokens
            .as_ref()
            .expect("a clean entry has tokens");
        assert!(!tokens.data.is_empty());
        assert_eq!(tokens.data.len(), tokens.tokens.len() * 5);
        assert_eq!(names(&a.outline), vec![("Object", vec!["Array"])]);
        assert_eq!(a.outline[0].range, range(0, 0, 0, 11));
        assert_eq!(a.outline[0].children[0].range, range(0, 5, 0, 10));
        // The reconciled trace holds every token the parse used, the
        // end-of-source sentinel included.
        let used: Vec<&str> = a.reconciled.iter().map(|t| t.src.as_str()).collect();
        assert_eq!(used, ["{", "\"a\"", ":", "[", "1", ",", "2", "]", "}", ""]);
    }

    #[test]
    fn a_broken_document_yields_the_canonical_diagnostic() {
        let (instances, inst, entry) = stack();
        let a = analyze(&instances, &inst, &entry, &doc(r#"{"a":true blah,"b":2}"#));
        assert!(!a.failed, "recovery carried the parse to the end");
        assert_eq!(codes(&a), ["unexpected"]);
        let d = &a.diagnostics[0];
        assert_eq!(d.range, range(0, 10, 0, 14));
        assert_eq!(d.severity, SEVERITY_ERROR);
        assert_eq!(d.source, "tabnas:jsonf");
        assert_eq!(
            d.message,
            "unexpected character(s): blah\n\nThe character(s) blah do not match any rule alternative active at\nthis position."
        );
        assert_eq!(
            d.code_description.as_ref().map(|c| c.href.as_str()),
            Some("https://tabnas.dev/errors/unexpected")
        );
        // Recovery kept what it could of the value.
        assert_eq!(json(&a), Some(serde_json::json!({"a": true, "b": 2.0})));
        assert_eq!(names(&a.outline), vec![("Object", vec![])]);
        assert_eq!(a.outline[0].range, range(0, 0, 0, 21));
    }

    #[test]
    fn a_multi_error_document_yields_one_diagnostic_per_error_in_order() {
        let (instances, inst, entry) = stack();
        let a = analyze(
            &instances,
            &inst,
            &entry,
            &doc(r#"{"a":true blah,"b":false blah,"c":true blah}"#),
        );
        assert!(!a.failed);
        assert_eq!(codes(&a), ["unexpected", "unexpected", "unexpected"]);
        let ranges: Vec<Range> = a.diagnostics.iter().map(|d| d.range).collect();
        assert_eq!(
            ranges,
            [
                range(0, 10, 0, 14),
                range(0, 25, 0, 29),
                range(0, 39, 0, 43)
            ]
        );
        assert_eq!(a.errors.len(), 3);
        assert_eq!(
            json(&a),
            Some(serde_json::json!({"a": true, "b": false, "c": true}))
        );
        assert_eq!(a.outline[0].range, range(0, 0, 0, 44));

        // Errors of different kinds, on different lines: a lex error
        // (unterminated string) and two syntax errors.
        let a = analyze(
            &instances,
            &inst,
            &entry,
            &doc("{\"a\":1,,\n\"b\":[1 2],\n\"c\":\"open}"),
        );
        assert_eq!(
            codes(&a),
            ["unexpected", "unexpected", "unterminated_string"]
        );
        let ranges: Vec<Range> = a.diagnostics.iter().map(|d| d.range).collect();
        assert_eq!(
            ranges,
            [range(0, 7, 0, 8), range(1, 7, 1, 8), range(2, 4, 2, 10)]
        );
        assert_eq!(
            a.diagnostics[2].message,
            "unterminated string: \"open}\n\nThis string has no end quote."
        );
        // The unterminated string ends the parse inside the map, which
        // recovery force-pops: its symbol ends at its open token, and the
        // completed list on the second line stands beside it.
        assert_eq!(
            names(&a.outline),
            vec![("Object", vec![]), ("Array", vec![])]
        );
        assert_eq!(a.outline[0].range, range(0, 0, 0, 1));
        assert_eq!(a.outline[1].range, range(1, 4, 1, 9));
    }

    #[test]
    fn an_error_at_the_end_of_source_has_an_empty_range_there() {
        let (instances, inst, entry) = stack();
        let a = analyze(&instances, &inst, &entry, &doc("[1,2"));
        assert_eq!(codes(&a), ["unexpected"]);
        assert_eq!(a.diagnostics[0].range, range(0, 4, 0, 4));
        assert_eq!(
            a.diagnostics[0].message,
            "unexpected character(s): \n\nThe character(s)  do not match any rule alternative active at\nthis position."
        );
        // The force-popped list still has a symbol, ending at its open
        // token.
        assert_eq!(names(&a.outline), vec![("Array", vec![])]);
        assert_eq!(a.outline[0].range, range(0, 0, 0, 1));
        assert_eq!(json(&a), Some(serde_json::json!([1.0])));

        // Two force-popped containers do not nest: their spans are
        // empty, so the canonical pipeline lists them side by side.
        let a = analyze(&instances, &inst, &entry, &doc(r#"{"a":[1,2"#));
        assert_eq!(
            names(&a.outline),
            vec![("Object", vec![]), ("Array", vec![])]
        );
        assert_eq!(a.outline[0].range, range(0, 0, 0, 1));
        assert_eq!(a.outline[1].range, range(0, 5, 0, 6));
        assert_eq!(a.diagnostics[0].range, range(0, 9, 0, 9));
    }

    #[test]
    fn positions_are_counted_in_the_document_encoding() {
        let (instances, inst, entry) = stack();
        // An astral character is one scalar value and two UTF-16 units:
        // the error after it starts at character 5 on the wire.
        let text = "\"\u{1D11E}\" q";
        let a = analyze(&instances, &inst, &entry, &doc(text));
        assert_eq!(codes(&a), ["unexpected"]);
        assert_eq!(a.diagnostics[0].range, range(0, 5, 0, 6));
        assert!(a.outline.is_empty());
        // A UTF-8 client counts the same error four bytes further on.
        let bytes = doc(text).with_encoding(PositionEncoding::Utf8);
        let a = analyze(&instances, &inst, &entry, &bytes);
        assert_eq!(a.diagnostics[0].range, range(0, 7, 0, 8));

        // Semantic token data follows the encoding too: the string is 4
        // UTF-16 units and 6 bytes, so `q`... is not a token, but the
        // `[` after a key is: {"𝄞":[1]} puts `[` at unit 5, byte 7.
        let text = "{\"\u{1D11E}\":[1]}";
        let units = analyze(&instances, &inst, &entry, &doc(text));
        let bytes = analyze(
            &instances,
            &inst,
            &entry,
            &doc(text).with_encoding(PositionEncoding::Utf8),
        );
        let units = units.semantic_tokens.unwrap();
        let bytes = bytes.semantic_tokens.unwrap();
        // The decoded tokens are the UTF-16 form either way.
        assert_eq!(units.tokens, bytes.tokens);
        // `data`: {, "𝄞", :, [, 1, ], } as (dLine, dChar, len, type, 0).
        assert_eq!(
            units.data,
            [
                0, 0, 1, 4, 0, 0, 1, 4, 0, 0, 0, 4, 1, 4, 0, 0, 1, 1, 4, 0, 0, 1, 1, 1, 0, 0, 1, 1,
                4, 0, 0, 1, 1, 4, 0
            ]
        );
        assert_eq!(
            bytes.data,
            [
                0, 0, 1, 4, 0, 0, 1, 6, 0, 0, 0, 6, 1, 4, 0, 0, 1, 1, 4, 0, 0, 1, 1, 1, 0, 0, 1, 1,
                4, 0, 0, 1, 1, 4, 0
            ]
        );
    }

    #[test]
    fn columns_restart_where_the_engine_restarts_them() {
        // A lone `\r` resets the engine's column without starting a
        // line, and the canonical positions are the engine's (`cI - 1`),
        // so the inner lists sit at the TypeScript columns: measured
        // from the `\r`, not from the line's start. TypeScript gives
        // exactly these ranges and this token data for this text.
        let (instances, inst, entry) = stack();
        let a = analyze(&instances, &inst, &entry, &doc("[1,\r[\"\u{1D11E}\",[2]]]"));
        assert!(a.diagnostics.is_empty());
        assert_eq!(a.outline[0].range, range(0, 0, 0, 11));
        assert_eq!(a.outline[0].children[0].range, range(0, 0, 0, 10));
        assert_eq!(
            a.outline[0].children[0].children[0].range,
            range(0, 6, 0, 9)
        );
        assert_eq!(
            a.semantic_tokens.unwrap().data,
            [
                0, 0, 1, 4, 0, 0, 1, 1, 1, 0, 0, 1, 1, 4, 0, 0, 3, 1, 4, 0, 0, 1, 1, 4, 0, 0, 1, 1,
                1, 0, 0, 1, 1, 4, 0, 0, 1, 1, 4, 0, 0, 1, 1, 4, 0
            ]
        );

        // A `\r\n` pair ends a line at the `\r`: the list opened before
        // it closes on the next line.
        let a = analyze(&instances, &inst, &entry, &doc("[\"\u{1D11E}\"\r,[\r\n]]"));
        assert!(a.diagnostics.is_empty());
        assert_eq!(a.outline[0].range, range(0, 0, 1, 2));
        assert_eq!(a.outline[0].children[0].range, range(0, 1, 1, 1));
    }

    #[test]
    fn semantic_tokens_are_gated_on_a_clean_lex_stream() {
        let (instances, inst, entry) = stack();
        let mut speculative = entry.clone();
        speculative.lex_stream = Some("speculative".into());
        let a = analyze(&instances, &inst, &speculative, &doc(r#"{"a":1}"#));
        assert!(a.semantic_tokens.is_none());
        // Diagnostics and the outline are served regardless.
        assert!(a.diagnostics.is_empty());
        assert_eq!(names(&a.outline), vec![("Object", vec![])]);
        assert!(!a.reconciled.is_empty());
        // And the direct form agrees with the analysis of a clean entry.
        let clean = analyze(&instances, &inst, &entry, &doc(r#"{"a":1}"#));
        let direct = semantic_tokens_of_reconciled(&clean.reconciled, &entry, &doc(r#"{"a":1}"#));
        assert_eq!(Some(direct), clean.semantic_tokens);
    }

    #[test]
    fn a_fail_fast_parse_is_failed_with_its_one_diagnostic() {
        // A host that built the instance without recovery: the parse
        // stops at the first error, which is both the fatal one and the
        // list's only entry, so the diagnostic appears once.
        let make: MakeInstance = Arc::new(|_entry| {
            let mut parser = Tabnas::new();
            parser.grammar_json(&fixture("json-grammar.json")).unwrap();
            Ok(parser)
        });
        let mut instances = Instances::new(make);
        let entry = entry();
        let inst = instances.get(&entry, None).unwrap().unwrap();
        let a = analyze(
            &instances,
            &inst,
            &entry,
            &doc(r#"{"a":true blah,"b":false blah}"#),
        );
        assert!(a.failed);
        assert_eq!(codes(&a), ["unexpected"]);
        assert_eq!(a.diagnostics[0].range, range(0, 10, 0, 14));
        assert_eq!(a.value, None);
        // The tokens lexed before the failure are still served.
        assert!(a.semantic_tokens.is_some_and(|t| !t.data.is_empty()));
    }

    #[test]
    fn an_internal_error_is_the_one_diagnostic_of_a_failed_analysis() {
        // A recovery result the engine builds after catching a panic: no
        // value, no recovered errors, the internal error as `fatal`. The
        // canonical `analyze` turns exactly that into its one diagnostic
        // (`if (e.internal) errors = [e]`) and marks the analysis failed.
        let internal = TabnasError::new("internal", "", "{\"a\":1}", 0, 1, 1);
        let recovery = ParseRecovery {
            value: None,
            errors: Vec::new(),
            fatal: Some(internal.clone()),
        };
        let (value, errors) = errors_of(recovery);
        assert_eq!(value, None);
        assert_eq!(errors, vec![internal.clone()]);
        // When the list already ends with the terminal error (recovery
        // off), it is not repeated.
        let recovery = ParseRecovery {
            value: None,
            errors: vec![internal.clone()],
            fatal: Some(internal),
        };
        assert_eq!(errors_of(recovery).1.len(), 1);
    }

    #[test]
    fn diagnostics_take_their_source_from_the_entry_and_skip_the_unknown_registry_page() {
        let text = "{\"a\":1 # 2}";
        let unknown = TabnasError::new("unknown", "#", text, 7, 1, 8);
        let d = diagnostics(std::slice::from_ref(&unknown), None, &doc(text));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].source, "tabnas");
        assert_eq!(d[0].code.as_deref(), Some("unknown"));
        assert_eq!(
            d[0].code_description, None,
            "no registry page for `unknown`"
        );
        assert_eq!(d[0].range, range(0, 7, 0, 8));
        assert_eq!(d[0].severity, 1);
        // With an entry, the source names the language id, and a known
        // code links its page.
        let mut entry = Entry::new("@tabnas/toml");
        let known = TabnasError::new("unexpected", "#", text, 7, 1, 8);
        let d = diagnostics(&[unknown, known], Some(&entry), &doc(text));
        assert_eq!(d[0].source, "tabnas:toml");
        assert_eq!(
            d[1].code_description.as_ref().map(|c| c.href.as_str()),
            Some("https://tabnas.dev/errors/unexpected")
        );
        entry.language_id = Some("tml".into());
        assert_eq!(
            diagnostics(&d_errors(text), Some(&entry), &doc(text))[0].source,
            "tabnas:tml"
        );
        // A message without a hint carries no blank line.
        let mut bare = TabnasError::new("unexpected", "#", text, 7, 1, 8);
        bare.hint = String::new();
        let d = diagnostics(&[bare], None, &doc(text));
        assert_eq!(d[0].message, "unexpected character(s): #");
    }

    fn d_errors(text: &str) -> Vec<TabnasError> {
        vec![TabnasError::new("unexpected", "#", text, 7, 1, 8)]
    }

    #[test]
    fn the_same_instance_analyzes_document_after_document() {
        // The mux collects for the current parse only: a second document
        // sees none of the first one's tokens or symbols, and the
        // results are stable across repeats.
        let (instances, inst, entry) = stack();
        let first = analyze(&instances, &inst, &entry, &doc(r#"{"a":[1,2]}"#));
        let second = analyze(&instances, &inst, &entry, &doc("[true]"));
        assert_eq!(second.reconciled.len(), 4);
        assert_eq!(names(&second.outline), vec![("Array", vec![])]);
        assert!(second.diagnostics.is_empty());
        for _ in 0..3 {
            let again = analyze(&instances, &inst, &entry, &doc(r#"{"a":[1,2]}"#));
            assert_eq!(again.reconciled, first.reconciled);
            assert_eq!(again.outline, first.outline);
            assert_eq!(again.semantic_tokens, first.semantic_tokens);
        }
    }
}
