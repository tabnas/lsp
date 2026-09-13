# Build and test both halves of the language server: the TypeScript
# package (ts/, canonical) and the Go module (go/, mirrors it by
# fixture parity). The TS side ships plain CommonJS — there is no
# build step; `build` exists for fleet-uniform target names.
#
# Local test resolves the engine from the sibling checkout: ts/ via
# the file:../../parser/ts devDependency, go/ via a go.work created by
# `make go-work` (not committed; .gitignore'd).

.PHONY: all build test clean build-ts build-go test-ts test-go \
        clean-ts clean-go go-work gen-registry gen-fixtures publish-ts

all: build test

build: build-ts build-go

test: test-ts test-go

clean: clean-ts clean-go

# --- TypeScript (package in ts/) ---
build-ts:
	cd ts && npm install

test-ts:
	cd ts && npm test

clean-ts:
	rm -rf ts/node_modules

# Publish the TypeScript package at its current package.json version.
# Releases normally run in CI over OIDC trusted publishing (fleet
# convention); this target is the escape hatch.
publish-ts: test-ts
	cd ts && npm publish --access public

# --- Go (module in go/) ---
# The go/ module requires github.com/tabnas/parser/go at a pinned
# pseudo-version the proxy serves, so it builds on its own. A sibling
# parser checkout is used INSTEAD when one is present, which is the
# fleet dev loop: engine changes are picked up without a release.
#
# GOWORK is always set explicitly, and that is load-bearing. Go searches
# UPWARD for a go.work, so in a fleet checkout — where the maintainer
# keeps a repo-set go.work at the root — a bare `go build` here resolved
# against that file, which does not list this newer module, and failed
# with "directory prefix . does not contain modules listed in go.work".
# For the same reason `go work init` cannot be used to create ours: it
# refuses outright while ANY parent go.work exists ("go: <root>/go.work
# already exists"), so this target passes GOWORK to the init too.
#
# go/go.work is generated, never committed (.gitignore).
PARSER_GO := $(abspath $(CURDIR)/../parser/go)
GO_WORK   := $(CURDIR)/go/go.work

go-work:
ifneq ($(wildcard $(PARSER_GO)/go.mod),)
	@test -f $(GO_WORK) || \
	  ( cd go && GOWORK=$(GO_WORK) go work init . $(PARSER_GO) )
	@echo "go: using sibling engine at $(PARSER_GO)"
else
	@rm -f $(GO_WORK)
	@echo "go: no sibling parser checkout — using the pinned engine from go.mod"
endif

# GOWORK=off when there is no sibling: it stops an unrelated parent
# go.work from being picked up, and the committed go.sum makes the
# pinned build reproducible.
GOW = $(if $(wildcard $(PARSER_GO)/go.mod),GOWORK=$(GO_WORK),GOWORK=off)

build-go: go-work
	cd go && $(GOW) go build ./...

test-go: go-work
	cd go && $(GOW) go vet ./... && $(GOW) go test ./...

clean-go:
	cd go && GOWORK=off go clean && rm -f go.work go.work.sum

# --- Generated data (ts/data/*.json) ---
# Both generators walk the fleet checkout (sibling repos of this one)
# and refuse to write from a partial checkout rather than clobbering
# good data. Run from a full fleet checkout only.
gen-registry:
	cd ts && npm run gen-registry

gen-fixtures:
	cd ts && npm run gen-fixtures

# --- Generated editor plugins (editors/) ---
# The repo's own multi-language editor plugins are tabnas-lsp-gen
# --unified output; ts/test/geneditors.test.js gates staleness.
gen-editors:
	cd ts && node bin/tabnas-lsp-gen.js --unified --out ../editors

# The prose gate (see docs/STYLE-GUIDE.md). Vale over the reader-facing
# pages, at the levels set in .vale.ini, on the same file list
# ts/test/docs.test.js reads. Requires `vale` on PATH and one
# `vale sync`. Warnings are advisory, errors fail.
prose:
	vale --minAlertLevel=error $$(node ts/scripts/gated-docs.cjs)
	node ts/scripts/vale-counts.cjs

# Re-measure what .vale.ini and the style guide record, after
# a change to the pages or to the rules moves the numbers.
prose-counts:
	node ts/scripts/vale-counts.cjs --write
