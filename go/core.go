/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

// The parse pipeline, mirroring ts/src/core.js: ONE parse per change,
// with the mux collectors installed, yields diagnostics, semantic
// tokens, and outline together. Instance management mirrors
// ts/src/instances.js: the engine has no unsubscribe API, so exactly
// one permanent mux subscriber pair is installed at instance creation,
// forwarding to whichever collector the serialized parse has made
// active; a nil active collector (e.g. during Continuations' internal
// parses) makes the mux a no-op.

import (
	"encoding/json"
	"sync"
	"unicode/utf8"

	tabnas "github.com/tabnas/parser/go"
)

const QuarantineLimit = 3

// tokenPoint is the position slice of one token event, in engine
// units (SI bytes, RI rows, CI rune columns) plus the source text.
type tokenPoint struct {
	Name string
	SI   int
	RI   int
	CI   int
	Src  string
}

// ruleEvent is one post-process rule event.
type ruleEvent struct {
	I      int
	Name   string
	State  string // "o" | "c"
	Forced bool
	O0     *tokenPoint
	C0     *tokenPoint
}

// collector receives one parse's events.
type collector struct {
	lexEvents  []tokenPoint
	ruleEvents []ruleEvent
}

// Instances caches engine instances per entry, serializes parses, and
// quarantines repeatedly failing grammars (design §6).
type Instances struct {
	mu       sync.Mutex // guards cache and failures
	parseMu  sync.Mutex // serializes parses; guards active
	makeInst MakeInstance
	cache    map[string]*tabnas.Tabnas
	failures map[string]int
	active   *collector
}

func NewInstances(makeInst MakeInstance) *Instances {
	return &Instances{
		makeInst: makeInst,
		cache:    map[string]*tabnas.Tabnas{},
		failures: map[string]int{},
	}
}

func (s *Instances) Quarantined(e *Entry) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return QuarantineLimit <= s.failures[e.LanguageID]
}

func (s *Instances) RecordFailure(e *Entry) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.failures[e.LanguageID]++
}

// Get returns the cached instance for an entry, building it (and
// installing the permanent mux) on first use. nil when quarantined.
func (s *Instances) Get(e *Entry) (*tabnas.Tabnas, error) {
	s.mu.Lock()
	if QuarantineLimit <= s.failures[e.LanguageID] {
		s.mu.Unlock()
		return nil, nil
	}
	if inst, ok := s.cache[e.LanguageID]; ok {
		s.mu.Unlock()
		return inst, nil
	}
	s.mu.Unlock()

	inst, err := s.makeInst(e)
	if err != nil {
		s.RecordFailure(e)
		return nil, err
	}
	s.installMux(inst)

	s.mu.Lock()
	s.cache[e.LanguageID] = inst
	s.mu.Unlock()
	return inst, nil
}

// Invalidate drops an entry's cached instance (grammar hot-reload:
// rebuild, never re-apply — Grammar() prepends alternates).
func (s *Instances) Invalidate(e *Entry) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.cache, e.LanguageID)
	delete(s.failures, e.LanguageID)
}

// installMux installs the ONE permanent subscriber pair. The engine
// converts a panicking subscriber into an `internal` parse error, so
// the mux guards with recover() — a collector bug must never abort a
// user's parse.
func (s *Instances) installMux(inst *tabnas.Tabnas) {
	inst.Sub(func(tkn *tabnas.Token, rule *tabnas.Rule, ctx *tabnas.Context) {
		defer func() { _ = recover() }()
		c := s.active
		if nil == c || nil == tkn || tkn.SI < 0 {
			return
		}
		c.lexEvents = append(c.lexEvents, tokenPoint{
			Name: tkn.Name, SI: tkn.SI, RI: tkn.RI, CI: tkn.CI, Src: tkn.Src,
		})
	}, nil)
	inst.SubRuleDone(func(rule *tabnas.Rule, ctx *tabnas.Context, done tabnas.RuleDone) {
		defer func() { _ = recover() }()
		c := s.active
		if nil == c || nil == rule {
			return
		}
		ev := ruleEvent{
			I: rule.I, Name: rule.Name,
			State: string(done.State), Forced: done.Forced,
		}
		if 0 < rule.ON && nil != rule.O[0] {
			t := rule.O[0]
			ev.O0 = &tokenPoint{Name: t.Name, SI: t.SI, RI: t.RI, CI: t.CI, Src: t.Src}
		}
		if 0 < rule.CN && nil != rule.C[0] {
			t := rule.C[0]
			ev.C0 = &tokenPoint{Name: t.Name, SI: t.SI, RI: t.RI, CI: t.CI, Src: t.Src}
		}
		c.ruleEvents = append(c.ruleEvents, ev)
	})
}

