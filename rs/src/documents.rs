// Copyright (c) 2026 Richard Rodger, MIT License

//! Documents: the line index and every position conversion (design §9).
//! All encoding knowledge lives here and only here. Three units are in
//! play, and every conversion walks the actual document text because an
//! astral code point is one scalar value, two UTF-16 units and four
//! UTF-8 bytes:
//!
//! - The Rust engine's positions: `row` 1-based; `col` 1-based in
//!   Unicode scalar values since the last line or lone-CR reset (see
//!   `Columns` in [`crate::semantic`]); a token's `si` a UTF-8 BYTE
//!   offset and `pos` a scalar-value offset into the source.
//! - A diagnostic's `len`: Unicode scalar values of the token source
//!   (the cross-runtime unit, `schema/diagnostic.schema.json`).
//! - LSP positions: `line` 0-based; `character` in the document's
//!   negotiated [`PositionEncoding`], UTF-16 code units by default and
//!   UTF-8 bytes when the client offered them.
//!
//! Mirrors `Doc` and `DocumentStore` in `ts/src/documents.js`
//! (canonical; its engine counts UTF-16 units, so its `posFrom` is a
//! subtraction where this one walks the text) and `go/documents.go`
//! (the closest model: rune columns, byte offsets, UTF-16 wire units).
//!
//! Lines are `\n`-delimited, as in both canonical stores: a `\r\n` pair
//! ends a line at the `\r`, and a lone `\r` starts no line. (The
//! protocol counts a lone `\r` as a line break too; neither canonical
//! store nor the engine's rows do, so a client editing a document with
//! lone `\r` line breaks addresses lines none of the three ports has.)
//! Where the TypeScript store and the Go port differ, TypeScript wins,
//! with two exceptions: a `character` past a line's content clamps to
//! that line rather than spilling into the line break and the next line
//! (the protocol's rule and the Go port's; `offsetAt`), which is what
//! keeps a client's edit on the line it named; and a `character` inside
//! a surrogate pair moves to the end of the pair, since a Rust string
//! cannot be cut there.

use std::collections::HashMap;

use crate::trace::TokenPoint;
use crate::types::{Doc, Position, PositionEncoding, Range};

