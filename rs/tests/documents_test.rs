// The documents module as a host sees it (design §9): the engine's own
// positions for a document, converted through the text into wire
// positions in the negotiated encoding, and a didChange sequence applied
// the way the server applies one. The astral case is the fixture's
// ("astral character before the error", `"𝄞" q`, firstRange 5..6); the
// rest are the conversions' edges measured from real engine output, so a
// change in the engine's units fails here before it reaches a diagnostic.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tabnas::{Options, Tabnas};
use tabnas_lsp::{
    Doc, DocumentStore, Entry, Instances, MakeInstance, Position, PositionEncoding, Range,
};

fn fixture(name: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rs/ has a parent")
        .join("test")
        .join("fixtures")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn instance() -> Tabnas {
    let mut options = Options::default();
    options.parse.recover.enabled = true;
    let mut parser = Tabnas::with_options(options);
    parser
        .grammar_json(&fixture("json-grammar.json"))
        .expect("json-grammar.json installs");
    parser
}

fn stack() -> (Instances, Arc<Tabnas>) {
    let make: MakeInstance = Arc::new(|_entry| Ok(instance()));
    let mut instances = Instances::new(make);
    let inst = instances
        .get(&Entry::new("jsonf"), None)
        .expect("the fixture grammar loads")
        .expect("a fresh entry is not quarantined");
    (instances, inst)
}

fn at(line: u32, character: u32) -> Position {
    Position::new(line, character)
}

fn range(l0: u32, c0: u32, l1: u32, c1: u32) -> Range {
    Range::new(at(l0, c0), at(l1, c1))
}

/// The first error's range for `text`, converted through a document in
/// `encoding`, from the engine's own `row`, `col`, `pos` and `len`.
fn first_error_range(text: &str, encoding: PositionEncoding) -> Range {
    let (instances, inst) = stack();
    let (recovery, _) = instances.parse(&inst, text);
    let error = recovery
        .errors
        .first()
        .or(recovery.fatal.as_ref())
        .unwrap_or_else(|| panic!("{text:?}: no error"));
    let doc = Doc::new("file:///t.jsonf", "jsonf", 1, text).with_encoding(encoding);
    doc.range_from(error.row, error.col, error.pos, error.len)
}

#[test]
fn the_fixture_astral_range_converts_from_the_engine_positions() {
    // "astral character before the error": the clef is one scalar value
    // and two UTF-16 units, so the `q` after it is at character 5.
    assert_eq!(
        first_error_range("\"\u{1D11E}\" q", PositionEncoding::Utf16),
        range(0, 5, 0, 6)
    );
    // The same document in UTF-8: four bytes for the clef.
    assert_eq!(
        first_error_range("\"\u{1D11E}\" q", PositionEncoding::Utf8),
        range(0, 7, 0, 8)
    );
    // Without an astral character the two encodings agree.
    assert_eq!(
        first_error_range("[1 2]", PositionEncoding::Utf16),
        range(0, 3, 0, 4)
    );
    assert_eq!(
        first_error_range("[1 2]", PositionEncoding::Utf8),
        range(0, 3, 0, 4)
    );
}

#[test]
fn an_error_on_a_later_line_lands_on_that_line() {
    // Rows are 1-based in the engine and lines 0-based on the wire; a
    // CRLF ends the first line at the CR and an astral character before
    // the line break does not disturb the next line's columns.
    assert_eq!(
        first_error_range("[\n  1 2]", PositionEncoding::Utf16),
        range(1, 4, 1, 5)
    );
    assert_eq!(
        first_error_range("[\"\u{1D11E}\",\r\n  1 2]", PositionEncoding::Utf16),
        range(1, 4, 1, 5)
    );
    assert_eq!(
        first_error_range("[\"\u{1D11E}\",\r\n  1 2]", PositionEncoding::Utf8),
        range(1, 4, 1, 5)
    );
}

#[test]
fn an_error_at_the_end_of_the_text_is_an_empty_range_there() {
    let end = first_error_range("{\"a\":", PositionEncoding::Utf16);
    assert_eq!(end.start, end.end, "{end:?}");
    assert_eq!(end.start, at(0, 5));
    let end = first_error_range("[\"\u{1D11E}\",\n", PositionEncoding::Utf16);
    assert_eq!(end.start, end.end, "{end:?}");
    assert_eq!(end.start, at(1, 0));
}

#[test]
fn after_a_lone_cr_the_range_has_the_canonical_shape() {
    // A lone CR restarts the engine's column on the same row. The
    // canonical range starts at the TypeScript `cI - 1` (the 15 UTF-16
    // units since the reset) and ends where the text walk from the line
    // start puts the token's end (25): the values `ts/src/core.js`
    // produces for this document, measured.
    assert_eq!(
        first_error_range(
            "{\"\u{1F600}\":1,\r\"bbbbbbbbb\":2} x",
            PositionEncoding::Utf16
        ),
        range(0, 15, 0, 25)
    );
}

#[test]
fn a_didchange_notification_is_applied_change_by_change() {
    // What the server does with `contentChanges`: every change against
    // the text that exists at that point, then the version stamped once.
    let mut store = DocumentStore::new();
    let doc = store.open("file:///t.jsonf", "jsonf", 1, "aaa\nbbb\nccc");
    doc.apply_changes(
        [
            (None, "x\ny"),
            (Some(range(1, 0, 1, 0)), "Z"),
            (Some(range(1, 2, 1, 2)), "\n"),
            (Some(range(2, 0, 2, 0)), "end"),
        ],
        7,
    );
    let doc = store.get("file:///t.jsonf").expect("open");
    assert_eq!(doc.text, "x\nZy\nend");
    assert_eq!(doc.version, 7);
    assert_eq!(doc.position_at(doc.text.len()), at(2, 3));
    assert_eq!(doc.offset_at(at(2, 3)), doc.text.len());
}

#[test]
fn a_utf8_document_edits_and_measures_in_bytes() {
    let mut doc =
        Doc::new("file:///t", "t", 1, "é\u{1D11E}!").with_encoding(PositionEncoding::Utf8);
    assert_eq!(doc.position_at(doc.text.len()), at(0, 7));
    doc.apply_change(Some(range(0, 2, 0, 6)), "-");
    assert_eq!(doc.text, "é-!");
    assert_eq!(doc.position_from_engine(1, 3), at(0, 3));
    let utf16 = Doc::new("file:///t", "t", 1, "é\u{1D11E}!");
    assert_eq!(utf16.position_at(utf16.text.len()), at(0, 4));
    assert_eq!(utf16.position_from_engine(1, 3), at(0, 3));
}
