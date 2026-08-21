/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

import (
	"sort"
	"strings"
	"unicode/utf8"
)

// Document store: line index and position-encoding conversion. All
// encoding knowledge lives here and only here (design §9). Three
// units are in play:
//
//   - Go engine positions: Row 1-based; Col 1-based in RUNES; token
//     SI a BYTE offset into the source.
//   - Diagnostic `len`: Unicode CODE POINTS of the token source (the
//     cross-runtime unit; schema/diagnostic.schema.json).
//   - LSP positions: line 0-based; character in UTF-16 code units
//     (the protocol default this server negotiates).
//
// Every conversion walks the actual document text, because an astral
// code point is one rune, one code point, and TWO UTF-16 units.

// Position is an LSP position (0-based line, UTF-16 character).
type Position struct {
	Line      int `json:"line"`
	Character int `json:"character"`
}

// Range is an LSP range.
type Range struct {
	Start Position `json:"start"`
	End   Position `json:"end"`
}

// Doc is one open document.
type Doc struct {
	URI        string
	LanguageID string
	Version    int
	Text       string

	lineStarts []int // byte offsets, lazily built
}

func (d *Doc) Update(text string, version int) {
	d.Text = text
	d.Version = version
	d.lineStarts = nil
}

// LineStarts returns the byte offset of each line start.
func (d *Doc) LineStarts() []int {
	if d.lineStarts == nil {
		starts := []int{0}
		for i := 0; i < len(d.Text); i++ {
			if '\n' == d.Text[i] {
				starts = append(starts, i+1)
			}
		}
		d.lineStarts = starts
	}
	return d.lineStarts
}

// lineSpan returns the byte range [start, end) of a 0-based line,
// excluding the trailing newline.
func (d *Doc) lineSpan(line int) (int, int) {
	starts := d.LineStarts()
	if line < 0 {
		line = 0
	}
	if line >= len(starts) {
		line = len(starts) - 1
	}
	start := starts[line]
	end := len(d.Text)
	if line+1 < len(starts) {
		end = starts[line+1] - 1 // strip '\n'
	}
	if end > start && '\r' == d.Text[end-1] {
		end--
	}
	return start, end
}

func utf16Units(r rune) int {
	if r > 0xFFFF {
		return 2
	}
	return 1
}

// UTF16Len counts UTF-16 code units of a string.
func UTF16Len(s string) int {
	n := 0
	for _, r := range s {
		n += utf16Units(r)
	}
	return n
}

// PosFromEngine converts an engine (row, col) — 1-based, col in runes
// — to an LSP Position.
func (d *Doc) PosFromEngine(row, col int) Position {
	line := row - 1
	if line < 0 {
		line = 0
	}
	start, end := d.lineSpan(line)
	ch := 0
	runeI := 0
	for _, r := range d.Text[start:end] {
		if runeI >= col-1 {
			break
		}
		ch += utf16Units(r)
		runeI++
	}
	return Position{Line: line, Character: ch}
}

// byteOffsetOfEngine returns the byte offset of an engine (row, col).
func (d *Doc) byteOffsetOfEngine(row, col int) int {
	line := row - 1
	if line < 0 {
		line = 0
	}
	start, end := d.lineSpan(line)
	runeI := 0
	for i, r := range d.Text[start:end] {
		if runeI >= col-1 {
			return start + i
		}
		_ = r
		runeI++
	}
	return end
}

// RangeFrom converts an engine diagnostic position (row, col) plus a
// token length in CODE POINTS into an LSP Range, walking the document
// text so astral characters convert correctly and multi-line tokens
// end on the right line.
func (d *Doc) RangeFrom(row, col, lenCodePoints int) Range {
	start := d.PosFromEngine(row, col)
	from := d.byteOffsetOfEngine(row, col)
	endByte := from
	cp := 0
	for cp < lenCodePoints && endByte < len(d.Text) {
		_, size := utf8.DecodeRuneInString(d.Text[endByte:])
		endByte += size
		cp++
	}
	// A positive-length token gets a non-empty range even when the
	// reported length overshoots the text (mirrors the TS conversion).
	if lenCodePoints > 0 && endByte == from && from < len(d.Text) {
		_, size := utf8.DecodeRuneInString(d.Text[from:])
		endByte = from + size
	}
	return Range{Start: start, End: d.PositionAt(endByte)}
}

// OffsetAt converts an LSP Position to a byte offset.
func (d *Doc) OffsetAt(p Position) int {
	start, end := d.lineSpan(p.Line)
	units := 0
	for i, r := range d.Text[start:end] {
		if units >= p.Character {
			return start + i
		}
		units += utf16Units(r)
	}
	// Past the line's content: clamp to line end (the LSP convention
	// treats over-long characters as end-of-line).
	return end
}

// PositionAt converts a byte offset to an LSP Position.
func (d *Doc) PositionAt(offset int) Position {
	if offset < 0 {
		offset = 0
	}
	if offset > len(d.Text) {
		offset = len(d.Text)
	}
	starts := d.LineStarts()
	line := sort.Search(len(starts), func(i int) bool {
		return starts[i] > offset
	}) - 1
	if line < 0 {
		line = 0
	}
	return Position{
		Line:      line,
		Character: UTF16Len(d.Text[starts[line]:offset]),
	}
}

// DocumentStore holds the open documents.
type DocumentStore struct {
	docs map[string]*Doc
}

func NewDocumentStore() *DocumentStore {
	return &DocumentStore{docs: map[string]*Doc{}}
}

func (s *DocumentStore) Open(uri, languageID string, version int, text string) *Doc {
	d := &Doc{URI: uri, LanguageID: languageID, Version: version, Text: text}
	s.docs[uri] = d
	return d
}

func (s *DocumentStore) Get(uri string) *Doc { return s.docs[uri] }

func (s *DocumentStore) Close(uri string) { delete(s.docs, uri) }

// ExtOf returns the lowercased extension of a URI or path, "" if none.
func ExtOf(uri string) string {
	i := strings.LastIndexByte(uri, '.')
	if i < 0 || strings.ContainsAny(uri[i:], "/\\") {
		return ""
	}
	return strings.ToLower(uri[i:])
}
