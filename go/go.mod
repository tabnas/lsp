module github.com/tabnas/lsp/go

go 1.24.7

// The engine floor is go/v0.9.0, the first TAGGED release carrying the
// LSP engine contract (recovery, ruleDone, continuations — parser#94–#109)
// including the trailing-content and give-up recovery fixes this module's
// conformance fixtures pin. It replaces a pseudo-version of parser main,
// which was the right placeholder only while no tagged release contained
// the contract.
//
// The pseudo-version it replaced is worth remembering, because both halves
// of it were got wrong the first time and the same traps apply to any
// future floor:
//
//   - The COMMIT had to contain the recovery fix. The first attempt named
//     abfed2c ("ci: apply scorecard.yml"), which predates it, and
//     go/conformance_test.go FAILS against that engine.
//   - The BASE had to be v0.8.12-0., not v0.0.0-. Tag go/v0.8.11 is an
//     ancestor, so under minimal version selection a v0.0.0- pseudo-version
//     LOSES to the released v0.8.11 and the pin does nothing.
//
// Neither trap applies to a real tag. Local dev and CI still resolve the
// sibling checkout via `make go-work` rather than a committed replace,
// which would break `go install` consumers.
require github.com/tabnas/parser/go v0.9.0
