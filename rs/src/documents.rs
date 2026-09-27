// Copyright (c) 2026 Richard Rodger, MIT License

//! Documents: the line index and every position conversion (design §9).
//! All encoding knowledge lives here and only here. Three units are in
//! play, and every conversion walks the actual document text because an
//! astral code point is one scalar value and TWO UTF-16 units:
//!
//! - The Rust engine's positions: `row` 1-based; `col` 1-based in
//!   Unicode scalar values since the last line or lone-CR reset (see
//!   `Columns` in [`crate::semantic`]); a token's `si` a UTF-8 BYTE
//!   offset and `pos` a scalar-value offset into the source.
//! - A diagnostic's `len`: Unicode scalar values of the token source
//!   (the cross-runtime unit, `schema/diagnostic.schema.json`).
//! - LSP positions: `line` 0-based; `character` in the document's
//!   negotiated [`PositionEncoding`](crate::types::PositionEncoding), UTF-16
//!   code units by default and
//!   UTF-8 bytes when the client offered them.
//!
//! Mirrors `Doc` and `DocumentStore` in `ts/src/documents.js`
//! (canonical; its engine counts UTF-16 units, so its `posFrom` is a
//! subtraction where this one walks the text) and `go/documents.go`
//! (the closest model: rune columns, byte offsets, UTF-16 wire units).
//!
//! Status: [`Doc::update`], [`Doc::line_starts`], [`utf16_len`] and the
//! [`DocumentStore`] are complete; the conversions are signatures for
//! the documents module agent, with the fixture's astral case
//! (`"𝄞" q` gives `firstRange` 5..6) and the lone-CR column rule to
//! pin in tests.

use std::collections::HashMap;

use crate::types::{Doc, Position, Range};

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

    /// A byte offset into the text as a wire position in the document's
    /// encoding: the line it falls on, and the encoded length of the
    /// text from that line's start to the offset. Offsets past the end
    /// clamp to the end. `positionAt` in the canonical stores.
    #[allow(unused_variables)] // stub
    pub fn position_at(&self, offset: usize) -> Position {
        todo!("documents::Doc::position_at")
    }

    /// A wire position as a byte offset: a `character` past the line's
    /// content clamps to the line's end (the protocol convention), a
    /// `line` past the last to the text's end. `offsetAt` in the
    /// canonical stores.
    #[allow(unused_variables)] // stub
    pub fn offset_at(&self, position: Position) -> usize {
        todo!("documents::Doc::offset_at")
    }

    /// An engine `(row, col)` (1-based, `col` in scalar values) as a
    /// wire position. `posFrom` in TypeScript, `PosFromEngine` in Go.
    #[allow(unused_variables)] // stub
    pub fn position_from_engine(&self, row: usize, col: usize) -> Position {
        todo!("documents::Doc::position_from_engine")
    }

    /// An engine diagnostic's place as a wire [`Range`]: `row` and `col`
    /// as [`Doc::position_from_engine`], `pos` the scalar-value offset of
    /// the token (the Rust engine's `TabnasError::pos`; TypeScript's is
    /// UTF-16 and Go has none, so the two canonical forms differ here
    /// and both derive the same range from the text), `len` the token
    /// source's length in scalar values. The end is walked through the
    /// text so astral characters convert and a token spanning lines ends
    /// on the right line; a positive `len` that overshoots the text
    /// still yields a non-empty range. `rangeFrom` / `RangeFrom`.
    #[allow(unused_variables)] // stub
    pub fn range_from(&self, row: usize, col: usize, pos: usize, len: usize) -> Range {
        todo!("documents::Doc::range_from")
    }

    /// Apply one `contentChanges` item: a ranged edit replaces the
    /// bytes between the range's offsets, a full change (`None`) replaces
    /// the whole text. The line index is refreshed after EVERY change,
    /// full ones included, so a later ranged edit in the same
    /// notification computes offsets against the text that exists (the
    /// mixed full-then-ranged case both canonical servers fixed). The
    /// version is left to the caller.
    #[allow(unused_variables)] // stub
    pub fn apply_change(&mut self, range: Option<Range>, text: &str) {
        todo!("documents::Doc::apply_change")
    }
}

/// The length of a text in UTF-16 code units.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The open documents, by URI (`DocumentStore` in both canonical
/// stores).
#[derive(Debug, Clone, Default)]
pub struct DocumentStore {
    docs: HashMap<String, Doc>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open (or replace) a document, in the store's default encoding.
    pub fn open(
        &mut self,
        uri: impl Into<String>,
        language_id: impl Into<String>,
        version: i64,
        text: impl Into<String>,
    ) -> &mut Doc {
        let uri = uri.into();
        let doc = Doc::new(uri.clone(), language_id, version, text);
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

    #[test]
    fn line_starts_are_byte_offsets_and_reset_on_update() {
        let mut doc = Doc::new("file:///t", "t", 1, "aé\nb\r\nc");
        assert_eq!(doc.line_starts(), &[0, 4, 7]);
        doc.update("x", 2);
        assert_eq!(doc.version, 2);
        assert_eq!(doc.line_starts(), &[0]);
        let empty = Doc::new("file:///e", "t", 1, "");
        assert_eq!(empty.line_starts(), &[0]);
    }

    #[test]
    fn utf16_length_counts_astral_characters_twice() {
        assert_eq!(utf16_len(""), 0);
        assert_eq!(utf16_len("aé"), 2);
        assert_eq!(utf16_len("\u{1D11E}"), 2);
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
    }
}
