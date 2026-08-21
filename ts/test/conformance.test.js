/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// The cross-runtime conformance suite (design §13): the same fixtures
// go/conformance_test.go runs, against the shared pure-data grammar.
// TS is canonical — when this file and the Go runner disagree, the Go
// port changes.

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
})
