/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

// The Go mirror of ts/test/core.test.js, against the shared pure-data
// strict-JSON grammar (test/fixtures/json-grammar.json — the L2 lane
// both runtimes load, which is exactly what makes it the parity test
// grammar).

import (
	"os"
	"path/filepath"
	"testing"

	tabnas "github.com/tabnas/parser/go"
)

func fixture(t *testing.T, name string) []byte {
	t.Helper()
	b, err := os.ReadFile(filepath.Join("..", "test", "fixtures", name))
	if err != nil {
		t.Fatalf("fixture %s: %v", name, err)
	}
	return b
}

func makeStack(t *testing.T) (*Instances, *tabnas.Tabnas, *Entry) {
	t.Helper()
	entry, makeInst := EntryFromSpecJSON("jsonf", []string{".jsonf"}, fixture(t, "json-grammar.json"))
	instances := NewInstances(makeInst)
	inst, err := instances.Get(entry)
	if err != nil {
		t.Fatalf("instance: %v", err)
	}
	return instances, inst, entry
}

func doc(text string) *Doc {
	return &Doc{URI: "file:///t.jsonf", LanguageID: "jsonf", Version: 1, Text: text}
}

func TestCleanParse(t *testing.T) {
	instances, inst, entry := makeStack(t)
	a := Analyze(instances, inst, entry, doc(`{"a":[1,2]}`))
	if 0 != len(a.Diagnostics) {
		t.Fatalf("diagnostics on a clean parse: %+v", a.Diagnostics)
	}
	if nil == a.SemanticTokens || 0 == len(a.SemanticTokens.Data) {
		t.Fatal("no semantic tokens emitted")
	}
	if 1 != len(a.Outline) {
		t.Fatalf("outline roots = %d, want 1", len(a.Outline))
	}
	if "Object" != a.Outline[0].Name {
		t.Fatalf("root symbol = %s, want Object", a.Outline[0].Name)
	}
	if 1 != len(a.Outline[0].Children) || "Array" != a.Outline[0].Children[0].Name {
		t.Fatalf("child symbols = %+v, want one Array", a.Outline[0].Children)
	}
}

func TestBrokenDocumentDiagnostics(t *testing.T) {
	instances, inst, entry := makeStack(t)
	a := Analyze(instances, inst, entry, doc(`{"a":true blah,"b":2}`))
	if 1 > len(a.Diagnostics) {
		t.Fatal("no diagnostics for a broken document")
	}
	d := a.Diagnostics[0]
	if 1 != d.Severity {
		t.Fatalf("severity = %d", d.Severity)
	}
	if 0 != d.Range.Start.Line {
		t.Fatalf("line = %d", d.Range.Start.Line)
	}
	if 10 > d.Range.Start.Character {
		t.Fatalf("range should sit at the bad word, got %+v", d.Range)
	}
	if nil == d.CodeDescription {
		t.Fatal("no codeDescription link")
	}
}

func TestMultiErrorDiagnostics(t *testing.T) {
	instances, inst, entry := makeStack(t)
	a := Analyze(instances, inst, entry,
		doc(`{"a":true blah,"b":false blah,"c":true blah}`))
	if 2 > len(a.Diagnostics) {
		t.Fatalf("recovery should yield multiple diagnostics, got %d", len(a.Diagnostics))
	}
}

func TestSemanticTokensDeltaEncoding(t *testing.T) {
	instances, inst, entry := makeStack(t)
	a := Analyze(instances, inst, entry, doc("{\"a\":1,\n\"b\":2}"))
	data := a.SemanticTokens.Data
	if 0 != len(data)%5 {
		t.Fatalf("data length %d not a multiple of 5", len(data))
	}
	sawNewline := false
	for i := 0; i < len(data); i += 5 {
		if 1 == data[i] {
			sawNewline = true
		}
	}
	if !sawNewline {
		t.Fatal("delta encoding never crossed the newline")
	}
}

func TestLexStreamGate(t *testing.T) {
	instances, inst, entry := makeStack(t)
	spec := *entry
	spec.LexStream = "speculative"
	a := Analyze(instances, inst, &spec, doc(`{"a":1}`))
	if nil != a.SemanticTokens {
		t.Fatal("speculative entries must not get semantic tokens")
	}
}

func TestCompletionOffersColonAfterKey(t *testing.T) {
	instances, inst, _ := makeStack(t)
	items := Complete(instances, inst, doc(`{"a"`), Position{Line: 0, Character: 4})
	found := false
	for _, i := range items {
		if ":" == i.Label {
			found = true
		}
	}
	if !found {
		t.Fatalf("no ':' completion after a key: %+v", items)
	}
}

