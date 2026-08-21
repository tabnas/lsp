/* Copyright (c) 2026 Richard Rodger, MIT License */

// Package lsp is the Go half of the tabnas language server: the same
// protocol-free pipeline as the canonical TypeScript package
// (ts/src/core.js) — diagnostics via ParseRecover, semantic tokens via
// the reconciled lex trace, outline via SubRuleDone, completion via
// Continuations — plus a minimal stdio JSON-RPC front-end (Serve).
//
// TS is canonical; this port mirrors it by fixture parity
// (test/fixtures/, run by both runtimes), never by code sharing. It
// exists so the generator can emit static, single-binary language
// servers for grammars that live in Go — or for any pure-data
// GrammarSpec, which is runtime-independent (design §7).
package lsp

import (
	"fmt"

	tabnas "github.com/tabnas/parser/go"
)

// VERSION is this package's version.
const VERSION = "0.1.1"

// Entry describes one served language: the Go mirror of a registry
// entry (ts/src/registry.js). Generated servers construct these
// statically; there is no bundled fleet registry on the Go side —
// fleet grammars are npm modules, and only pure-data specs or Go
// plugin packages can serve from a Go binary.
type Entry struct {
	Name       string
	LanguageID string
	Extensions []string

	// PluginKind: grammar | compiler | modifier. Modifiers are never
	// routing targets.
	PluginKind string

	// LexStream gates semantic tokens: only "clean" entries (no relex,
	// no rewind) serve them. Default "clean".
	LexStream string

	// SemanticTokens maps engine token names to LSP token types,
	// overriding the CANON defaults (core DefaultTokenTypes).
	SemanticTokens map[string]string

	// OutlineRules maps rule names to symbol labels, overriding the
	// defaults (map -> Object, list -> Array).
	OutlineRules map[string]string

	// SyncGroups REPLACES the engine's recovery sync-group defaults
	// when non-nil (mirrors the TS option's semantics).
	SyncGroups []string

	// Enabled follows the editor-collision policy; disabled entries
	// are not routed.
	Enabled bool
}

// EntryFromSpecJSON builds an Entry plus a grammar loader from a
// serialized GrammarSpec — the L2 lane, and the only lane a Go server
// needs for pure-data grammars. The spec bytes are expected to have
// passed the generation-time firewall (the generator refuses poisoned
// specs); GrammarSpecFromJSON re-applies the engine's own gates
// (schema-version ceiling among them) at load time.
func EntryFromSpecJSON(languageID string, extensions []string, spec []byte) (*Entry, MakeInstance) {
	entry := &Entry{
		Name:       languageID,
		LanguageID: languageID,
		Extensions: extensions,
		PluginKind: "grammar",
		LexStream:  "clean",
		Enabled:    true,
	}
	make_ := func(e *Entry) (*tabnas.Tabnas, error) {
		gs, err := tabnas.GrammarSpecFromJSON(spec)
		if err != nil {
			return nil, fmt.Errorf("grammar for %s: %w", e.LanguageID, err)
		}
		tn := NewInstance(e)
		if err := tn.Grammar(gs); err != nil {
			return nil, fmt.Errorf("grammar for %s: %w", e.LanguageID, err)
		}
		return tn, nil
	}
	return entry, make_
}

// NewInstance builds a bare engine instance configured for LSP use:
// recovery on (multi-error diagnostics), the entry's sync groups
// applied when it declares any.
func NewInstance(e *Entry) *tabnas.Tabnas {
	r := &tabnas.RecoverOptions{Enabled: true}
	if e != nil && nil != e.SyncGroups {
		r.SyncGroups = e.SyncGroups
	}
	return tabnas.Make(tabnas.Options{Parse: &tabnas.ParseOptions{Recover: r}})
}

// MakeInstance builds (or rebuilds, on reload) the engine instance for
// an entry. Instances installs the single mux subscriber on whatever
// this returns — implementations must NOT install their own
// subscribers for the pipeline's events.
type MakeInstance func(*Entry) (*tabnas.Tabnas, error)
