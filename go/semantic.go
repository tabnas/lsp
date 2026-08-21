/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

// Semantic tokens from the reconciled lex trace, mirroring
// ts/src/core.js: newest event per position wins, spans shadow
// interior positions; CANON default map + prefix conventions +
// per-entry overrides; the fixed superset legend so hot-added grammars
// never force re-registration.

import (
	"regexp"
	"sort"
)

// DefaultTokenTypes maps engine-standard token names (railroad's CANON
// key set) to LSP semantic token types. #ID is per-plugin and comes
// from entry overrides.
var DefaultTokenTypes = map[string]string{
	"#ST": "string",
	"#NR": "number",
	"#CM": "comment",
	"#VL": "keyword",
	"#TX": "string",
	"#OB": "operator",
	"#CB": "operator",
	"#OS": "operator",
	"#CS": "operator",
	"#CL": "operator",
	"#CA": "operator",
}

var prefixTypes = []struct {
	re  *regexp.Regexp
	typ string
}{
	{regexp.MustCompile(`^KW_`), "keyword"},
	{regexp.MustCompile(`^LIT_`), "string"},
	{regexp.MustCompile(`^TRIVIA_`), "comment"},
	{regexp.MustCompile(`^PP_`), "macro"},
	{regexp.MustCompile(`^PUNC_`), "operator"},
	{regexp.MustCompile(`^ID$|^#ID$`), "variable"},
}

// Legend is the fixed superset legend (design §11).
var Legend = []string{
	"string", "number", "comment", "keyword", "operator", "variable",
	"macro", "type", "property",
}

var legendIndex = func() map[string]int {
	m := map[string]int{}
	for i, t := range Legend {
		m[t] = i
	}
	return m
}()

// TokenType resolves an engine token name to an LSP token type, "" for
// none.
func TokenType(name string, overrides map[string]string) string {
	if nil != overrides {
		if t, ok := overrides[name]; ok {
			return t
		}
	}
	if t, ok := DefaultTokenTypes[name]; ok {
		return t
	}
	for _, p := range prefixTypes {
		if p.re.MatchString(name) {
			return p.typ
		}
	}
	return ""
}

// SemanticTokens is the LSP result plus the legend it indexes.
type SemanticTokens struct {
	Data   []int    `json:"data"`
	Legend []string `json:"-"`
}

// reconcile reconstructs the final token list per the documented
// lex-trace contract: iterate newest-first, a claimed byte span
// shadows any older event starting inside it.
func reconcile(events []tokenPoint) []tokenPoint {
	type span struct{ s, e int }
	var claimed []span
	var out []tokenPoint
	for i := len(events) - 1; i >= 0; i-- {
		t := events[i]
		ln := srcLenBytes(t.Src)
		shadowed := false
		for _, c := range claimed {
			if t.SI >= c.s && t.SI < c.e {
				shadowed = true
				break
			}
		}
		if shadowed {
			continue
		}
		claimed = append(claimed, span{t.SI, t.SI + ln})
		out = append(out, t)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].SI < out[j].SI })
	return out
}

// SemanticTokensOf derives the delta-encoded token data (line and
// character deltas in UTF-16 units, per the negotiated encoding).
func SemanticTokensOf(events []tokenPoint, entry *Entry, doc *Doc) *SemanticTokens {
	var overrides map[string]string
	if nil != entry {
		overrides = entry.SemanticTokens
	}
	data := []int{}
	prevLine, prevChar := 0, 0
	for _, t := range reconcile(events) {
		typ := TokenType(t.Name, overrides)
		if "" == typ {
			continue
		}
		typeI, ok := legendIndex[typ]
		if !ok {
			continue
		}
		pos := doc.PosFromEngine(t.RI, t.CI)
		length := srcLenUTF16(t.Src)
		dLine := pos.Line - prevLine
		dChar := pos.Character
		if 0 == dLine {
			dChar = pos.Character - prevChar
		}
		if dLine < 0 || (0 == dLine && dChar < 0) {
			continue // out-of-order guard
		}
		data = append(data, dLine, dChar, length, typeI, 0)
		prevLine = pos.Line
		prevChar = pos.Character
	}
	return &SemanticTokens{Data: data, Legend: Legend}
}
