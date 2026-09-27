// Copyright (c) 2026 Richard Rodger, MIT License

//! Hover (design §8): the token under the cursor plus its description,
//! degrading to nothing. Mirrors `onHover` in `ts/src/server.js`, which
//! today answers `null` for every position (token descriptions are
//! tracked work, design §14); the Go port has no hover. This module is
//! the seam for that feature: [`token_at`] finds the token in the
//! analysis's reconciled trace, [`hover`] asks `describe` what the
//! server can say about it and answers with that over the token's
//! range. `describe` has nothing to say yet, so `hover` answers `None`
//! for every position, exactly as TypeScript does; when TypeScript ships
//! descriptions, a `hover` fixture section pins the content and
//! `describe` is where this port mirrors it.
//!
//! Positions go through the document: the wire position is a byte
//! offset ([`Doc::offset_at`]), and the token whose source covers that
//! byte is the one under the cursor.

use serde::{Deserialize, Serialize};

use crate::analyze::Analysis;
use crate::trace::TokenPoint;
use crate::types::{Doc, Entry, Position, Range};

/// LSP `MarkupContent`; `kind` is `markdown` or `plaintext`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkupContent {
    pub kind: String,
    pub value: String,
}

impl MarkupContent {
    /// Markdown content.
    pub fn markdown(value: impl Into<String>) -> Self {
        MarkupContent {
            kind: "markdown".into(),
            value: value.into(),
        }
    }

    /// Plain-text content.
    pub fn plaintext(value: impl Into<String>) -> Self {
        MarkupContent {
            kind: "plaintext".into(),
            value: value.into(),
        }
    }
}

/// An LSP hover result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hover {
    pub contents: MarkupContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

/// The hover at a position over the current analysis of `doc`: the
/// description of the token there, over the token's range. `None` when
/// there is nothing to say: no token under the cursor, or no description
/// for it, which today is every token (see the module docs). The server
/// calls this only with an analysis of the document's current version,
/// as the canonical server serves hover from `currentAnalysis` alone.
pub fn hover(analysis: &Analysis, entry: &Entry, doc: &Doc, position: Position) -> Option<Hover> {
    let token = token_at(&analysis.reconciled, doc, position)?;
    let description = describe(token, entry)?;
    Some(Hover {
        contents: MarkupContent::markdown(description),
        range: Some(token_range(token, doc)),
    })
}

/// What the server can say about a token: nothing yet. The canonical
/// `onHover` returns `null` for every position and the conformance
/// fixture has no `hover` section, so there is no content to mirror; a
/// description source added here arrives with the TypeScript one and the
/// fixture section that pins both.
fn describe(_token: &TokenPoint, _entry: &Entry) -> Option<String> {
    None
}

/// The reconciled token whose source covers a wire position, if any:
/// the token with `si <= offset < si + len` for the position's byte
/// offset. A zero-length token (the end-of-source sentinel, an empty bad
/// token) covers nothing and is never under the cursor. The trace is
/// scanned rather than searched so that a host's hand-built trace need
/// not be sorted.
pub fn token_at<'a>(
    reconciled: &'a [TokenPoint],
    doc: &Doc,
    position: Position,
) -> Option<&'a TokenPoint> {
    let offset = doc.offset_at(position);
    reconciled
        .iter()
        .find(|token| token.si <= offset && offset < token.si.saturating_add(token.len))
}