func TestCompletionMidEdit(t *testing.T) {
	instances, inst, _ := makeStack(t)
	items := Complete(instances, inst, doc(`{"a":`), Position{Line: 0, Character: 5})
	if 0 == len(items) {
		t.Fatal("no value starters offered after a colon")
	}
	items2 := Complete(instances, inst, doc(`[1,`), Position{Line: 0, Character: 3})
	found := false
	for _, i := range items2 {
		if "]" == i.Label {
			found = true
		}
	}
	if !found {
		t.Fatalf("no closer offered after a separator: %+v", items2)
	}
}

func TestCompletionDropsSentinels(t *testing.T) {
	instances, inst, _ := makeStack(t)
	items := Complete(instances, inst, doc(`{"a":1}`), Position{Line: 0, Character: 7})
	if 0 != len(items) {
		t.Fatalf("a complete document should offer nothing, got %+v", items)
	}
	items2 := Complete(instances, inst, doc(`[1,`), Position{Line: 0, Character: 3})
	if 0 == len(items2) {
		t.Fatal("real completions must survive the sentinel filter")
	}
	for _, i := range items2 {
		if "#ZZ" == i.Detail {
			t.Fatalf("sentinel leaked: %+v", items2)
		}
	}
}

func TestQuarantine(t *testing.T) {
	bad := &Entry{Name: "bad", LanguageID: "bad", Enabled: true}
	instances := NewInstances(func(e *Entry) (*tabnas.Tabnas, error) {
		return nil, os.ErrInvalid
	})
	for i := 0; i < QuarantineLimit; i++ {
		if _, err := instances.Get(bad); err == nil {
			t.Fatal("expected a load error")
		}
	}
	inst, err := instances.Get(bad)
	if nil != inst || nil != err {
		t.Fatalf("want quarantined (nil, nil), got (%v, %v)", inst, err)
	}
}

func TestRegistryRouting(t *testing.T) {
	reg := NewRegistry([]*Entry{
		{Name: "toml", LanguageID: "toml", Extensions: []string{".toml"}, Enabled: true},
		{Name: "hoover", LanguageID: "hoover", PluginKind: "modifier", Enabled: true},
		{Name: "yaml", LanguageID: "yaml", Extensions: []string{".yaml"}, Enabled: false},
	})
	if e := reg.Resolve("toml", "file:///x.toml").Entry; nil == e || "toml" != e.LanguageID {
		t.Fatal("languageId routing failed")
	}
	if e := reg.Resolve("plaintext", "file:///x.toml").Entry; nil == e || "toml" != e.LanguageID {
		t.Fatal("extension fallback failed")
	}
	if e := reg.Resolve("plaintext", "file:///x.yaml").Entry; nil != e {
		t.Fatal("disabled entries must not route")
	}
	if e := reg.Resolve("hoover", "file:///x.hoover").Entry; nil != e {
		t.Fatal("modifiers must never be routing targets")
	}
}

func TestAstralRangeConversion(t *testing.T) {
	// "€" is one UTF-16 unit; "𝄞" (U+1D11E) is TWO. Engine columns are
	// runes; the LSP range must count UTF-16 units.
	d := doc("\"𝄞x\" q")
	// Engine sees: col 1 '"', col 2 '𝄞', col 3 'x', col 4 '"', col 5
	// ' ', col 6 'q'. In UTF-16: '"'=0, '𝄞'=1..2, 'x'=3, '"'=4, ' '=5,
	// 'q'=6.
	p := d.PosFromEngine(1, 6)
	if 6 != p.Character {
		t.Fatalf("astral conversion off: got %d, want 6", p.Character)
	}
	r := d.RangeFrom(1, 2, 1) // the 𝄞 token itself, len 1 code point
	if 1 != r.Start.Character || 3 != r.End.Character {
		t.Fatalf("astral token range %+v, want chars 1..3", r)
	}
}

func TestMultilineTokensSplitPerLine(t *testing.T) {
	// Mirrors the TS case "multiline tokens split into line-local
	// spans": a block comment spanning lines must not emit one token
	// whose length crosses the line break.
	instances, inst, entry := makeStack(t)
	d := doc("{\"a\":1,/*x\ny*/\"b\":2}")
	a := Analyze(instances, inst, entry, d)
	lines := []string{"{\"a\":1,/*x", "y*/\"b\":2}"}
	line, char := 0, 0
	sawContinuation := false
	data := a.SemanticTokens.Data
	for i := 0; i+4 < len(data); i += 5 {
		if 0 < data[i] {
			line += data[i]
			char = data[i+1]
		} else {
			char += data[i+1]
		}
		length := data[i+2]
		if char+length > UTF16Len(lines[line]) {
			t.Fatalf("token crosses its line: line %d char %d len %d", line, char, length)
		}
		if 1 == line && 0 == char {
			sawContinuation = true
		}
	}
	if !sawContinuation {
		t.Fatal("no continuation span on the second line")
	}
}
