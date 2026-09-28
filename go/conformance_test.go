/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

// The cross-runtime conformance suite (design §13): the same fixtures
// ts/test/conformance.test.js and rs/tests/conformance_test.rs run. TS
// is canonical — a mismatch here is a defect in this port, never a
// fixture update.

import (
	"encoding/json"
	"sort"
	"testing"
)

type confOutline struct {
	Name     string        `json:"name"`
	Children []confOutline `json:"children"`
}

type confSuite struct {
	Analyze []struct {
		Name       string        `json:"name"`
		Input      string        `json:"input"`
		Codes      []string      `json:"codes"`
		FirstRange *Range        `json:"firstRange"`
		Outline    []confOutline `json:"outline"`
	} `json:"analyze"`
	Completions []struct {
		Name     string   `json:"name"`
		Input    string   `json:"input"`
		Position Position `json:"position"`
		Labels   []string `json:"labels"`
	} `json:"completions"`
	Outlines []struct {
		Name    string            `json:"name"`
		Input   string            `json:"input"`
		Symbols []*DocumentSymbol `json:"symbols"`
	} `json:"outlines"`
	Semantic []struct {
		Name      string            `json:"name"`
		Input     string            `json:"input"`
		Overrides map[string]string `json:"overrides"`
		Errors    int               `json:"errors"`
		Tokens    [][]any           `json:"tokens"`
		Data      []int             `json:"data"`
	} `json:"semantic"`
}

// decodeTokens is the fixture's `tokens` form of a `data` array: the
// deltas resolved through the legend. It keeps the two forms of each
// case honest with each other, as the TS runner's decodeTokens does.
func decodeTokens(data []int) [][]any {
	rows := [][]any{}
	line, char := 0, 0
	for i := 0; i+4 < len(data); i += 5 {
		line += data[i]
		if 0 == data[i] {
			char += data[i+1]
		} else {
			char = data[i+1]
		}
		rows = append(rows, []any{line, char, data[i+2], Legend[data[i+3]]})
	}
	return rows
}

func outlineNames(list []*DocumentSymbol) []confOutline {
	out := []confOutline{}
	for _, s := range list {
		out = append(out, confOutline{
			Name:     s.Name,
			Children: outlineNames(s.Children),
		})
	}
	return out
}

func TestConformance(t *testing.T) {
	var suite confSuite
	if err := json.Unmarshal(fixture(t, "lsp-conformance.json"), &suite); err != nil {
		t.Fatalf("lsp-conformance.json: %v", err)
	}

	instances, inst, entry := makeStack(t)

	for _, c := range suite.Analyze {
		t.Run("analyze: "+c.Name, func(t *testing.T) {
			a := Analyze(instances, inst, entry, doc(c.Input))
			codes := []string{}
			for _, d := range a.Diagnostics {
				codes = append(codes, d.Code)
			}
			if enc(codes) != enc(c.Codes) {
				t.Fatalf("codes = %s, want %s", enc(codes), enc(c.Codes))
			}
			if nil != c.FirstRange {
				if 0 == len(a.Diagnostics) {
					t.Fatal("no diagnostics to check firstRange against")
				}
				if enc(a.Diagnostics[0].Range) != enc(*c.FirstRange) {
					t.Fatalf("firstRange = %s, want %s",
						enc(a.Diagnostics[0].Range), enc(*c.FirstRange))
				}
			}
			if enc(outlineNames(a.Outline)) != enc(c.Outline) {
				t.Fatalf("outline = %s, want %s",
					enc(outlineNames(a.Outline)), enc(c.Outline))
			}
		})
	}

	for _, c := range suite.Completions {
		t.Run("completion: "+c.Name, func(t *testing.T) {
			items := Complete(instances, inst, doc(c.Input), c.Position)
			labels := []string{}
			for _, i := range items {
				labels = append(labels, i.Label)
			}
			sort.Strings(labels)
			if enc(labels) != enc(c.Labels) {
				t.Fatalf("labels = %s, want %s", enc(labels), enc(c.Labels))
			}
		})
	}

	for _, c := range suite.Outlines {
		t.Run("outline: "+c.Name, func(t *testing.T) {
			a := Analyze(instances, inst, entry, doc(c.Input))
			if enc(a.Outline) != enc(c.Symbols) {
				t.Fatalf("outline = %s, want %s", enc(a.Outline), enc(c.Symbols))
			}
		})
	}

	for _, c := range suite.Semantic {
		t.Run("semantic: "+c.Name, func(t *testing.T) {
			spec := *entry
			spec.SemanticTokens = c.Overrides
			a := Analyze(instances, inst, &spec, doc(c.Input))
			if len(a.Errors) != c.Errors {
				t.Fatalf("errors = %d, want %d", len(a.Errors), c.Errors)
			}
			if nil == a.SemanticTokens {
				t.Fatal("no semantic tokens for a clean entry")
			}
			if enc(a.SemanticTokens.Data) != enc(c.Data) {
				t.Fatalf("data = %s, want %s", enc(a.SemanticTokens.Data), enc(c.Data))
			}
			if enc(decodeTokens(c.Data)) != enc(c.Tokens) {
				t.Fatalf("tokens = %s, want %s", enc(decodeTokens(c.Data)), enc(c.Tokens))
			}
		})
	}
}

func enc(v any) string {
	b, err := json.Marshal(v)
	if err != nil {
		return "<unmarshalable>"
	}
	return string(b)
}
