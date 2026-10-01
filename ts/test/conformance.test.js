/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// The cross-runtime conformance suite (design §13): the same fixtures
// go/conformance_test.go and rs/tests/conformance_test.rs run, against
// the shared pure-data grammar. TS is canonical — when this file and a
// port's runner disagree, the port changes.

const { describe, it } = require('node:test')
const assert = require('node:assert')
const fs = require('fs')
const path = require('path')

const { Tabnas } = require('@tabnas/parser')
const { Doc } = require('../src/documents')
const { SerialInstances } = require('../src/instances')
const core = require('../src/core')
const { normalize } = require('../src/registry')

const FIXTURES = path.join(__dirname, '..', '..', 'test', 'fixtures')
const SPEC = JSON.parse(
  fs.readFileSync(path.join(FIXTURES, 'json-grammar.json'), 'utf8'))
const SUITE = JSON.parse(
  fs.readFileSync(path.join(FIXTURES, 'lsp-conformance.json'), 'utf8'))

const ENTRY = normalize({
  name: 'jsonf', languageId: 'jsonf', extensions: ['.jsonf'],
  grammarKind: 'data',
})

const instances = new SerialInstances(() => {
  const tn = new Tabnas({ parse: { recover: { enabled: true } } })
  tn.grammar(SPEC)
  ENTRY._inst = tn
  return tn
})
const inst = instances.get(ENTRY)

function outlineNames(list) {
  return (list || []).map((s) => ({
    name: s.name,
    children: outlineNames(s.children),
  }))
}

// The fixture's `tokens` rows are the `data` decoded through the legend;
// decoding here keeps the two forms of each case honest with each other.
function decodeTokens(data) {
  const rows = []
  let line = 0
  let char = 0
  for (let i = 0; i < data.length; i += 5) {
    line += data[i]
    char = 0 === data[i] ? char + data[i + 1] : data[i + 1]
    rows.push([line, char, data[i + 2], core.LEGEND[data[i + 3]]])
  }
  return rows
}

describe('lsp-conformance', () => {
  for (const c of SUITE.analyze) {
    it('analyze: ' + c.name, () => {
      const doc = new Doc('file:///t.jsonf', 'jsonf', 1, c.input)
      const a = core.analyze(instances, inst, ENTRY, doc)
      assert.deepStrictEqual(a.diagnostics.map((d) => d.code), c.codes)
      if (c.firstRange) {
        assert.deepStrictEqual(a.diagnostics[0].range, c.firstRange)
      }
      assert.deepStrictEqual(outlineNames(a.outline), c.outline)
    })
  }

  for (const c of SUITE.completions) {
    it('completion: ' + c.name, () => {
      const doc = new Doc('file:///t.jsonf', 'jsonf', 1, c.input)
      const items = core.completion(inst, ENTRY, doc, c.position)
      assert.deepStrictEqual(items.map((i) => i.label).sort(), c.labels)
    })
  }

  for (const c of SUITE.outlines) {
    it('outline: ' + c.name, () => {
      const doc = new Doc('file:///t.jsonf', 'jsonf', 1, c.input)
      const a = core.analyze(instances, inst, ENTRY, doc)
      assert.deepStrictEqual(JSON.parse(JSON.stringify(a.outline)), c.symbols)
    })
  }

  for (const c of SUITE.semantic) {
    it('semantic: ' + c.name, () => {
      const entry = Object.assign({}, ENTRY, { semanticTokens: c.overrides || {} })
      const doc = new Doc('file:///t.jsonf', 'jsonf', 1, c.input)
      const a = core.analyze(instances, inst, entry, doc)
      assert.equal(a.errors.length, c.errors)
      assert.deepStrictEqual(a.semanticTokens.data, c.data)
      assert.deepStrictEqual(decodeTokens(c.data), c.tokens)
    })
  }

  // A lex trace recorded from a fleet grammar's own lexer, fed to the
  // pipeline as it is: the reconciliation and the mapping over that
  // grammar's tokens, with no parse and no grammar.
  for (const c of SUITE.traces) {
    it('trace: ' + c.name, () => {
      const events = c.events.map(([name, sI, rI, cI, len, src]) =>
        ({ name, sI, rI, cI, len, src }))
      const entry = Object.assign({}, ENTRY, { semanticTokens: c.overrides || {} })
      const doc = new Doc('file:///t', 'trace', 1, c.input)
      const { data } = core.semanticTokens(events, entry, doc)
      assert.deepStrictEqual(data, c.data)
      assert.deepStrictEqual(decodeTokens(c.data), c.tokens)
    })
  }
})
