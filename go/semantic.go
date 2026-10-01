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
	"strings"
	"unicode/utf8"
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

// prefixTypes are the conventions for grammars with their own token
// schemes. The keyword one also takes the whole name #KW, as the
// identifier one takes ID and #ID: alchemy's lexer gives its `:name`
// keywords that name.
var prefixTypes = []struct {
	re  *regexp.Regexp
	typ string
}{
	{regexp.MustCompile(`^KW_|^#KW$`), "keyword"},
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

// anchor puts a token where its source text is, mirroring the TS
// core's anchor(). The lex-trace contract places a token at
// [SI, SI+len(Src)), and reconcile shadows by that span. A grammar's
// matcher can build its token from the cursor AFTER its text instead
// (@tabnas/toml's string matcher does, deliberately, in every runtime),
// and the token that starts there, lexed later, then shadows it: no
// TOML string was ever coloured. A token whose source text ends at SI
// rather than starting there is moved back onto that text, its row by
// the line feeds it spans and its column by its length in runes, or,
// when it spans lines, from the text before it. Every other token is
// returned as it is, so a grammar that keeps the contract sees no
// change.
func anchor(t tokenPoint, text string) tokenPoint {
	n := len(t.Src)
	if 0 == n || t.SI < n || t.SI > len(text) {
		return t
	}
	if strings.HasPrefix(text[t.SI:], t.Src) {
		return t
	}
	start := t.SI - n
	if text[start:t.SI] != t.Src {
		return t
	}
	ci := t.CI - utf8.RuneCountInString(t.Src)
	if strings.ContainsAny(t.Src, "\r\n") || ci < 1 {
		// The engine restarts the column at a line feed and at a lone CR.
		brk := strings.LastIndexAny(text[:start], "\r\n")
		ci = 1 + utf8.RuneCountInString(text[brk+1:start])
	}
	t.RI -= strings.Count(t.Src, "\n")
	t.SI = start
	t.CI = ci
	return t
}

// reconcile reconstructs the final token list per the documented
// lex-trace contract: iterate newest-first, a claimed byte span
// shadows any older event starting inside it. Each token is first put
// where its text is in text, the source that was parsed (anchor).
func reconcile(events []tokenPoint, text string) []tokenPoint {
	type span struct{ s, e int }
	var claimed []span
	var out []tokenPoint
	for i := len(events) - 1; i >= 0; i-- {
		t := anchor(events[i], text)
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
// Tokens spanning lines are split into line-local spans, mirroring the
// TS core: multiline semantic tokens are an optional client
// capability, and an unsplit one mis-highlights or is rejected by
// clients without it.
func SemanticTokensOf(events []tokenPoint, entry *Entry, doc *Doc) *SemanticTokens {
	var overrides map[string]string
	if nil != entry {
		overrides = entry.SemanticTokens
	}
	text := ""
	if nil != doc {
		text = doc.Text
	}
	data := []int{}
	prevLine, prevChar := 0, 0
	emit := func(line, char, length, typeI int) {
		if length < 1 {
			return
		}
		dLine := line - prevLine
		dChar := char
		if 0 == dLine {
			dChar = char - prevChar
		}
		if dLine < 0 || (0 == dLine && dChar < 0) {
			return // out-of-order guard
		}
		data = append(data, dLine, dChar, length, typeI, 0)
		prevLine = line
		prevChar = char
	}
	for _, t := range reconcile(events, text) {
		typ := TokenType(t.Name, overrides)
		if "" == typ {
			continue
		}
		typeI, ok := legendIndex[typ]
		if !ok {
			continue
		}
		// The column is measured from the text right before the token,
		// as the TS core's cI-1 is, rather than from the start of its
		// line: the engine restarts CI at a lone '\r' without starting
		// a row (see utf16ColumnBefore).
		line := t.RI - 1
		if line < 0 {
			line = 0
		}
		col := doc.utf16ColumnBefore(t.SI, t.CI)
		if strings.Contains(t.Src, "\n") {
			for i, part := range strings.Split(t.Src, "\n") {
				seg := strings.TrimSuffix(part, "\r")
				char := 0
				if 0 == i {
					char = col
				}
				emit(line+i, char, UTF16Len(seg), typeI)
			}
		} else {
			emit(line, col, srcLenUTF16(t.Src), typeI)
		}
	}
	return &SemanticTokens{Data: data, Legend: Legend}
}