/// A token's source as a wire range, through the document text.
pub fn token_range(token: &TokenPoint, doc: &Doc) -> Range {
    let end = token.si.saturating_add(token.len).min(doc.text.len());
    Range::new(doc.position_at(token.si), doc.position_at(end))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tabnas::{Options, Tabnas};

    use super::*;
    use crate::analyze::analyze;
    use crate::instances::Instances;
    use crate::types::{MakeInstance, PositionEncoding};

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

    fn doc(text: &str) -> Doc {
        Doc::new("file:///t.jsonf", "jsonf", 1, text)
    }

    fn at(line: u32, character: u32) -> Position {
        Position::new(line, character)
    }

    /// The reconciled trace of `{"a":[1,\n"𝄞"]}` as the engine reports
    /// it, the end-of-source sentinel included.
    fn trace() -> Vec<TokenPoint> {
        vec![
            token("#OB", 0, 1, 1, "{"),
            token("#ST", 1, 1, 2, "\"a\""),
            token("#CL", 4, 1, 5, ":"),
            token("#OS", 5, 1, 6, "["),
            token("#NR", 6, 1, 7, "1"),
            token("#CA", 7, 1, 8, ","),
            token("#ST", 9, 2, 1, "\"\u{1D11E}\""),
            token("#CS", 15, 2, 5, "]"),
            token("#CB", 16, 2, 6, "}"),
            token("#ZZ", 17, 2, 7, ""),
        ]
    }

    const TEXT: &str = "{\"a\":[1,\n\"\u{1D11E}\"]}";

    #[test]
    fn the_token_under_the_cursor_covers_the_position() {
        let doc = doc(TEXT);
        let trace = trace();
        let names = |position: Position| token_at(&trace, &doc, position).map(|t| t.name.as_str());
        assert_eq!(names(at(0, 0)), Some("#OB"));
        // Inside a token, and at its first character.
        assert_eq!(names(at(0, 1)), Some("#ST"));
        assert_eq!(names(at(0, 2)), Some("#ST"));
        assert_eq!(names(at(0, 3)), Some("#ST"));
        assert_eq!(names(at(0, 4)), Some("#CL"));
        assert_eq!(names(at(0, 6)), Some("#NR"));
        // The position after the last token on a line is the line break,
        // which no token covers.
        assert_eq!(names(at(0, 8)), None);
        // On the second line the astral character takes two units; the
        // string covers units 0 to 3 and `]` sits at 4.
        assert_eq!(names(at(1, 0)), Some("#ST"));
        assert_eq!(names(at(1, 2)), Some("#ST"));
        assert_eq!(names(at(1, 3)), Some("#ST"));
        assert_eq!(names(at(1, 4)), Some("#CS"));
        assert_eq!(names(at(1, 5)), Some("#CB"));
        // The end of the document: only the zero-length sentinel is
        // there, and it covers nothing.
        assert_eq!(names(at(1, 6)), None);
        assert_eq!(names(at(1, 99)), None);
        // A line past the last is the last line, as the canonical
        // `offsetAt` clamps it: its first character is under the cursor.
        assert_eq!(names(at(9, 0)), Some("#ST"));
        // In UTF-8 the same tokens sit at byte columns.
        let bytes =
            Doc::new("file:///t.jsonf", "jsonf", 1, TEXT).with_encoding(PositionEncoding::Utf8);
        assert_eq!(
            token_at(&trace, &bytes, at(1, 5)).map(|t| t.name.as_str()),
            Some("#ST")
        );
        assert_eq!(
            token_at(&trace, &bytes, at(1, 6)).map(|t| t.name.as_str()),
            Some("#CS")
        );
        // An empty trace has no token anywhere.
        assert_eq!(token_at(&[], &doc, at(0, 0)), None);
    }

    #[test]
    fn a_token_range_is_its_source_on_the_wire() {
        let doc = doc(TEXT);
        let trace = trace();
        assert_eq!(token_range(&trace[1], &doc), Range::new(at(0, 1), at(0, 4)));
        assert_eq!(token_range(&trace[6], &doc), Range::new(at(1, 0), at(1, 4)));
        // A token claiming more than the text holds ends at the text.
        let long = token("#TX", 16, 2, 6, "}}}}");
        assert_eq!(token_range(&long, &doc), Range::new(at(1, 5), at(1, 6)));
        assert!(
            serde_json::to_value(Hover {
                contents: MarkupContent::plaintext("x"),
                range: None,
            })
            .unwrap()
            .get("range")
            .is_none(),
            "an absent range is omitted"
        );
        assert_eq!(MarkupContent::markdown("m").kind, "markdown");
    }

    #[test]
    fn hover_answers_nothing_as_the_canonical_server_does() {
        // The token is found; there is no description for it, so the
        // answer is `None` at every position, tokens and gaps alike.
        let make: MakeInstance = Arc::new(|_entry| {
            let mut options = Options::default();
            options.parse.recover.enabled = true;
            let mut parser = Tabnas::with_options(options);
            let spec = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../test/fixtures/json-grammar.json"
            ))
            .expect("json-grammar.json");
            parser
                .grammar_json(&spec)
                .expect("json-grammar.json installs");
            Ok(parser)
        });
        let mut instances = Instances::new(make);
        let mut entry = Entry::new("jsonf");
        entry.language_id = Some("jsonf".into());
        let inst = instances.get(&entry, None).unwrap().unwrap();
        let doc = doc(TEXT);
        let analysis = analyze(&instances, &inst, &entry, &doc);
        assert!(analysis.diagnostics.is_empty());
        assert!(
            token_at(&analysis.reconciled, &doc, at(0, 1)).is_some(),
            "the string is under the cursor"
        );
        for line in 0..2 {
            for character in 0..8 {
                assert_eq!(hover(&analysis, &entry, &doc, at(line, character)), None);
            }
        }
        assert_eq!(describe(&analysis.reconciled[0], &entry), None);
    }
}
