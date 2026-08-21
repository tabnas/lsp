/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Core pipeline tests: one parse yields diagnostics + semantic tokens
// + outline, against the real engine (A1-A6 stack) and the strict-JSON
// fixture grammar. Protocol-free: server.js is wiring over this.

const { describe, it } = require('node:test')
const assert = require('node:assert')
const path = require('path')

const { Tabnas } = require('@tabnas/parser')
const { json } = require(path.join(
  require.resolve('@tabnas/parser').replace(/dist[/\\].*$/, ''),
  'dist-test',
  'json-plugin',
))

const { Doc } = require('../src/documents')
const { SerialInstances } = require('../src/instances')
const core = require('../src/core')
const { Registry, normalize } = require('../src/registry')

const ENTRY = normalize({
  name: '@tabnas/json-fixture',
  languageId: 'jsonf',
  extensions: ['.jsonf'],
  grammarKind: 'data',
  lexStream: 'clean',
})

function makeStack() {
  const instances = new SerialInstances(() => {
    const tn = new Tabnas({
      plugins: [json],
      parse: { recover: { enabled: true } },
    })
    ENTRY._inst = tn
    return tn
  })
  const inst = instances.get(ENTRY)
  return { instances, inst }
}

describe('lsp-core', () => {
  it('clean parse: no diagnostics, tokens and outline present', () => {
    const { instances, inst } = makeStack()
    const doc = new Doc('file:///t.jsonf', 'jsonf', 1, '{"a":[1,2]}')
    const a = core.analyze(instances, inst, ENTRY, doc)
    assert.deepStrictEqual(a.diagnostics, [])
    assert.ok(0 < a.semanticTokens.data.length, 'semantic tokens emitted')
    assert.equal(a.outline.length, 1)
    assert.equal(a.outline[0].name, 'Object')
    assert.equal(a.outline[0].children.length, 1)
    assert.equal(a.outline[0].children[0].name, 'Array')
  })

  it('broken document: diagnostics with ranges, partial value', () => {
    const { instances, inst } = makeStack()
    const doc = new Doc('file:///t.jsonf', 'jsonf', 1, '{"a":true blah,"b":2}')
    const a = core.analyze(instances, inst, ENTRY, doc)
    assert.ok(1 <= a.diagnostics.length)
    const d = a.diagnostics[0]
    assert.equal(d.severity, 1)
    assert.equal(d.range.start.line, 0)
    assert.ok(10 <= d.range.start.character, 'range at the bad word: ' + JSON.stringify(d.range))
    assert.ok(d.codeDescription.href.includes('/errors/'))
    assert.equal(a.value.b, 2)
  })

  it('multi-error document produces multiple diagnostics', () => {
    const { instances, inst } = makeStack()
    const doc = new Doc(
      'file:///t.jsonf', 'jsonf', 1,
      '{"a":true blah,"b":false blah,"c":true blah}',
    )
    const a = core.analyze(instances, inst, ENTRY, doc)
    assert.ok(2 <= a.diagnostics.length, 'got ' + a.diagnostics.length)
  })

  it('semantic tokens use delta encoding over multiple lines', () => {
    const { instances, inst } = makeStack()
    const doc = new Doc('file:///t.jsonf', 'jsonf', 1, '{"a":1,\n"b":2}')
    const a = core.analyze(instances, inst, ENTRY, doc)
    const data = a.semanticTokens.data
    assert.equal(0, data.length % 5)
    // Some token starts a new line (deltaLine 1).
    let sawNewline = false
    for (let i = 0; i < data.length; i += 5) if (1 === data[i]) sawNewline = true
    assert.ok(sawNewline, 'delta encoding crossed the newline')
  })

  it('completion offers the colon after a key', () => {
    const { inst } = makeStack()
    const doc = new Doc('file:///t.jsonf', 'jsonf', 1, '{"a"')
    const items = core.completion(inst, ENTRY, doc, { line: 0, character: 4 })
    assert.ok(items.some((i) => ':' === i.label), JSON.stringify(items))
  })

  it('completion works mid-edit, not just at error points', () => {
    // The engine's continuations primitive now answers for prefixes
    // that parse (parser#101), which is what an editor actually asks
    // about. Before that fix these returned nothing.
    const { inst } = makeStack()
    const afterColon = new Doc('file:///t.jsonf', 'jsonf', 1, '{"a":')
    const items = core.completion(inst, ENTRY, afterColon, { line: 0, character: 5 })
    assert.ok(0 < items.length, 'value starters offered after a colon')

    const afterComma = new Doc('file:///t.jsonf', 'jsonf', 1, '[1,')
    const items2 = core.completion(inst, ENTRY, afterComma, { line: 0, character: 3 })
    assert.ok(
      items2.some((i) => ']' === i.label),
      'closer offered after a separator: ' + JSON.stringify(items2.map((i) => i.label)),
    )
  })

  it('registry routing: languageId gate and extension fallback', () => {
    const reg = new Registry([
      { name: '@tabnas/toml', languageId: 'toml', extensions: ['.toml'] },
      { name: '@tabnas/hoover', languageId: 'hoover', pluginKind: 'modifier' },
      { name: '@tabnas/yaml', languageId: 'yaml', extensions: ['.yaml'], enabled: false },
    ])
    assert.equal(reg.resolve('toml', 'file:///x.toml').entry.languageId, 'toml')
    // Generic client id falls back to extension matching.
    assert.equal(reg.resolve('plaintext', 'file:///x.toml').entry.languageId, 'toml')
    // Disabled entries are not routed even by extension.
    assert.equal(reg.resolve('plaintext', 'file:///x.yaml').entry, null)
    // Modifiers are never routing targets.
    assert.equal(reg.resolve('hoover', 'file:///x.hoover').entry, null)
  })

  it('quarantine disables a repeatedly failing grammar', () => {
    const bad = normalize({ name: '@tabnas/bad', languageId: 'bad' })
    const instances = new SerialInstances(() => {
      throw new Error('boom')
    })
    for (let i = 0; i < 3; i++) {
      try {
        instances.get(bad)
      } catch (e) {
        // expected
      }
    }
    assert.equal(instances.get(bad), null, 'quarantined after repeated failures')
  })

  it('lexStream gate: speculative grammars get no semantic tokens', () => {
    const { instances, inst } = makeStack()
    const spec = Object.assign({}, ENTRY, { lexStream: 'speculative' })
    const doc = new Doc('file:///t.jsonf', 'jsonf', 1, '{"a":1}')
    const a = core.analyze(instances, inst, spec, doc)
    assert.equal(a.semanticTokens, null)
  })

  it('completion drops engine sentinels', () => {
    // The engine names #ZZ (end-of-source) as a legal continuation for
    // every prefix that parses — its way of saying the document is
    // already valid. It is not something a user types, so it must not
    // reach the item list; a complete document offers nothing at all
    // rather than an item labelled "#ZZ".
    const { inst } = makeStack()
    const done = new Doc('file:///t.jsonf', 'jsonf', 1, '{"a":1}')
    const items = core.completion(inst, ENTRY, done, { line: 0, character: 7 })
    assert.deepStrictEqual(items, [])

    // A mid-edit prefix keeps its real completions; only the sentinel
    // is removed.
    const mid = new Doc('file:///t.jsonf', 'jsonf', 1, '[1,')
    const items2 = core.completion(inst, ENTRY, mid, { line: 0, character: 3 })
    assert.ok(0 < items2.length, 'real completions survive')
    for (const i of items2) {
      assert.ok('#ZZ' !== i.detail, 'sentinel leaked: ' + JSON.stringify(items2))
    }
  })
})
