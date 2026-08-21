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
# The go/ module requires github.com/tabnas/parser/go at a floor
# version; local dev and CI point it at the sibling checkout with a
# go.work instead of a committed replace (a committed replace would
# break `go install` consumers).
go-work:
	cd go && go work init . ../../parser/go 2>/dev/null || true

build-go: go-work
	cd go && go build ./...

test-go: go-work
	cd go && go test ./...

clean-go:
	cd go && go clean && rm -f go.work go.work.sum

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
