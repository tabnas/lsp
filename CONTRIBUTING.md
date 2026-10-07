# Contributing to lsp

Thanks for your interest in contributing! The organization-wide conventions
in [tabnas/.github](https://github.com/tabnas/.github/blob/main/CONTRIBUTING.md) are
canonical and apply here. This file adds what is specific to
**tabnas/lsp**.

Start with [`AGENTS.md`](AGENTS.md) — it is the working guide to this
repository for humans and agents alike.

## Build & test

This repository is *polyglot*: `ts/`, `go/` and `rs/` hold three parallel
implementations of the same package. **`ts/` is canonical; `go/` and `rs/`
track it** — a behaviour change normally lands in all three, with tests in
all three.

```bash
make build   # ts/ (npm install: it ships plain CommonJS), go/ and rs/
make test    # tests ts/, go/ and rs/

# or per stack:
cd ts && npm install && npm run build && npm test
cd go && go build ./... && go test ./...
cd rs && cargo build --all-targets && cargo test --all-targets
```

The TypeScript and Go sides install published packages, `@tabnas/*` from
the npm registry and `github.com/tabnas/*/go` from the module proxy. A
sibling checkout of `tabnas/parser` still matters here, in three places:

- the generator's end-to-end tests in `ts/` link a built `../parser/ts`
  into the servers they generate, so build it first
  (`cd ../parser/ts && npm i && npm run build`);
- `make go-work` points `go/` at `../parser/go` when that checkout exists,
  and at the release `go.mod` pins otherwise;
- `rs/Cargo.toml` takes the engine, and every dialect compiler and fleet
  grammar crate, as path dependencies, so `rs/` needs all of them (and the
  siblings they name in turn) checked out beside this repository. The
  crates are on crates.io, but the committed manifest stays path-only.

CI (`.github/workflows/ci.yml`, which calls no shared workflow) clones what
each job needs. To work against unreleased TypeScript siblings, run admin's
`scripts/link.sh`, which links them over `ts/node_modules/@tabnas/*`. Never
commit that wiring.

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org/) are required,
for commit messages and PR titles alike. PRs are squash-merged, so a PR's
title is its commit message, and the GitHub Release that each release creates
lists those titles in its generated notes. They do not set the version: a
release is its own version-bump pull request, then a `release.yml` dispatch
(see [`AGENTS.md`](AGENTS.md), "Releasing"). For example:

```
feat: add lax mode for trailing commas
fix: handle CRLF inside block scalars
docs: clarify plugin ordering
```

Use `feat!:` / `fix!:` (or a `BREAKING CHANGE:` footer) for breaking changes.

## Pull requests

1. Open an issue first for anything larger than a small fix.
2. Branch from `main`; keep the PR focused on one change.
3. `make test` must pass for **all three** implementations.
4. PR titles follow Conventional Commits — PRs are squash-merged, so the
   title becomes the commit message.
5. CI must be green before merge.

## Security issues

Never open a public issue for a vulnerability — see [SECURITY.md](SECURITY.md).

## Code of conduct

Participation is covered by the org
[Code of Conduct](https://github.com/tabnas/.github/blob/main/CODE_OF_CONDUCT.md).
