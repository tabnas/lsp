/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Registry generation precedence. The OVERRIDES table in
// tools/gen-registry.js is interim scaffolding for the LSP fields
// fleet descriptors do not carry yet (plan C1). Once a descriptor
// carries one, the descriptor is authoritative — ADR-10's "derive,
// never duplicate". These pin that direction, because getting it
// backwards would make C1's rollout appear to have no effect.

const { describe, it } = require('node:test')
const assert = require('node:assert')

const { entryFor, LSP_FIELDS } = require('../tools/gen-registry')

describe('gen-registry', () => {
  it('lets a descriptor field beat the override table', () => {
    // '@tabnas/chess' carries languageId: 'pgn' in OVERRIDES today.
    const red = []
    const e = entryFor({ name: '@tabnas/chess', languageId: 'chess-fen' }, red)
    assert.equal(e.languageId, 'chess-fen')
    assert.deepStrictEqual(red, ['@tabnas/chess.languageId'])
  })

  it('keeps the override when the descriptor is silent', () => {
    const red = []
    const e = entryFor({ name: '@tabnas/chess' }, red)
    assert.equal(e.languageId, 'pgn')
    assert.deepStrictEqual(red, [], 'nothing to prune yet')
  })

  it('never lets a descriptor enable itself', () => {
    // enabled is editor-collision policy, not a grammar fact: a plugin
    // must not be able to claim a language the host turned off.
    const red = []
    const e = entryFor({ name: '@tabnas/yaml', enabled: true }, red)
    assert.equal(e.enabled, false, 'descriptor overrode host policy')
    assert.ok(!LSP_FIELDS.includes('enabled'))
  })

  it('passes descriptor-only LSP fields straight through', () => {
    const red = []
    const e = entryFor(
      {
        name: '@tabnas/toml',
        extensions: ['.toml'],
        syncGroups: ['end', 'comma', 'close'],
        lexStream: 'clean',
        semanticTokens: { '#KEY': 'property' },
      },
      red,
    )
    assert.deepStrictEqual(e.syncGroups, ['end', 'comma', 'close'])
    assert.equal(e.lexStream, 'clean')
    assert.deepStrictEqual(e.semanticTokens, { '#KEY': 'property' })
    assert.deepStrictEqual(e.extensions, ['.toml'])
    // toml's override is enabled:false and must survive.
    assert.equal(e.enabled, false)
  })
})
