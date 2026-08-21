/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

// The protocol front-end: a thin stdio JSON-RPC wiring over the
// pipeline, mirroring ts/src/server.js. Single-threaded by design —
// messages are handled in arrival order, so document state needs no
// locking and results are trivially version-consistent. Analysis runs
// synchronously per change (the TS server debounces; a generated Go
// server serves one language and full reparse is engine-fast — worker
// isolation and budgets are the tracked escalation, design §11).

import (
	"encoding/json"
	"fmt"
	"io"
	"os"
)

// Config configures Serve.
type Config struct {
	// Entries are the served languages.
	Entries []*Entry

	// MakeInstance builds the engine instance for an entry. Generated
	// servers pass the closure from EntryFromSpecJSON or their own
	// plugin-applying loader.
	MakeInstance MakeInstance

	// In/Out default to stdin/stdout.
	In  io.Reader
	Out io.Writer

	// Logf receives server-side log lines; default: stderr.
	Logf func(format string, args ...any)
}

// Server is the running state, exported for tests.
type Server struct {
	cfg       Config
	conn      *rpcConn
	registry  *Registry
	docs      *DocumentStore
	instances *Instances
	analyses  map[string]*versionedAnalysis
	exited    bool
}

type versionedAnalysis struct {
	version  int
	analysis *Analysis
}

// Serve runs the server until the client closes the stream or sends
// exit. It returns nil on an orderly shutdown.
func Serve(cfg Config) error {
	s := NewServer(cfg)
	return s.Run()
}

// NewServer builds a server without starting the loop (tests drive
// Handle directly).
func NewServer(cfg Config) *Server {
	if nil == cfg.In {
		cfg.In = os.Stdin
	}
	if nil == cfg.Out {
		cfg.Out = os.Stdout
	}
	if nil == cfg.Logf {
		cfg.Logf = func(format string, args ...any) {
			fmt.Fprintf(os.Stderr, "tabnas-lsp: "+format+"\n", args...)
		}
	}
	return &Server{
		cfg:       cfg,
		conn:      newRPCConn(cfg.In, cfg.Out),
		registry:  NewRegistry(cfg.Entries),
		docs:      NewDocumentStore(),
		instances: NewInstances(cfg.MakeInstance),
		analyses:  map[string]*versionedAnalysis{},
	}
}

// Run is the message loop.
func (s *Server) Run() error {
	for !s.exited {
		msg, err := s.conn.read()
		if err != nil {
			if io.EOF == err {
				return nil
			}
			return err
		}
		s.Handle(msg)
	}
	return nil
}

