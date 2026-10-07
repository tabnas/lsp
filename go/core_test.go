/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

// The Go mirror of ts/test/core.test.js, against the shared pure-data
// strict-JSON grammar (test/fixtures/json-grammar.json — the L2 lane
// every runtime loads, which is exactly what makes it the parity test
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

func TestLoneCRColumnIsMeasuredBeforeTheToken(t *testing.T) {
	// A lone '\r' restarts the engine column on the same row. The
	// canonical column counts UTF-16 units from that restart, so it is
	// measured from the text right before the token: after
	// "{\"😀\":1,\r" the ':' at engine column 12 is 11 units in (the
	// key before it), not the 12 a walk from the line start counts.
	d := doc("{\"😀\":1,\r\"bbbbbbbbb\":2}")
	if c := d.utf16ColumnBefore(22, 12); 11 != c {
		t.Fatalf("column after a lone CR: got %d, want 11", c)
	}
	// Without a reset the two measures agree: the astral key is two
	// units, so the ':' at engine column 5 is 5 units in.
	c, p := d.utf16ColumnBefore(7, 5), d.PosFromEngine(1, 5)
	if 5 != c || c != p.Character {
		t.Fatalf("column without a reset: got %d and %d, want 5", c, p.Character)
	}
	// Past the text, or asking for more runes than precede the token,
	// measures what is there.
	if c := d.utf16ColumnBefore(99, 3); 2 != c {
		t.Fatalf("column past the text: got %d, want 2", c)
	}
	if c := d.utf16ColumnBefore(1, 99); 1 != c {
		t.Fatalf("column asking past the start: got %d, want 1", c)
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

func TestKeywordConventionTakesTheWholeNameHashKW(t *testing.T) {
	// alchemy's lexer names its `:name` keywords #KW: no CANON name, and
	// not the KW_ prefix, so they went uncoloured.
	for name, want := range map[string]string{
		"#KW": "keyword", "KW_if": "keyword", "#KWX": "", "#KW_": "",
	} {
		if got := TokenType(name, nil); got != want {
			t.Errorf("TokenType(%q) = %q, want %q", name, got, want)
		}
	}
	if got := TokenType("#KW", map[string]string{"#KW": "property"}); "property" != got {
		t.Errorf("an entry's override must still win: got %q", got)
	}
}

// anchored returns the reconciled token named name, failing when the
// reconciliation dropped it.
func anchored(t *testing.T, events []tokenPoint, text, name string) tokenPoint {
	t.Helper()
	for _, k := range reconcile(events, text) {
		if name == k.Name {
			return k
		}
	}
	t.Fatalf("%s did not survive reconciliation", name)
	return tokenPoint{}
}

func TestATokenReportedAtTheEndOfItsTextIsPutBackOnIt(t *testing.T) {
	// @tabnas/toml builds each string token from the cursor after the
	// string, so the token that starts there shadowed it and no string
	// was coloured. Go units: SI in bytes, CI in runes.
	text := "k = \"é😀\"\n"
	events := []tokenPoint{
		{Name: "#ID", SI: 0, RI: 1, CI: 1, Src: "k"},
		{Name: "#SP", SI: 1, RI: 1, CI: 2, Src: " "},
		{Name: "#CL", SI: 2, RI: 1, CI: 3, Src: "="},
		{Name: "#SP", SI: 3, RI: 1, CI: 4, Src: " "},
		{Name: "#ST", SI: 12, RI: 1, CI: 9, Src: "\"é😀\""},
		{Name: "#LN", SI: 12, RI: 1, CI: 9, Src: "\n"},
		{Name: "#ZZ", SI: 13, RI: 2, CI: 1, Src: ""},
	}
	st := anchored(t, events, text, "#ST")
	if 4 != st.SI || 1 != st.RI || 5 != st.CI {
		t.Fatalf("string at SI %d RI %d CI %d, want 4 1 5", st.SI, st.RI, st.CI)
	}
	got := SemanticTokensOf(events, nil, doc(text)).Data
	// k, =, then the string: five UTF-16 units at character 4.
	want := []int{0, 0, 1, 5, 0, 0, 2, 1, 4, 0, 0, 2, 5, 0, 0}
	if enc(got) != enc(want) {
		t.Fatalf("data = %v, want %v", got, want)
	}
}

func TestATokenSpanningLinesIsPutBackWhereItStarts(t *testing.T) {
	// The row moves back by the line feeds the token spans; the column
	// is measured from the text before it, from the lone CR that
	// restarted it.
	text := "x\rc = '''\nz'''\n"
	events := []tokenPoint{
		{Name: "#ID", SI: 0, RI: 1, CI: 1, Src: "x"},
		{Name: "#LN", SI: 1, RI: 1, CI: 2, Src: "\r"},
		{Name: "#ID", SI: 2, RI: 1, CI: 1, Src: "c"},
		{Name: "#CL", SI: 4, RI: 1, CI: 3, Src: "="},
		{Name: "#ST", SI: 14, RI: 2, CI: 5, Src: "'''\nz'''"},
		{Name: "#LN", SI: 14, RI: 2, CI: 5, Src: "\n"},
	}
	st := anchored(t, events, text, "#ST")
	if 6 != st.SI || 1 != st.RI || 5 != st.CI {
		t.Fatalf("string at SI %d RI %d CI %d, want 6 1 5", st.SI, st.RI, st.CI)
	}
	got := SemanticTokensOf(events, nil, doc(text)).Data
	// x, c (column 0 after the lone CR), =, ''' and z''' on the next row.
	want := []int{0, 0, 1, 5, 0, 0, 0, 1, 5, 0, 0, 2, 1, 4, 0, 0, 2, 3, 0, 0, 1, 0, 4, 0, 0}
	if enc(got) != enc(want) {
		t.Fatalf("data = %v, want %v", got, want)
	}
}

func TestATokenWhoseTextIsWhereItSaysOrNowhereNearIsLeftAlone(t *testing.T) {
	text := "ab \"s\" cd"
	for _, k := range []tokenPoint{
		{Name: "#ST", SI: 3, RI: 1, CI: 4, Src: "\"s\""},
		{Name: "#ST", SI: 7, RI: 1, CI: 8, Src: "\"t\""},
		{Name: "#ZZ", SI: 9, RI: 1, CI: 10, Src: ""},
		{Name: "#ST", SI: 99, RI: 1, CI: 100, Src: "\"s\""},
	} {
		if got := anchor(k, text); got != k {
			t.Errorf("anchor(%+v) = %+v, want it unchanged", k, got)
		}
	}
}
