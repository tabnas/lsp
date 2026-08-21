/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

// Registry routing, mirroring ts/src/registry.js: the client's
// languageId wins only when it names an enabled, non-modifier entry
// (editors send generic ids like "plaintext" for unknown extensions);
// otherwise the most-specific extension match; ties are surfaced, not
// silently picked.

import "strings"

type Registry struct {
	entries []*Entry
	byID    map[string]*Entry
}

func NewRegistry(entries []*Entry) *Registry {
	r := &Registry{byID: map[string]*Entry{}}
	for _, e := range entries {
		if nil == e {
			continue
		}
		if "" == e.LexStream {
			e.LexStream = "clean"
		}
		if "" == e.PluginKind {
			e.PluginKind = "grammar"
		}
		r.entries = append(r.entries, e)
		r.byID[e.LanguageID] = e
	}
	return r
}

func (r *Registry) Get(languageID string) *Entry { return r.byID[languageID] }

func (r *Registry) All() []*Entry { return r.entries }

// Resolution names the entry and how it was reached; Ambiguous lists
// competing languageIds when an extension is claimed more than once.
type Resolution struct {
	Entry     *Entry
	Via       string // "languageId" | "extension" | ""
	Ambiguous []string
}

func (r *Registry) Resolve(languageID, uri string) Resolution {
	if direct := r.byID[languageID]; nil != direct &&
		direct.Enabled && "modifier" != direct.PluginKind {
		return Resolution{Entry: direct, Via: "languageId"}
	}

	ext := ExtOf(uri)
	if "" == ext {
		return Resolution{}
	}
	var best *Entry
	var ambiguous []string
	for _, e := range r.entries {
		if !e.Enabled || "modifier" == e.PluginKind {
			continue
		}
		for _, x := range e.Extensions {
			if strings.ToLower(x) == ext {
				if nil != best && best != e {
					ambiguous = append(ambiguous, e.LanguageID)
				} else {
					best = e
				}
			}
		}
	}
	if nil == best {
		return Resolution{}
	}
	res := Resolution{Entry: best, Via: "extension"}
	if 0 < len(ambiguous) {
		res.Ambiguous = append([]string{best.LanguageID}, ambiguous...)
	}
	return res
}
