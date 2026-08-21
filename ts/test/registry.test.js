/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Folder-scoped workspace routing (design §3/§6): two roots declaring
// the same languageId keep their own grammars — resolution follows the
// document's folder, not load order — and workspace entries beat the
// global tiers only inside their folder.

const { describe, it } = require('node:test')
const assert = require('node:assert')

const { Registry } = require('../src/registry')

const BUNDLED = [
  { name: '@tabnas/toml', languageId: 'toml', extensions: ['.toml'] },
]

function ws(languageId, dir, extra) {
  return Object.assign({
    name: languageId, languageId,
    extensions: ['.' + languageId],
    _source: 'workspace', _dir: dir,
  }, extra)
}

describe('lsp-registry-multiroot', () => {
  it('same languageId in two folders resolves by document folder', () => {
    const a = ws('mydsl', '/ws/alpha')
    const b = ws('mydsl', '/ws/beta')
    const reg = new Registry(BUNDLED, [a, b])
    assert.equal(
      reg.resolve('mydsl', 'file:///ws/alpha/doc.mydsl').entry._dir, '/ws/alpha')
    assert.equal(
      reg.resolve('mydsl', 'file:///ws/beta/doc.mydsl').entry._dir, '/ws/beta')
    // Sibling-prefix folders do not capture each other's documents.
    assert.equal(
      reg.resolve('mydsl', 'file:///ws/alphabet/doc.mydsl').entry, null)
  })

  it('deepest matching folder wins for nested roots', () => {
    const outer = ws('mydsl', '/ws/app')
    const inner = ws('mydsl', '/ws/app/vendored')
    const reg = new Registry([], [outer, inner])
    assert.equal(
      reg.resolve('mydsl', 'file:///ws/app/vendored/x.mydsl').entry._dir,
      '/ws/app/vendored')
    assert.equal(
      reg.resolve('mydsl', 'file:///ws/app/x.mydsl').entry._dir, '/ws/app')
  })

  it('workspace beats bundled inside its folder, not outside', () => {
    const wsToml = ws('toml', '/ws/alpha', { extensions: ['.toml'] })
    const reg = new Registry(BUNDLED, [wsToml])
    assert.equal(
      reg.resolve('toml', 'file:///ws/alpha/x.toml').entry._source, 'workspace')
    assert.equal(
      reg.resolve('toml', 'file:///elsewhere/x.toml').entry._source, 'bundled')
  })

  it('workspace extension fallback stays folder-scoped', () => {
    const a = ws('mydsl', '/ws/alpha')
    const reg = new Registry([], [a])
    // Generic client id, extension match, inside the folder.
    assert.equal(
      reg.resolve('plaintext', 'file:///ws/alpha/doc.mydsl').entry._dir,
      '/ws/alpha')
    // Same extension outside the folder: no workspace routing.
    assert.equal(
      reg.resolve('plaintext', 'file:///other/doc.mydsl').entry, null)
  })

  it('non-file documents never match workspace entries', () => {
    const a = ws('mydsl', '/ws/alpha')
    const reg = new Registry(BUNDLED, [a])
    assert.equal(reg.resolve('mydsl', 'untitled:Untitled-1').entry, null)
    // Globals still resolve for non-file documents.
    assert.equal(
      reg.resolve('toml', 'untitled:Untitled-1.toml').entry._source, 'bundled')
  })
})
