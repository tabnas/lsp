module github.com/tabnas/lsp/go

go 1.24.7

// The engine floor is a pseudo-version of the parser main commit that
// carries the LSP engine contract (recovery, ruleDone, continuations —
// parser#94–#109) INCLUDING the trailing-content recovery fix the
// conformance fixtures pin; no tagged go/v* release contains it yet.
// The fleet precedent (jsonl, zon, json5, jsonc) is to pin such floors
// as pseudo-versions and let Renovate lift them when the release wave
// tags the engine. Local dev and CI resolve the sibling checkout via
// `make go-work` instead of a committed replace, which would break
// `go install` consumers.
//
// Two things about this line are load-bearing, and the first version of
// it got both wrong:
//
//   - The COMMIT must contain the recovery fix. It first named
//     abfed2c ("ci: apply scorecard.yml"), which predates it, and
//     go/conformance_test.go FAILS against that engine
//     ("analyze: astral character before the error": codes = [], want
//     ["unexpected"]). A floor that cannot pass this module's own
//     tests is not a floor.
//   - The BASE must be v0.8.12-0., not v0.0.0-. Tag go/v0.8.11 is an
//     ancestor of this commit, so under minimal version selection a
//     v0.0.0- pseudo-version LOSES to the released v0.8.11: any
//     consumer that also requires the engine silently resolves to
//     v0.8.11 and the pin does nothing.
require github.com/tabnas/parser/go v0.8.12-0.20260821143505-2bdaeb8b1c3b
