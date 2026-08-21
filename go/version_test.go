/* Copyright (c) 2026 Richard Rodger, MIT License */

package lsp

// Version-drift gate, the Go half of ts/test/version.test.js: the
// VERSION const must equal the TS package version — the release
// orchestrator rewrites both, and this is what makes a missed rewrite
// fail instead of shipping silently.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestVersionMatchesTSPackage(t *testing.T) {
	b, err := os.ReadFile(filepath.Join("..", "ts", "package.json"))
	if err != nil {
		t.Fatalf("ts/package.json: %v", err)
	}
	var pkg struct {
		Version string `json:"version"`
	}
	if err := json.Unmarshal(b, &pkg); err != nil {
		t.Fatalf("ts/package.json: %v", err)
	}
	if VERSION != pkg.Version {
		t.Fatalf("go VERSION %q != ts package.json version %q", VERSION, pkg.Version)
	}
}
