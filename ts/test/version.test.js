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

  it('VERSION equals the Go const in go/lsp.go', () => {
    const goSrc = fs.readFileSync(
      path.join(__dirname, '..', '..', 'go', 'lsp.go'), 'utf8')
    const m = /^const VERSION = "([0-9.]+)"$/m.exec(goSrc)
    assert.ok(m, 'no VERSION const found in go/lsp.go')
    assert.equal(VERSION, m[1])
  })
})