/// A `usize` count as a wire `u32`, saturating (a document of four
/// gigabytes is not one this server edits).
fn wire(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl Doc {
    /// Replace the text and version and drop the line index.
    pub fn update(&mut self, text: impl Into<String>, version: i64) {
        self.text = text.into();
        self.version = version;
        self.line_starts.take();
    }

    /// The byte offset of each line start (`\n`-delimited; a lone `\r`
    /// starts no line here, as in both canonical stores), built on first
    /// use.
    pub fn line_starts(&self) -> &[usize] {
        self.line_starts.get_or_init(|| {
            let mut starts = vec![0];
            starts.extend(
                self.text
                    .bytes()
                    .enumerate()
                    .filter(|&(_, byte)| byte == b'\n')
                    .map(|(i, _)| i + 1),
            );
            starts
        })
    }

    /// How many lines the text has: one more than its `\n`s, so an empty
    /// text has one.
    pub fn line_count(&self) -> usize {
        self.line_starts().len()
    }

    /// The wire length of one character in the document's encoding.
    fn units_of(&self, c: char) -> usize {
        match self.encoding {
            PositionEncoding::Utf16 => c.len_utf16(),
            PositionEncoding::Utf8 => c.len_utf8(),
        }
    }

    /// The wire length of a slice of the text in the document's
    /// encoding.
    fn measure(&self, s: &str) -> usize {
        match self.encoding {
            PositionEncoding::Utf16 => utf16_len(s),
            PositionEncoding::Utf8 => s.len(),
        }
    }

    /// The 0-based line a byte offset (already clamped into the text)
    /// falls on: the last line start at or before it.
    fn line_of(&self, offset: usize) -> usize {
        let starts = self.line_starts();
        starts.partition_point(|&start| start <= offset).max(1) - 1
    }

    /// The byte range `[start, end)` of a 0-based line without its line
    /// break: the `\n`, and a `\r` right before it (or ending the text),
    /// are excluded. A line past the last is the last line. `lineSpan` in
    /// Go.
    pub fn line_span(&self, line: usize) -> (usize, usize) {
        let starts = self.line_starts();
        let line = line.min(starts.len() - 1);
        let start = starts[line];
        let mut end = match starts.get(line + 1) {
            Some(&next) => next - 1,
            None => self.text.len(),
        };
        if end > start && self.text.as_bytes()[end - 1] == b'\r' {
            end -= 1;
        }
        (start, end)
    }

    /// `offset` clamped into the text and moved back to a char boundary
    /// when it points inside a character (engine offsets never do; a
    /// host's might).
    fn boundary(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    /// The byte offset of the `pos`th scalar value of the text, or the
    /// text's length when the text has fewer. The Rust engine's
    /// `TabnasError::pos` (and a token's `site.pos`) count this way.
    pub fn byte_of_scalar(&self, pos: usize) -> usize {
        self.text
            .char_indices()
            .nth(pos)
            .map_or(self.text.len(), |(byte, _)| byte)
    }

    /// The wire length of up to `chars` scalar values immediately before
    /// the byte offset `at`: the whole prefix when fewer precede it.
    /// `utf16ColumnBefore` in Go.
    fn column_before(&self, at: usize, chars: usize) -> usize {
        self.text[..self.boundary(at)]
            .chars()
            .rev()
            .take(chars)
            .map(|c| self.units_of(c))
            .sum()
    }

    /// A byte offset into the text as a wire position in the document's
    /// encoding: the line it falls on, and the encoded length of the
    /// text from that line's start to the offset. Offsets past the end
    /// clamp to the end. `positionAt` in the canonical stores.
    pub fn position_at(&self, offset: usize) -> Position {
        let offset = self.boundary(offset);
        let line = self.line_of(offset);
        let start = self.line_starts()[line];
        Position::new(wire(line), wire(self.measure(&self.text[start..offset])))
    }

    /// A wire position as a byte offset: a `character` past the line's
    /// content clamps to the line's end, before a CRLF's `\r` (the
    /// protocol's rule, and the Go port's; the TypeScript store adds the
    /// character to the line start and so spills into the line break and
    /// the lines after it), and a `line` past the last is the last line
    /// (both canonical stores). A `character` inside a character (the
    /// second unit of a surrogate pair, a byte inside a multi-byte
    /// sequence) rounds up to the boundary after it, as the Go port does:
    /// a Rust string cannot be split there, where a JavaScript one splits
    /// the pair. `offsetAt` in the canonical stores.
    pub fn offset_at(&self, position: Position) -> usize {
        let (start, end) = self.line_span(position.line as usize);
        let want = position.character as usize;
        let mut units = 0;
        for (i, c) in self.text[start..end].char_indices() {
            if units >= want {
                return start + i;
            }
            units += self.units_of(c);
        }
        end
    }

    /// An engine `(row, col)` (1-based, `col` in scalar values) as a
    /// wire position, measured from the line's start: `col - 1` scalar
    /// values along line `row - 1`. `posFrom` in TypeScript,
    /// `PosFromEngine` in Go.
    ///
    /// A lone `\r` resets the engine's column without starting a row, so
    /// after one this measures from the wrong place, as the Go port does.
    /// With the token's byte offset in hand, [`Doc::position_from_site`]
    /// measures the column where the engine counted it and reproduces
    /// the TypeScript value in that case too; prefer it when a token is
    /// known, as [`Doc::range_from`] does.
    pub fn position_from_engine(&self, row: usize, col: usize) -> Position {
        let line = row.saturating_sub(1).min(self.line_count() - 1);
        let (start, end) = self.line_span(line);
        let units = self.text[start..end]
            .chars()
            .take(col.saturating_sub(1))
            .map(|c| self.units_of(c))
            .sum();
        Position::new(wire(line), wire(units))
    }

    /// The wire position of a token at byte offset `si` with engine
    /// `(row, col)`: line `row - 1`, and the wire length of the `col - 1`
    /// scalar values immediately BEFORE `si`. That is what the
    /// TypeScript engine's `cI - 1` is (UTF-16 units since the same
    /// reset the Rust engine counted from), so it holds after a lone
    /// `\r` too, where measuring the `\n`-delimited line from its start
    /// does not. `{line: rI - 1, character: cI - 1}` in `ts/src/core.js`;
    /// `utf16ColumnBefore` in Go.
    pub fn position_from_site(&self, si: usize, row: usize, col: usize) -> Position {
        let line = row.saturating_sub(1).min(self.line_count() - 1);
        Position::new(
            wire(line),
            wire(self.column_before(si, col.saturating_sub(1))),
        )
    }

    /// [`Doc::position_from_site`] for a recorded token event: its start.
    pub fn position_of_token(&self, token: &TokenPoint) -> Position {
        self.position_from_site(token.si, token.ri, token.ci)
    }

    /// An engine diagnostic's place as a wire [`Range`]. `row` and `col`
    /// are the token's, `pos` its scalar-value offset (the Rust engine's
    /// `TabnasError::pos`; TypeScript's is UTF-16 and Go has none, so the
    /// two canonical forms differ here and both derive the same range
    /// from the text), `len` the token source's length in scalar values.
    ///
    /// The start is [`Doc::position_from_site`] at the byte offset `pos`
    /// names, so its column is the TypeScript `cI - 1`; the end is walked
    /// `len` scalar values through the text from there, so astral
    /// characters convert and a token spanning lines ends on the right
    /// line. A `len` that overshoots the text ends at the text's end.
    /// `rangeFrom` / `RangeFrom`.
    pub fn range_from(&self, row: usize, col: usize, pos: usize, len: usize) -> Range {
        let from = self.byte_of_scalar(pos);
        let start = self.position_from_site(from, row, col);
        let mut end = from;
        for c in self.text[from..].chars().take(len) {
            end += c.len_utf8();
        }
        Range::new(start, self.position_at(end))
    }

    /// Apply one `contentChanges` item: a ranged edit replaces the bytes
    /// between the range's offsets, a full change (`None`) replaces the
    /// whole text. The line index is refreshed after EVERY change, full
    /// ones included, so a later ranged edit in the same notification
    /// computes offsets against the text that exists (the mixed
    /// full-then-ranged case both canonical servers fixed). The version
    /// is left to the caller, which stamps the notification's version
    /// once the whole array is applied, as both canonical servers do.
    ///
    /// A range whose end precedes its start is not one the protocol
    /// allows; both canonical servers splice it as written (the text
    /// between the two offsets is kept twice) and never fail, and this
    /// does the same.
    pub fn apply_change(&mut self, range: Option<Range>, text: &str) {
        let next = match range {
            None => text.to_string(),
            Some(range) => {
                let start = self.offset_at(range.start);
                let end = self.offset_at(range.end);
                let mut next = String::with_capacity(self.text.len() + text.len());
                next.push_str(&self.text[..start]);
                next.push_str(text);
                next.push_str(&self.text[end..]);
                next
            }
        };
        self.update(next, self.version);
    }

    /// Apply a `didChange` notification: every change in order, then the
    /// new version. `onDidChangeTextDocument` in `ts/src/server.js`, the
    /// `textDocument/didChange` arm in `go/server.go`.
    pub fn apply_changes<'a>(
        &mut self,
        changes: impl IntoIterator<Item = (Option<Range>, &'a str)>,
        version: i64,
    ) {
        for (range, text) in changes {
            self.apply_change(range, text);
        }
        self.version = version;
    }
}

/// The length of a text in UTF-16 code units.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The open documents, by URI (`DocumentStore` in both canonical
/// stores), and the encoding the session negotiated for them.
#[derive(Debug, Clone, Default)]
pub struct DocumentStore {
    docs: HashMap<String, Doc>,
    encoding: PositionEncoding,
}

impl DocumentStore {
    /// An empty store whose documents count positions in UTF-16, the
    /// protocol default.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty store whose documents count positions in `encoding`, the
    /// one `initialize` negotiated.
    pub fn with_encoding(encoding: PositionEncoding) -> Self {
        DocumentStore {
            docs: HashMap::new(),
            encoding,
        }
    }

    /// The encoding [`DocumentStore::open`] gives new documents.
    pub fn encoding(&self) -> PositionEncoding {
        self.encoding
    }

    /// Change the encoding [`DocumentStore::open`] gives new documents
    /// (a client that negotiates after the store was built). Documents
    /// already open keep theirs.
    pub fn set_encoding(&mut self, encoding: PositionEncoding) {
        self.encoding = encoding;
    }

    /// Open (or replace) a document, in the store's encoding.
    pub fn open(
        &mut self,
        uri: impl Into<String>,
        language_id: impl Into<String>,
        version: i64,
        text: impl Into<String>,
    ) -> &mut Doc {
        let uri = uri.into();
        let doc = Doc::new(uri.clone(), language_id, version, text).with_encoding(self.encoding);
        self.docs.entry(uri).insert_entry(doc).into_mut()
    }

    /// Put a document built elsewhere (with its own encoding) in the
    /// store.
    pub fn insert(&mut self, doc: Doc) -> &mut Doc {
        let uri = doc.uri.clone();
        self.docs.entry(uri).insert_entry(doc).into_mut()
    }

    pub fn get(&self, uri: &str) -> Option<&Doc> {
        self.docs.get(uri)
    }

    pub fn get_mut(&mut self, uri: &str) -> Option<&mut Doc> {
        self.docs.get_mut(uri)
    }

    /// Close a document, returning it.
    pub fn close(&mut self, uri: &str) -> Option<Doc> {
        self.docs.remove(uri)
    }

    /// Every open document, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = &Doc> {
        self.docs.values()
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Doc {
        Doc::new("file:///t", "t", 1, text)
    }

    fn utf8(text: &str) -> Doc {
        doc(text).with_encoding(PositionEncoding::Utf8)
    }

    fn at(line: u32, character: u32) -> Position {
        Position::new(line, character)
    }

    fn range(l0: u32, c0: u32, l1: u32, c1: u32) -> Range {
        Range::new(at(l0, c0), at(l1, c1))
    }

    #[test]
    fn line_starts_are_byte_offsets_and_reset_on_update() {
        let mut doc = doc("aé\nb\r\nc");
        assert_eq!(doc.line_starts(), &[0, 4, 7]);
        assert_eq!(doc.line_count(), 3);
        doc.update("x", 2);
        assert_eq!(doc.version, 2);
        assert_eq!(doc.line_starts(), &[0]);
        let empty = Doc::new("file:///e", "t", 1, "");
        assert_eq!(empty.line_starts(), &[0]);
        assert_eq!(empty.line_count(), 1);
    }

    #[test]
    fn utf16_length_counts_astral_characters_twice() {
        assert_eq!(utf16_len(""), 0);
        assert_eq!(utf16_len("aé"), 2);
        assert_eq!(utf16_len("\u{1D11E}"), 2);
    }

    #[test]
    fn line_spans_exclude_the_line_break_and_a_carriage_return() {
        let d = doc("ab\r\ncd\ne\r");
        assert_eq!(d.line_span(0), (0, 2), "before the CR of a CRLF");
        assert_eq!(d.line_span(1), (4, 6));
        assert_eq!(d.line_span(2), (7, 8), "a trailing CR ending the text");
        assert_eq!(d.line_span(99), (7, 8), "a line past the last is the last");
        assert_eq!(doc("").line_span(0), (0, 0));
        // A lone CR mid-line is content, not a break.
        assert_eq!(doc("a\rb").line_span(0), (0, 3));
        // A CR alone on a line is stripped, leaving the empty line.
        assert_eq!(doc("\r\nx").line_span(0), (0, 0));
    }

    #[test]
    fn positions_and_offsets_round_trip_over_ascii_lines() {
        let d = doc("aaa\nbbb\nccc");
        assert_eq!(d.position_at(0), at(0, 0));
        assert_eq!(d.position_at(3), at(0, 3), "the newline is on its line");
        assert_eq!(d.position_at(4), at(1, 0));
        assert_eq!(d.position_at(10), at(2, 2));
        assert_eq!(d.position_at(11), at(2, 3), "the end of the text");
        assert_eq!(d.position_at(999), at(2, 3), "past the end clamps");
        for offset in 0..=11 {
            assert_eq!(d.offset_at(d.position_at(offset)), offset, "{offset}");
        }
    }

    #[test]
    fn a_character_past_the_line_clamps_to_that_line() {
        let d = doc("aaa\nbbb\nccc");
        assert_eq!(d.offset_at(at(0, 3)), 3, "the line's end");
        assert_eq!(
            d.offset_at(at(0, 4)),
            3,
            "the newline itself is not reachable"
        );
        assert_eq!(d.offset_at(at(0, 99)), 3, "never spills into the next line");
        assert_eq!(d.offset_at(at(1, 99)), 7);
        assert_eq!(d.offset_at(at(2, 99)), 11, "the last line ends at the text");
        assert_eq!(
            d.offset_at(at(99, 0)),
            8,
            "a line past the last is the last"
        );
        assert_eq!(d.offset_at(at(99, 99)), 11);
        assert_eq!(doc("").offset_at(at(5, 5)), 0);
    }

    #[test]
    fn crlf_positions_stay_before_the_carriage_return() {
        let d = doc("ab\r\ncd");
        assert_eq!(d.line_starts(), &[0, 4]);
        assert_eq!(d.offset_at(at(0, 2)), 2);
        assert_eq!(d.offset_at(at(0, 3)), 2, "the CR is not content");
        assert_eq!(d.offset_at(at(0, 99)), 2);
        assert_eq!(d.offset_at(at(1, 0)), 4);
        assert_eq!(d.offset_at(at(1, 2)), 6);
        // Measured back, an offset at the CR or the LF is still on line 0
        // and counts what precedes it, as both canonical stores do.
        assert_eq!(d.position_at(2), at(0, 2));
        assert_eq!(d.position_at(3), at(0, 3));
        assert_eq!(d.position_at(4), at(1, 0));
    }

    #[test]
    fn astral_characters_are_two_utf16_units_and_four_bytes() {
        // "𝄞" is U+1D11E: one scalar value, two UTF-16 units, four bytes.
        let d = doc("\"\u{1D11E}x\" q");
        assert_eq!(d.position_at(0), at(0, 0));
        assert_eq!(d.position_at(1), at(0, 1), "before the clef");
        assert_eq!(d.position_at(5), at(0, 3), "after the clef: two units");
        assert_eq!(d.position_at(8), at(0, 6));
        assert_eq!(d.offset_at(at(0, 1)), 1);
        assert_eq!(d.offset_at(at(0, 3)), 5);
        assert_eq!(d.offset_at(at(0, 6)), 8);
        // Inside the surrogate pair rounds up to the character after it.
        assert_eq!(d.offset_at(at(0, 2)), 5);
        // Inside the four bytes rounds down to the boundary before.
        assert_eq!(d.position_at(3), at(0, 1));

        let u = utf8("\"\u{1D11E}x\" q");
        assert_eq!(u.position_at(5), at(0, 5), "bytes: four for the clef");
        assert_eq!(u.position_at(8), at(0, 8));
        assert_eq!(u.offset_at(at(0, 5)), 5);
        assert_eq!(u.offset_at(at(0, 3)), 5, "a byte inside the clef rounds up");
        assert_eq!(u.offset_at(at(0, 8)), 8);
    }

    #[test]
    fn multi_byte_bmp_characters_are_one_unit_and_several_bytes() {
        let d = doc("é€\nx");
        assert_eq!(d.position_at(2), at(0, 1));
        assert_eq!(d.position_at(5), at(0, 2));
        assert_eq!(d.position_at(6), at(1, 0));
        assert_eq!(d.offset_at(at(0, 2)), 5);
        let u = utf8("é€\nx");
        assert_eq!(u.position_at(5), at(0, 5));
        assert_eq!(u.offset_at(at(0, 2)), 2);
        assert_eq!(u.offset_at(at(0, 1)), 2, "rounds up out of the é");
    }

    #[test]
    fn engine_columns_convert_through_the_text() {
        // The Go port's TestAstralRangeConversion: engine columns are
        // scalar values, the wire counts UTF-16 units. Engine: col 1 '"',
        // col 2 '𝄞', col 3 'x', col 4 '"', col 5 ' ', col 6 'q'.
        let d = doc("\"\u{1D11E}x\" q");
        assert_eq!(d.position_from_engine(1, 6), at(0, 6));
        assert_eq!(d.position_from_engine(1, 2), at(0, 1));
        assert_eq!(d.position_from_engine(1, 3), at(0, 3));
        assert_eq!(d.position_from_engine(1, 1), at(0, 0));
        assert_eq!(
            d.position_from_engine(0, 0),
            at(0, 0),
            "row 0 and col 0 are clamped"
        );
        assert_eq!(
            d.position_from_engine(1, 99),
            at(0, 7),
            "past the line's content"
        );
        assert_eq!(d.position_from_engine(9, 1), at(0, 0), "past the last row");
        let u = utf8("\"\u{1D11E}x\" q");
        assert_eq!(u.position_from_engine(1, 6), at(0, 8));
        assert_eq!(u.position_from_engine(1, 3), at(0, 5));
        // Rows: the second line's first column.
        let m = doc("[\n  1]");
        assert_eq!(m.position_from_engine(2, 3), at(1, 2));
        // A CRLF's CR is not content of its line.
        assert_eq!(doc("ab\r\ncd").position_from_engine(1, 4), at(0, 2));
    }

    #[test]
    fn a_site_measures_its_column_before_the_token() {
        // The Go port's TestLoneCRColumnIsMeasuredBeforeTheToken. A lone
        // CR restarts the engine column on the same row: after
        // "{\"😀\":1,\r" the ':' at byte 22 has engine column 12, and the
        // canonical column is the 11 units of the key right before it,
        // not the 12 a walk from the line start counts.
        let text = "{\"\u{1F600}\":1,\r\"bbbbbbbbb\":2}";
        let d = doc(text);
        assert_eq!(d.position_from_site(22, 1, 12), at(0, 11));
        assert_eq!(
            d.position_from_engine(1, 12),
            at(0, 12),
            "the Go measure differs here"
        );
        // Without a reset the two agree: the astral key is two units, so
        // the ':' at engine column 5 is 5 units in.
        assert_eq!(d.position_from_site(7, 1, 5), at(0, 5));
        assert_eq!(d.position_from_engine(1, 5), at(0, 5));
        // Past the text, or asking for more characters than precede the
        // token, measures what is there.
        assert_eq!(d.position_from_site(99, 1, 3), at(0, 2));
        assert_eq!(d.position_from_site(1, 1, 99), at(0, 1));
        assert_eq!(d.position_from_site(0, 1, 1), at(0, 0));
        // In UTF-8 the same site is measured in bytes.
        let u = utf8(text);
        assert_eq!(u.position_from_site(22, 1, 12), at(0, 11));
        assert_eq!(u.position_from_site(7, 1, 5), at(0, 7));
        // A token event carries its own site.
        let token = TokenPoint {
            name: "#CL".into(),
            si: 22,
            ri: 1,
            ci: 12,
            len: 1,
            src: ":".into(),
        };
        assert_eq!(d.position_of_token(&token), at(0, 11));
        // The row picks the line; a row past the last is the last.
        let m = doc("[\n  1]");
        assert_eq!(m.position_from_site(4, 2, 3), at(1, 2));
        assert_eq!(m.position_from_site(4, 9, 3), at(1, 2));
    }

    #[test]
    fn scalar_offsets_become_byte_offsets() {
        let d = doc("a\u{1D11E}é");
        assert_eq!(d.byte_of_scalar(0), 0);
        assert_eq!(d.byte_of_scalar(1), 1);
        assert_eq!(d.byte_of_scalar(2), 5);
        assert_eq!(d.byte_of_scalar(3), 7, "the end");
        assert_eq!(d.byte_of_scalar(4), 7, "past the end clamps");
        assert_eq!(doc("").byte_of_scalar(0), 0);
    }

    #[test]
    fn a_diagnostic_range_converts_its_length_from_scalar_values() {
        // The fixture's "astral character before the error": in `"𝄞" q`
        // the `q` is engine (row 1, col 5, pos 4, len 1) and its range is
        // 5..6 in UTF-16 units.
        let d = doc("\"\u{1D11E}\" q");
        assert_eq!(d.range_from(1, 5, 4, 1), range(0, 5, 0, 6));
        // The clef itself: one scalar value, two units (Go's
        // TestAstralRangeConversion).
        let d = doc("\"\u{1D11E}x\" q");
        assert_eq!(d.range_from(1, 2, 1, 1), range(0, 1, 0, 3));
        assert_eq!(
            utf8("\"\u{1D11E}x\" q").range_from(1, 2, 1, 1),
            range(0, 1, 0, 5)
        );
        // A token spanning lines ends on the right line.
        let d = doc("{\"a\":`x\ny`}");
        assert_eq!(d.range_from(1, 6, 5, 5), range(0, 5, 1, 2));
        // A length past the text ends at the text.
        let d = doc("[1");
        assert_eq!(d.range_from(1, 2, 1, 99), range(0, 1, 0, 2));
        // At the end of the text (the engine's end-of-source error) the
        // range is empty there, as the canonical conversion yields.
        assert_eq!(d.range_from(1, 3, 2, 1), range(0, 2, 0, 2));
        assert_eq!(d.range_from(1, 3, 2, 0), range(0, 2, 0, 2));
        // A zero length is an empty range at the token.
        assert_eq!(d.range_from(1, 1, 0, 0), range(0, 0, 0, 0));
        // After a lone CR the start column is the TypeScript `cI - 1`
        // (measured since the reset) while the end, walked from the
        // token through the text, is measured from the line's start:
        // the canonical conversion's own shape on that edge.
        let d = doc("{\"\u{1F600}\":1,\r\"bbbbbbbbb\":2}");
        assert_eq!(d.range_from(1, 12, 19, 1), range(0, 11, 0, 21));
    }

    #[test]
    fn a_full_change_replaces_the_text_and_keeps_the_version() {
        let mut d = doc("aaa\nbbb");
        d.apply_change(None, "x");
        assert_eq!(d.text, "x");
        assert_eq!(d.version, 1, "the version is the caller's to stamp");
        assert_eq!(d.line_starts(), &[0], "the index follows the text");
    }

    #[test]
    fn a_full_replacement_mixed_with_a_ranged_edit_keeps_the_index_live() {
        // The TypeScript server test of the same name and Go's
        // TestDidChangeMixedFullAndRanged: against a stale index the
        // ranged edit's line 1 resolved to offset 4, past the end of
        // "x\ny", and appended.
        let mut d = doc("aaa\nbbb\nccc");
        d.apply_changes([(None, "x\ny"), (Some(range(1, 0, 1, 0)), "Z")], 2);
        assert_eq!(d.text, "x\nZy");
        assert_eq!(d.version, 2);
    }

    #[test]
    fn consecutive_ranged_edits_compose() {
        let mut d = doc("[]");
        d.apply_changes(
            [
                (Some(range(0, 1, 0, 1)), "1"),
                (Some(range(0, 2, 0, 2)), ",2"),
            ],
            2,
        );
        assert_eq!(d.text, "[1,2]");
        // A deletion, and a replacement spanning lines.
        d.apply_change(Some(range(0, 1, 0, 2)), "");
        assert_eq!(d.text, "[,2]");
        let mut m = doc("aaa\nbbb\nccc");
        m.apply_change(Some(range(0, 1, 2, 1)), "-");
        assert_eq!(m.text, "a-cc");
        assert_eq!(m.line_starts(), &[0]);
    }

    #[test]
    fn an_edit_at_the_end_of_the_file_appends() {
        let mut d = doc("ab\ncd");
        d.apply_change(Some(range(1, 2, 1, 2)), "e");
        assert_eq!(d.text, "ab\ncde");
        // A position past the end, on a line past the last, is the end.
        d.apply_change(Some(range(9, 9, 9, 9)), "\nf");
        assert_eq!(d.text, "ab\ncde\nf");
        assert_eq!(d.line_starts(), &[0, 3, 7]);
        // The empty document takes its first text through an edit.
        let mut e = doc("");
        e.apply_change(Some(range(0, 0, 0, 0)), "[]");
        assert_eq!(e.text, "[]");
        // Appending after a trailing newline lands on the new last line.
        let mut n = doc("a\n");
        n.apply_change(Some(range(1, 0, 1, 0)), "b");
        assert_eq!(n.text, "a\nb");
    }

    #[test]
    fn ranged_edits_address_the_document_in_its_encoding() {
        // UTF-16: the clef is two units, so replacing 1..3 replaces it.
        let mut d = doc("\"\u{1D11E}x\"");
        d.apply_change(Some(range(0, 1, 0, 3)), "y");
        assert_eq!(d.text, "\"yx\"");
        // UTF-8: the same edit in bytes is 1..5.
        let mut u = utf8("\"\u{1D11E}x\"");
        u.apply_change(Some(range(0, 1, 0, 5)), "y");
        assert_eq!(u.text, "\"yx\"");
        // CRLF: an insertion at the line's end lands before the CR, and
        // a character past the content clamps there too.
        let mut c = doc("ab\r\ncd");
        c.apply_change(Some(range(0, 2, 0, 2)), "!");
        assert_eq!(c.text, "ab!\r\ncd");
        c.apply_change(Some(range(0, 99, 0, 99)), "?");
        assert_eq!(c.text, "ab!?\r\ncd");
        c.apply_change(Some(range(1, 0, 1, 0)), ">");
        assert_eq!(c.text, "ab!?\r\n>cd");
    }

    #[test]
    fn a_reversed_range_splices_as_the_canonical_servers_do() {
        // Not a range the protocol allows; both canonical servers keep
        // the text between the offsets twice and carry on, and so does
        // this.
        let mut d = doc("abcd");
        d.apply_change(Some(range(0, 3, 0, 1)), "-");
        assert_eq!(d.text, "abc-bcd");
    }

    #[test]
    fn the_store_opens_documents_in_its_negotiated_encoding() {
        let mut store = DocumentStore::new();
        assert_eq!(store.encoding(), PositionEncoding::Utf16);
        let d = store.open("file:///a", "json", 1, "\u{1D11E}x");
        assert_eq!(d.encoding, PositionEncoding::Utf16);
        assert_eq!(d.position_at(4), at(0, 2));

        let mut store = DocumentStore::with_encoding(PositionEncoding::Utf8);
        let d = store.open("file:///a", "json", 1, "\u{1D11E}x");
        assert_eq!(d.encoding, PositionEncoding::Utf8);
        assert_eq!(d.position_at(4), at(0, 4));

        // A later negotiation applies to documents opened after it.
        let mut store = DocumentStore::new();
        store.open("file:///old", "json", 1, "é");
        store.set_encoding(PositionEncoding::Utf8);
        store.open("file:///new", "json", 1, "é");
        assert_eq!(
            store.get("file:///old").unwrap().encoding,
            PositionEncoding::Utf16
        );
        assert_eq!(
            store.get("file:///new").unwrap().encoding,
            PositionEncoding::Utf8
        );
    }

    #[test]
    fn the_store_opens_replaces_and_closes() {
        let mut store = DocumentStore::new();
        assert!(store.is_empty());
        store.open("file:///a", "json", 1, "[]");
        store.open("file:///a", "json", 2, "{}");
        assert_eq!(store.len(), 1);
        assert_eq!(store.get("file:///a").map(|d| d.version), Some(2));
        store.get_mut("file:///a").unwrap().update("1", 3);
        assert_eq!(store.iter().count(), 1);
        assert_eq!(store.close("file:///a").map(|d| d.text), Some("1".into()));
        assert!(store.close("file:///a").is_none());
        // A document with its own encoding keeps it in the store.
        let inserted = store.insert(utf8("é"));
        assert_eq!(inserted.encoding, PositionEncoding::Utf8);
        assert_eq!(store.get("file:///t").unwrap().position_at(2), at(0, 2));
    }
}
