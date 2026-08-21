/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Version-drift gate (fleet convention): the VERSION const, the
// package.json version, and the Go const move together — the release
// orchestrator rewrites them, and this test is what makes a missed
// rewrite fail the release verify instead of shipping silently
// (@tabnas/json once exported a stale const across several releases).

const { describe, it } = require('node:test')
const assert = require('node:assert')
const fs = require('fs')
const path = require('path')

const { VERSION } = require('../src/core')

describe('version', () => {
  it('VERSION equals package.json version', () => {
    const pkg = require('../package.json')
    assert.equal(VERSION, pkg.version)
  })

  it('declares no engine version the fleet has not published', () => {
    // The peer range shipped as ">=0.9.0" while the newest published
    // @tabnas/parser was 0.8.11 — a floor no release satisfied, so
    // `npm install @tabnas/lsp` failed ETARGET for everyone. It went
    // unnoticed because the `file:` devDependency satisfies resolution
    // locally and npm does not enforce a root package's own peers.
    // The fleet convention (admin/publish.sh) is an OPEN range: every
    // @tabnas peer is ">=0" by design, so installs resolve the latest
    // published engine.
    const pkg = require('../package.json')
    assert.equal(pkg.peerDependencies['@tabnas/parser'], '>=0',
      'the @tabnas peer range must stay open — see admin/publish.sh')
  })

  it('VERSION equals the Go const in go/lsp.go', () => {
    const goSrc = fs.readFileSync(
      path.join(__dirname, '..', '..', 'go', 'lsp.go'), 'utf8')
    const m = /^const VERSION = "([0-9.]+)"$/m.exec(goSrc)
    assert.ok(m, 'no VERSION const found in go/lsp.go')
    assert.equal(VERSION, m[1])
  })
})
