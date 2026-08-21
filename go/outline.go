/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

// Outline from ruleDone events (including forced closes synthesized by
// recovery), mirroring ts/src/core.js: rule-name filter, spans from
// the open pass's first matched token to the close pass's, nested by
// span containment.

import "sort"

// DefaultOutlineRules maps structural rule names to symbol labels.
var DefaultOutlineRules = map[string]string{
	"map":  "Object",
	"list": "Array",
}

// DocumentSymbol is the LSP hierarchical symbol. Children hold
// pointers so nesting never copies a subtree that later grows.
type DocumentSymbol struct {
	Name           string            `json:"name"`
	Kind           int               `json:"kind"`
	Range          Range             `json:"range"`
	SelectionRange Range             `json:"selectionRange"`
	Children       []*DocumentSymbol `json:"children"`

	span [2]int // byte span, for nesting; not serialized
}

const (
	symbolKindArray  = 18
	symbolKindObject = 19
)

// Outline derives nested document symbols from one parse's rule
// events.
func Outline(events []ruleEvent, entry *Entry, doc *Doc) []*DocumentSymbol {
	rules := DefaultOutlineRules
	if nil != entry && nil != entry.OutlineRules {
		rules = map[string]string{}
		for k, v := range DefaultOutlineRules {
			rules[k] = v
		}
		for k, v := range entry.OutlineRules {
			rules[k] = v
		}
	}

	open := map[int]ruleEvent{}
	var symbols []*DocumentSymbol
	for _, e := range events {
		label, structural := rules[e.Name]
		if !structural {
			continue
		}
		if "o" == e.State && nil != e.O0 {
			open[e.I] = e
		} else if "c" == e.State {
			o, ok := open[e.I]
			delete(open, e.I)
			if !ok || (nil == e.C0 && !e.Forced) {
				continue
			}
			endTok := e.C0
			if nil == endTok {
				endTok = o.O0
			}
			start := doc.PosFromEngine(o.O0.RI, o.O0.CI)
			endPos := doc.PosFromEngine(endTok.RI, endTok.CI)
			end := Position{
				Line:      endPos.Line,
				Character: endPos.Character + srcLenUTF16(endTok.Src),
			}
			kind := symbolKindObject
			if "Array" == label {
				kind = symbolKindArray
			}
			symbols = append(symbols, &DocumentSymbol{
				Name:           label,
				Kind:           kind,
				Range:          Range{Start: start, End: end},
				SelectionRange: Range{Start: start, End: start},
				Children:       []*DocumentSymbol{},
				span:           [2]int{o.O0.SI, endTok.SI},
			})
		}
	}

	// Nest by span containment: sort by start ascending, wider first
	// on ties, then a stack walk assigns parents.
	sort.SliceStable(symbols, func(i, j int) bool {
		if symbols[i].span[0] != symbols[j].span[0] {
			return symbols[i].span[0] < symbols[j].span[0]
		}
		return symbols[i].span[1] > symbols[j].span[1]
	})
	roots := []*DocumentSymbol{}
	var stack []*DocumentSymbol
	for _, s := range symbols {
		for 0 < len(stack) {
			top := stack[len(stack)-1]
			if top.span[0] <= s.span[0] && s.span[1] <= top.span[1] {
				break
			}
			stack = stack[:len(stack)-1]
		}
		if 0 < len(stack) {
			top := stack[len(stack)-1]
			top.Children = append(top.Children, s)
		} else {
			roots = append(roots, s)
		}
		stack = append(stack, s)
	}
	return roots
}
