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

  it('multiline tokens split into line-local spans', () => {
    // A block comment spanning lines must not emit one token whose
    // length crosses the line break: multiline semantic tokens are an
    // optional client capability, and unsplit ones mis-highlight.
    //
    // Built on the shared pure-data grammar (comments ON — the strict
    // JSON plugin fixture disallows them), the same grammar the Go
    // mirror of this test uses.
    const fs = require('fs')
    const spec = JSON.parse(fs.readFileSync(
      path.join(__dirname, '..', '..', 'test', 'fixtures', 'json-grammar.json'),
      'utf8'))
    const entry = normalize({ name: 'specf', languageId: 'specf', grammarKind: 'data' })
    const instances = new SerialInstances(() => {
      const tn = new Tabnas({ parse: { recover: { enabled: true } } })
      tn.grammar(spec)
      entry._inst = tn
      return tn
    })
    const inst = instances.get(entry)
    const doc = new Doc('file:///t.specf', 'specf', 1, '{"a":1,/*x\ny*/"b":2}')
    const a = core.analyze(instances, inst, entry, doc)
    const lines = doc.text.split('\n')
    let line = 0
    let char = 0
    let sawContinuation = false
    const data = a.semanticTokens.data
    for (let i = 0; i < data.length; i += 5) {
      if (0 < data[i]) {
        line += data[i]
        char = data[i + 1]
      } else {
        char += data[i + 1]
      }
      const len = data[i + 2]
      assert.ok(char + len <= lines[line].length,
        'token crosses its line: line ' + line + ' char ' + char + ' len ' + len)
      if (1 === line && 0 === char) sawContinuation = true
    }
    assert.ok(sawContinuation, 'no continuation span on the second line')
  })

  it('the exported Instances class delivers collector events', () => {
    // The one instance-management class routes the permanent mux
    // through the active collector; a dead variant whose mux never
    // fired once shipped here (review catch on #1) — analyze() with it
    // returned empty tokens and outline while parsing succeeded.
    const { Instances } = require('../src/instances')
    const entry = normalize({
      name: '@tabnas/json-fixture', languageId: 'jsonf2',
      grammarKind: 'data', lexStream: 'clean',
    })
    const instances = new Instances(() => {
      const tn = new Tabnas({
        plugins: [json],
        parse: { recover: { enabled: true } },
      })
      entry._inst = tn
      return tn
    })
    const inst = instances.get(entry)
    const doc = new Doc('file:///t.jsonf', 'jsonf2', 1, '{"a":[1,2]}')
    const a = core.analyze(instances, inst, entry, doc)
    assert.ok(0 < a.semanticTokens.data.length, 'no lex events reached the collector')
    assert.equal(a.outline.length, 1, 'no ruleDone events reached the collector')
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

  it('the keyword convention takes the whole name #KW', () => {
    // alchemy's lexer names its `:name` keywords #KW: no CANON name, and
    // not the KW_ prefix, so they went uncoloured.
    assert.equal(core.tokenType('#KW'), 'keyword')
    assert.equal(core.tokenType('KW_if'), 'keyword')
    assert.equal(core.tokenType('#KWX'), null)
    assert.equal(core.tokenType('#KW_'), null)
    // An entry's override still wins.
    assert.equal(core.tokenType('#KW', { '#KW': 'property' }), 'property')
  })

  it('a token reported at the end of its text is put back on it', () => {
    // @tabnas/toml builds each string token from the cursor after the
    // string, so the token that starts there shadowed it and no string
    // was coloured. TypeScript units: UTF-16, so the emoji counts two.
    const text = 'k = "é😀"\n'
    const events = [
      { name: '#ID', sI: 0, rI: 1, cI: 1, len: 1, src: 'k' },
      { name: '#SP', sI: 1, rI: 1, cI: 2, len: 1, src: ' ' },
      { name: '#CL', sI: 2, rI: 1, cI: 3, len: 1, src: '=' },
      { name: '#SP', sI: 3, rI: 1, cI: 4, len: 1, src: ' ' },
      { name: '#ST', sI: 9, rI: 1, cI: 10, len: 5, src: '"é😀"' },
      { name: '#LN', sI: 9, rI: 1, cI: 10, len: 1, src: '\n' },
      { name: '#ZZ', sI: 10, rI: 2, cI: 1, len: 0, src: '' },
    ]
    const kept = core.reconcile(events, text)
    const st = kept.find((t) => '#ST' === t.name)
    assert.ok(st, 'the string survives reconciliation')
    assert.deepStrictEqual([st.sI, st.rI, st.cI, st.len], [4, 1, 5, 5])
    const doc = new Doc('file:///t.toml', 'toml', 1, text)
    const { data } = core.semanticTokens(events, { semanticTokens: {} }, doc)
    assert.deepStrictEqual(data, [
      0, 0, 1, 5, 0, // k
      0, 2, 1, 4, 0, // =
      0, 2, 5, 0, 0, // the string, five UTF-16 units at character 4
    ])
  })

  it('a token spanning lines is put back where it starts', () => {
    // The row moves back by the line feeds the token spans; the column
    // is measured from the text, since the end column is the last line's.
    const text = 'x = 1\nc = """\nz"""\n'
    const events = [
      { name: '#ID', sI: 6, rI: 2, cI: 1, len: 1, src: 'c' },
      { name: '#CL', sI: 8, rI: 2, cI: 3, len: 1, src: '=' },
      { name: '#ST', sI: 18, rI: 3, cI: 5, len: 8, src: '"""\nz"""' },
      { name: '#LN', sI: 18, rI: 3, cI: 5, len: 1, src: '\n' },
    ]
    const st = core.reconcile(events, text).find((t) => '#ST' === t.name)
    assert.deepStrictEqual([st.sI, st.rI, st.cI], [10, 2, 5])
    const doc = new Doc('file:///t.toml', 'toml', 1, text)
    const { data } = core.semanticTokens(events, { semanticTokens: {} }, doc)
    assert.deepStrictEqual(data, [
      1, 0, 1, 5, 0, // c
      0, 2, 1, 4, 0, // =
      0, 2, 3, 0, 0, // """
      1, 0, 4, 0, 0, // z"""
    ])
  })

  it('a token whose text is where it says, or nowhere near, is left alone', () => {
    const text = 'ab "s" cd'
    const at = { name: '#ST', sI: 3, rI: 1, cI: 4, len: 3, src: '"s"' }
    const elsewhere = { name: '#ST', sI: 7, rI: 1, cI: 8, len: 3, src: '"t"' }
    const empty = { name: '#ZZ', sI: 9, rI: 1, cI: 10, len: 0, src: '' }
    const kept = core.reconcile([at, elsewhere, empty], text)
    assert.deepStrictEqual(kept, [at, elsewhere, empty])
    // Without a text there is nothing to measure against.
    const end = { name: '#ST', sI: 6, rI: 1, cI: 7, len: 3, src: '"s"' }
    assert.deepStrictEqual(core.reconcile([end]), [end])
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
