module github.com/tabnas/lsp/go

go 1.24

// The engine floor is a pseudo-version of the parser main commit that
// carries the LSP engine contract (recovery, ruleDone, continuations —
// parser#94–#109); no tagged go/v* release contains it yet. The fleet
// precedent (jsonl, zon, json5, jsonc) is to pin such floors as
// pseudo-versions and let Renovate lift them when the release wave
// tags the engine. Local dev and CI resolve the sibling checkout via
// `make go-work` instead of a committed replace, which would break
// `go install` consumers.
require github.com/tabnas/parser/go v0.0.0-20260821020040-abfed2c11e8b
