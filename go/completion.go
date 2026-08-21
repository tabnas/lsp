/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

// Completion via the engine's continuation primitive, mirroring
// ts/src/core.js: sentinels filtered, fixed-token source as label.

import tabnas "github.com/tabnas/parser/go"

// Tokens the engine may name as legal continuations that a user can
// never type. #ZZ is end-of-source — the engine returns it whenever
// the prefix parses (its way of saying "this document is already
// valid"), so it reaches completion on nearly every keystroke in a
// permissive grammar. #AA is the match-any sentinel, #BD the bad-token
// marker.
var sentinelTokens = map[string]bool{
	"#ZZ": true,
	"#AA": true,
	"#BD": true,
}

// CompletionItem is the LSP completion item.
type CompletionItem struct {
	Label      string `json:"label"`
	Kind       int    `json:"kind"`
	Detail     string `json:"detail,omitempty"`
	InsertText string `json:"insertText,omitempty"`
}

const (
	completionKindKeyword  = 14
	completionKindOperator = 24
)

// Complete answers a completion request at a position: continuations
// of the document prefix, labeled by fixed-token source where the
// token has one. Runs under the parse lock with no collector active —
// Continuations parses internally, and those events must not leak
// into any analysis.
func Complete(instances *Instances, inst *tabnas.Tabnas, doc *Doc, pos Position) []CompletionItem {
	prefix := doc.Text[:doc.OffsetAt(pos)]
	items := []CompletionItem{}
	var tins []tabnas.Tin
	var names []string
	run := func() {
		defer func() { _ = recover() }()
		tins, names = inst.Continuations(prefix)
	}
	if nil != instances {
		instances.WithParseLock(run)
	} else {
		run()
	}
	for i, name := range names {
		if sentinelTokens[name] {
			continue
		}
		fixed := ""
		if i < len(tins) {
			fixed = inst.FixedTin(tins[i])
		}
		item := CompletionItem{Label: name, Kind: completionKindKeyword, Detail: name}
		if "" != fixed {
			item.Label = fixed
			item.Kind = completionKindOperator
			item.InsertText = fixed
		}
		items = append(items, item)
	}
	return items
}
