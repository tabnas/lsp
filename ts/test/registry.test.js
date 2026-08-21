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

  it('a Windows folder path matches its documents', () => {
    // The two sides of the containment test come from different
    // converters: fsPathOf() always yields forward slashes, while the
    // folder side comes from url.fileURLToPath, which yields
    // BACKSLASHES on win32. Unnormalised, they never matched and
    // folder-scoped routing was dead on Windows.
    const winFolder = 'c:\\ws\\alpha'
    const reg = new Registry([], [ws('mydsl', winFolder)])
    const r = reg.resolve('mydsl', 'file:///c%3A/ws/alpha/x.mydsl')
    assert.ok(r.entry, 'windows folder failed to match its own document')
    assert.equal(r.entry.languageId, 'mydsl')
    // Still not a sibling-prefix match.
    assert.equal(reg.resolve('mydsl', 'file:///c%3A/ws/alphabet/x.mydsl').entry, null)
  })

  it('a POSIX folder whose name contains a backslash routes correctly', () => {
    // On POSIX a backslash is an ordinary filename character, not a
    // separator. contains() used to rewrite every backslash to a slash
    // unconditionally — which turned the root `/work/a\\b` into
    // `/work/a/b`, so the folder stopped containing its OWN documents
    // and started claiming the unrelated `/work/a/b` tree instead.
    const odd = ws('mydsl', '/work/a\\b')
    const reg = new Registry([], [odd])

    assert.ok(
      reg.resolve('mydsl', 'file:///work/a%5Cb/x.mydsl').entry,
      'backslash folder failed to match its own document')
    assert.equal(
      reg.resolve('mydsl', 'file:///work/a/b/x.mydsl').entry, null,
      'backslash folder wrongly claimed the /work/a/b tree')
  })

  it('an unscoped entry routes everywhere, including non-file documents', () => {
    // _scope null means session-wide (client-supplied languages);
    // _dir is only the sandbox base for relative grammar paths.
    const anywhere = Object.assign(ws('mydsl', '/ws/alpha'), { _scope: null })
    const reg = new Registry(BUNDLED, [anywhere])
    assert.equal(reg.resolve('mydsl', 'file:///ws/beta/x.mydsl').entry.languageId, 'mydsl')
    assert.equal(reg.resolve('mydsl', 'untitled:Untitled-1').entry.languageId, 'mydsl')
  })

  it('a folder-scoped entry beats an unscoped one inside its folder', () => {
    const scoped = ws('mydsl', '/ws/alpha')
    const global_ = Object.assign(ws('mydsl', '/ws/alpha'), { _scope: null, name: 'global' })
    const reg = new Registry([], [global_, scoped])
    assert.equal(reg.resolve('mydsl', 'file:///ws/alpha/x.mydsl').entry.name, 'mydsl')
    assert.equal(reg.resolve('mydsl', 'file:///elsewhere/x.mydsl').entry.name, 'global')
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