// Handle dispatches one message.
func (s *Server) Handle(msg *rpcMessage) {
	switch msg.Method {

	case "initialize":
		s.reply(msg, map[string]any{
			"capabilities": map[string]any{
				"positionEncoding": "utf-16",
				"textDocumentSync": map[string]any{
					"openClose": true,
					"change":    2, // Incremental
				},
				"completionProvider": map[string]any{
					"triggerCharacters": []string{":", ",", "{", "[", "\""},
				},
				"documentSymbolProvider": true,
				"semanticTokensProvider": map[string]any{
					"legend": map[string]any{
						"tokenTypes":     Legend,
						"tokenModifiers": []string{},
					},
					"full": true,
				},
			},
			"serverInfo": map[string]any{"name": "tabnas-lsp-go", "version": VERSION},
		})

	case "initialized":
		// no-op

	case "shutdown":
		s.reply(msg, nil)

	case "exit":
		s.exited = true

	case "textDocument/didOpen":
		var p struct {
			TextDocument struct {
				URI        string `json:"uri"`
				LanguageID string `json:"languageId"`
				Version    int    `json:"version"`
				Text       string `json:"text"`
			} `json:"textDocument"`
		}
		if !s.params(msg, &p) {
			return
		}
		d := p.TextDocument
		s.docs.Open(d.URI, d.LanguageID, d.Version, d.Text)
		s.analyze(d.URI)

	case "textDocument/didChange":
		var p struct {
			TextDocument struct {
				URI     string `json:"uri"`
				Version int    `json:"version"`
			} `json:"textDocument"`
			ContentChanges []struct {
				Range *Range `json:"range"`
				Text  string `json:"text"`
			} `json:"contentChanges"`
		}
		if !s.params(msg, &p) {
			return
		}
		doc := s.docs.Get(p.TextDocument.URI)
		if nil == doc {
			return
		}
		text := doc.Text
		for _, change := range p.ContentChanges {
			if nil == change.Range {
				text = change.Text
			} else {
				start := doc.OffsetAt(change.Range.Start)
				end := doc.OffsetAt(change.Range.End)
				text = text[:start] + change.Text + text[end:]
			}
			// After EVERY change, not only ranged ones. A didChange array
			// may legally mix a full replacement with later ranged edits,
			// and the replacement branch used to leave doc (and its line
			// index) on the PREVIOUS text — so the next ranged edit
			// computed byte offsets against a document that no longer
			// existed. On a shrinking replacement that slices out of
			// range and panics the server.
			doc.Update(text, doc.Version) // keep line index fresh mid-loop
		}
		doc.Update(text, p.TextDocument.Version)
		delete(s.analyses, p.TextDocument.URI)
		s.analyze(p.TextDocument.URI)

	case "textDocument/didClose":
		var p struct {
			TextDocument struct {
				URI string `json:"uri"`
			} `json:"textDocument"`
		}
		if !s.params(msg, &p) {
			return
		}
		s.docs.Close(p.TextDocument.URI)
		delete(s.analyses, p.TextDocument.URI)
		s.conn.notify("textDocument/publishDiagnostics", map[string]any{
			"uri": p.TextDocument.URI, "diagnostics": []Diagnostic{},
		})

	case "textDocument/completion":
		var p struct {
			TextDocument struct {
				URI string `json:"uri"`
			} `json:"textDocument"`
			Position Position `json:"position"`
		}
		if !s.params(msg, &p) {
			return
		}
		doc := s.docs.Get(p.TextDocument.URI)
		if nil == doc {
			s.reply(msg, []CompletionItem{})
			return
		}
		entry := s.entryFor(doc)
		if nil == entry {
			s.reply(msg, []CompletionItem{})
			return
		}
		inst, err := s.instances.Get(entry)
		if err != nil || nil == inst {
			s.reply(msg, []CompletionItem{})
			return
		}
		s.reply(msg, Complete(s.instances, inst, doc, p.Position))

	case "textDocument/documentSymbol":
		var p struct {
			TextDocument struct {
				URI string `json:"uri"`
			} `json:"textDocument"`
		}
		if !s.params(msg, &p) {
			return
		}
		if a := s.currentAnalysis(p.TextDocument.URI); nil != a {
			s.reply(msg, a.Outline)
			return
		}
		s.reply(msg, []*DocumentSymbol{})

	case "textDocument/semanticTokens/full":
		var p struct {
			TextDocument struct {
				URI string `json:"uri"`
			} `json:"textDocument"`
		}
		if !s.params(msg, &p) {
			return
		}
		data := []int{}
		if a := s.currentAnalysis(p.TextDocument.URI); nil != a && nil != a.SemanticTokens {
			data = a.SemanticTokens.Data
		}
		s.reply(msg, map[string]any{"data": data})

	case "tabnas/status":
		languages := []map[string]any{}
		for _, e := range s.registry.All() {
			languages = append(languages, map[string]any{
				"languageId":  e.LanguageID,
				"enabled":     e.Enabled,
				"quarantined": s.instances.Quarantined(e),
				"lexStream":   e.LexStream,
			})
		}
		s.reply(msg, map[string]any{"languages": languages})

	default:
		if nil != msg.ID {
			s.conn.respondError(msg.ID, codeMethodNotFound,
				"method not found: "+msg.Method)
		}
		// unknown notifications are ignored, per the protocol
	}
}

// reply answers a request; a notification (no id) gets nothing.
func (s *Server) reply(msg *rpcMessage, result any) {
	if nil == msg.ID {
		return
	}
	if err := s.conn.respond(msg.ID, result); err != nil {
		s.cfg.Logf("write failed: %v", err)
	}
}

func (s *Server) params(msg *rpcMessage, v any) bool {
	if err := json.Unmarshal(msg.Params, v); err != nil {
		if nil != msg.ID {
			s.conn.respondError(msg.ID, codeInternalError, "bad params: "+err.Error())
		} else {
			s.cfg.Logf("bad params for %s: %v", msg.Method, err)
		}
		return false
	}
	return true
}

func (s *Server) entryFor(doc *Doc) *Entry {
	return s.registry.Resolve(doc.LanguageID, doc.URI).Entry
}

// currentAnalysis serves cached results only at the document's current
// version — stale structural results are suppressed, never served
// against newer content.
func (s *Server) currentAnalysis(uri string) *Analysis {
	doc := s.docs.Get(uri)
	va := s.analyses[uri]
	if nil != doc && nil != va && va.version == doc.Version {
		return va.analysis
	}
	return nil
}

// analyze parses a document and pushes version-stamped diagnostics.
func (s *Server) analyze(uri string) {
	doc := s.docs.Get(uri)
	if nil == doc {
		return
	}
	entry := s.entryFor(doc)
	if nil == entry {
		return
	}
	inst, err := s.instances.Get(entry)
	if err != nil {
		s.cfg.Logf("grammar load failed (%s): %v", entry.LanguageID, err)
		return
	}
	if nil == inst {
		return // quarantined
	}
	a := func() (a *Analysis) {
		defer func() {
			if r := recover(); nil != r {
				s.instances.RecordFailure(entry)
				s.cfg.Logf("analysis panicked (%s): %v", entry.LanguageID, r)
				a = nil
			}
		}()
		return Analyze(s.instances, inst, entry, doc)
	}()
	if nil == a {
		return
	}
	s.conn.notify("textDocument/publishDiagnostics", map[string]any{
		"uri":         uri,
		"version":     doc.Version,
		"diagnostics": a.Diagnostics,
	})
	if !a.Failed {
		s.analyses[uri] = &versionedAnalysis{version: doc.Version, analysis: a}
	}
}
