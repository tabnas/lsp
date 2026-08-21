/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

// End-to-end protocol smoke test: a scripted client session over the
// framed stdio transport — initialize, open a broken document, read
// the pushed diagnostics, ask for symbols and completions, shut down.

import (
	"bytes"
	"fmt"
	"io"
	"strings"
	"testing"
)

func frame(bodies ...string) io.Reader {
	var b strings.Builder
	for _, body := range bodies {
		fmt.Fprintf(&b, "Content-Length: %d\r\n\r\n%s", len(body), body)
	}
	return strings.NewReader(b.String())
}

func TestServerSession(t *testing.T) {
	entry, makeInst := EntryFromSpecJSON("jsonf", []string{".jsonf"}, fixture(t, "json-grammar.json"))

	open := `{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{` +
		`"textDocument":{"uri":"file:///t.jsonf","languageId":"jsonf",` +
		`"version":1,"text":"{\"a\":true blah,\"b\":2}"}}}`

	in := frame(
		`{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}`,
		`{"jsonrpc":"2.0","method":"initialized","params":{}}`,
		open,
		`{"jsonrpc":"2.0","id":2,"method":"textDocument/documentSymbol","params":{"textDocument":{"uri":"file:///t.jsonf"}}}`,
		`{"jsonrpc":"2.0","id":3,"method":"textDocument/completion","params":{"textDocument":{"uri":"file:///t.jsonf"},"position":{"line":0,"character":21}}}`,
		`{"jsonrpc":"2.0","id":4,"method":"tabnas/status","params":null}`,
		`{"jsonrpc":"2.0","id":5,"method":"shutdown","params":null}`,
		`{"jsonrpc":"2.0","method":"exit"}`,
	)
	var out bytes.Buffer

	err := Serve(Config{
		Entries:      []*Entry{entry},
		MakeInstance: makeInst,
		In:           in,
		Out:          &out,
		Logf:         func(string, ...any) {},
	})
	if err != nil {
		t.Fatalf("serve: %v", err)
	}

	raw := out.String()

	// The initialize response advertises the pipeline's capabilities.
	if !strings.Contains(raw, `"documentSymbolProvider":true`) {
		t.Fatal("initialize response missing documentSymbolProvider")
	}
	if !strings.Contains(raw, `"positionEncoding":"utf-16"`) {
		t.Fatal("initialize response missing positionEncoding")
	}

	// Version-stamped diagnostics were pushed for the broken document.
	if !strings.Contains(raw, `"method":"textDocument/publishDiagnostics"`) {
		t.Fatal("no publishDiagnostics notification")
	}
	if !strings.Contains(raw, `"code":"unexpected"`) {
		t.Fatal("diagnostics missing the unexpected code")
	}
	if !strings.Contains(raw, `"version":1`) {
		t.Fatal("diagnostics not version-stamped")
	}

	// The broken document still has structure: an Object symbol.
	if !strings.Contains(raw, `"name":"Object"`) {
		t.Fatal("documentSymbol response missing the Object symbol")
	}

	// Status lists the language.
	if !strings.Contains(raw, `"languageId":"jsonf"`) {
		t.Fatal("tabnas/status missing the language")
	}
}

func TestServerUnknownMethod(t *testing.T) {
	entry, makeInst := EntryFromSpecJSON("jsonf", []string{".jsonf"}, fixture(t, "json-grammar.json"))
	in := frame(
		`{"jsonrpc":"2.0","id":9,"method":"no/such/method","params":{}}`,
		`{"jsonrpc":"2.0","method":"exit"}`,
	)
	var out bytes.Buffer
	if err := Serve(Config{
		Entries: []*Entry{entry}, MakeInstance: makeInst,
		In: in, Out: &out, Logf: func(string, ...any) {},
	}); err != nil {
		t.Fatalf("serve: %v", err)
	}
	if !strings.Contains(out.String(), `"code":-32601`) {
		t.Fatal("unknown request did not get MethodNotFound")
	}
}
