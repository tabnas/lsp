/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

// Minimal JSON-RPC 2.0 framing over stdio: Content-Length headers,
// one JSON body per message. Deliberately dependency-free — the whole
// of what an LSP transport needs is below, and a generated Go server
// should build with nothing beyond the engine and this module.

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"strconv"
	"strings"
	"sync"
)

// rpcMessage is an incoming message: request (id+method),
// notification (method), or — unused by this server — a response.
type rpcMessage struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id,omitempty"`
	Method  string          `json:"method,omitempty"`
	Params  json.RawMessage `json:"params,omitempty"`
}

type rpcError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

const (
	codeMethodNotFound = -32601
	codeInternalError  = -32603
)

type rpcConn struct {
	r  *bufio.Reader
	w  io.Writer
	mu sync.Mutex
}

func newRPCConn(r io.Reader, w io.Writer) *rpcConn {
	return &rpcConn{r: bufio.NewReader(r), w: w}
}

// read returns the next message, io.EOF at end of stream.
func (c *rpcConn) read() (*rpcMessage, error) {
	length := -1
	for {
		line, err := c.r.ReadString('\n')
		if err != nil {
			return nil, err
		}
		line = strings.TrimRight(line, "\r\n")
		if "" == line {
			break // end of headers
		}
		name, value, found := strings.Cut(line, ":")
		if !found {
			continue
		}
		if "content-length" == strings.ToLower(strings.TrimSpace(name)) {
			n, err := strconv.Atoi(strings.TrimSpace(value))
			if err != nil {
				return nil, fmt.Errorf("bad Content-Length: %w", err)
			}
			length = n
		}
	}
	if length < 0 {
		return nil, fmt.Errorf("missing Content-Length header")
	}
	body := make([]byte, length)
	if _, err := io.ReadFull(c.r, body); err != nil {
		return nil, err
	}
	var msg rpcMessage
	if err := json.Unmarshal(body, &msg); err != nil {
		return nil, fmt.Errorf("bad JSON-RPC body: %w", err)
	}
	return &msg, nil
}

func (c *rpcConn) writeJSON(v any) error {
	body, err := json.Marshal(v)
	if err != nil {
		return err
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if _, err := fmt.Fprintf(c.w, "Content-Length: %d\r\n\r\n", len(body)); err != nil {
		return err
	}
	_, err = c.w.Write(body)
	return err
}

// respond sends a result for a request id. `result` is always emitted
// — a null result is a valid response, an absent one is not.
func (c *rpcConn) respond(id json.RawMessage, result any) error {
	return c.writeJSON(struct {
		JSONRPC string          `json:"jsonrpc"`
		ID      json.RawMessage `json:"id"`
		Result  any             `json:"result"`
	}{"2.0", id, result})
}

// respondError sends an error for a request id.
func (c *rpcConn) respondError(id json.RawMessage, code int, message string) error {
	return c.writeJSON(struct {
		JSONRPC string          `json:"jsonrpc"`
		ID      json.RawMessage `json:"id"`
		Error   *rpcError       `json:"error"`
	}{"2.0", id, &rpcError{Code: code, Message: message}})
}

// notify sends a server-initiated notification.
func (c *rpcConn) notify(method string, params any) error {
	return c.writeJSON(struct {
		JSONRPC string `json:"jsonrpc"`
		Method  string `json:"method"`
		Params  any    `json:"params"`
	}{"2.0", method, params})
}