// Parse runs one parse with the collector active. parseMu serializes
// parses across all instances, so the subscribers — which run
// synchronously inside the parsing goroutine — always see the one
// active collector. Continuations' internal parses (routed through
// Complete) hold the same lock with a nil collector, making the mux a
// no-op for them.
func (s *Instances) Parse(inst *tabnas.Tabnas, src string, c *collector) (any, []*tabnas.TabnasError, error) {
	s.parseMu.Lock()
	defer s.parseMu.Unlock()
	prev := s.active
	s.active = c
	defer func() { s.active = prev }()
	return inst.ParseRecover(src)
}

// WithParseLock runs fn with the parse lock held and no collector
// active — the engine-call guard for non-analyze parses such as
// Continuations.
func (s *Instances) WithParseLock(fn func()) {
	s.parseMu.Lock()
	defer s.parseMu.Unlock()
	prev := s.active
	s.active = nil
	defer func() { s.active = prev }()
	fn()
}

// Diagnostic is an LSP diagnostic.
type Diagnostic struct {
	Range           Range            `json:"range"`
	Severity        int              `json:"severity"`
	Code            string           `json:"code,omitempty"`
	Source          string           `json:"source"`
	Message         string           `json:"message"`
	CodeDescription *codeDescription `json:"codeDescription,omitempty"`
}

type codeDescription struct {
	HRef string `json:"href"`
}

// engineDiag is the slice of the engine's structured diagnostic this
// pipeline consumes (TabnasError.MarshalJSON — the same shape TS
// serializes, which is what keeps the two ports honest).
type engineDiag struct {
	Code    string `json:"code"`
	Message string `json:"message"`
	Hint    string `json:"hint"`
	Row     int    `json:"row"`
	Col     int    `json:"col"`
	Len     int    `json:"len"`
}

// Analysis is everything one parse yields.
type Analysis struct {
	Value          any
	Errors         []*tabnas.TabnasError
	Failed         bool
	Diagnostics    []Diagnostic
	SemanticTokens *SemanticTokens
	Outline        []*DocumentSymbol
}

// Analyze runs one parse and derives every artifact (ts/src/core.js
// analyze()).
func Analyze(instances *Instances, inst *tabnas.Tabnas, entry *Entry, doc *Doc) *Analysis {
	c := &collector{}
	value, errs, err := instances.Parse(inst, doc.Text, c)

	a := &Analysis{Value: value, Errors: errs}
	if err != nil {
		a.Failed = true
		if te, ok := err.(*tabnas.TabnasError); ok {
			// Fail-fast error (recovery gave up or was off): ctx.Errs
			// already records it; make sure it reaches diagnostics even
			// when errs came back empty.
			if 0 == len(a.Errors) {
				a.Errors = []*tabnas.TabnasError{te}
			}
		}
	}

	a.Diagnostics = Diagnostics(a.Errors, entry, doc)
	if nil == entry || "clean" == entry.LexStream {
		a.SemanticTokens = SemanticTokensOf(c.lexEvents, entry, doc)
	}
	a.Outline = Outline(c.ruleEvents, entry, doc)
	return a
}

// Diagnostics converts engine errors to LSP diagnostics through the
// structured-diagnostic JSON — the exact bytes TS consumes — so the
// two ports cannot disagree about codes, rows, or the `len` unit.
func Diagnostics(errs []*tabnas.TabnasError, entry *Entry, doc *Doc) []Diagnostic {
	out := []Diagnostic{}
	for _, e := range errs {
		if nil == e {
			continue
		}
		b, err := json.Marshal(e)
		if err != nil {
			continue
		}
		var d engineDiag
		if err := json.Unmarshal(b, &d); err != nil {
			continue
		}
		source := "tabnas"
		if nil != entry {
			source += ":" + entry.LanguageID
		}
		msg := d.Message
		if "" != d.Hint {
			msg += "\n\n" + d.Hint
		}
		diag := Diagnostic{
			Range:    doc.RangeFrom(d.Row, d.Col, d.Len),
			Severity: 1,
			Code:     d.Code,
			Source:   source,
			Message:  msg,
		}
		if "" != d.Code && "unknown" != d.Code {
			diag.CodeDescription = &codeDescription{
				HRef: "https://tabnas.dev/errors/" + d.Code,
			}
		}
		out = append(out, diag)
	}
	return out
}

func srcLenBytes(src string) int {
	n := len(src)
	if n < 1 {
		return 1
	}
	return n
}

func srcLenUTF16(src string) int {
	n := UTF16Len(src)
	if n < 1 {
		return 1
	}
	return n
}

func srcLenRunes(src string) int {
	n := utf8.RuneCountInString(src)
	if n < 1 {
		return 1
	}
	return n
}
