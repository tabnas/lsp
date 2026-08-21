/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Plugin resolution: which export of a grammar module is the plugin.
// Export SHAPE does not answer that — @tabnas/jsonic sets
// module.exports to its root instance wrapper and exports the real
// plugin as the lowercase `jsonic`. Applying the wrapper is silently
// harmless (it is a parse function, and parse returns non-string input
// unchanged), so the wrong pick produced a loaded entry serving an
// EMPTY grammar rather than an error. These pin the loader on picking
// the export that actually installs rules.

const { describe, it } = require('node:test')
const assert = require('node:assert')

const { makeLoader } = require('../src/server')

// A jsonic-shaped module: callable itself (the wrapper), with the real
// plugin under the lowercase name and the wrapper under CamelCase.
function jsonicShaped() {
  const realPlugin = function fake(tn) {
    tn.rule('faked', (rs) => rs.open([{ s: [] }]))
    return tn
  }
  const wrapper = function Fake(src) {
    return src // a parse function: non-string input passes through
  }
  wrapper.Fake = wrapper
  wrapper.fake = realPlugin
  return wrapper
}

function loaderFor(mod) {
  return makeLoader((name) => {
    if ('@tabnas/parser' === name) return require('@tabnas/parser')
    return mod
  })
}

describe('lsp-loader', () => {
  it('picks the export that installs a grammar, not the wrapper', () => {
    const makeInstance = loaderFor(jsonicShaped())
    const tn = makeInstance({ name: 'fake', languageId: 'fake' })
    assert.ok(
      Object.keys(tn.rule()).includes('faked'),
      'wrapper was applied instead of the plugin: ' +
        JSON.stringify(Object.keys(tn.rule())),
    )
  })

  it('still accepts a plain single-export plugin module', () => {
    const plain = function plain(tn) {
      tn.rule('plained', (rs) => rs.open([{ s: [] }]))
      return tn
    }
    const makeInstance = loaderFor(plain)
    const tn = makeInstance({ name: 'plain', languageId: 'plain' })
    assert.ok(Object.keys(tn.rule()).includes('plained'))
  })

  it('reports a module with no callable export', () => {
    const makeInstance = loaderFor({ notAFunction: 1 })
    assert.throws(
      () => makeInstance({ name: 'nope', languageId: 'nope' }),
      /plugin is not a function/,
    )
  })
})

describe('lsp-loader-idempotence', () => {
  // The probe must not leave a partial application behind, and no
  // candidate may be applied twice. A modifier-shaped module — several
  // callable exports, none of which install rules — is the case that
  // exposes both: a naive "apply each until one sticks, else apply the
  // first again" loop applies every candidate AND repeats the first.
  it('applies a modifier-shaped module exactly once', () => {
    let applied = 0
    const modifier = function modifier(tn) {
      applied++
      return tn
    }
    const mod = { Modifier: modifier, modifier: modifier, extra: () => {} }
    const makeInstance = makeLoader((name) => {
      if ('@tabnas/parser' === name) return require('@tabnas/parser')
      return mod
    })
    makeInstance({ name: 'modifier', languageId: 'modifier' })
    assert.equal(applied, 1, 'modifier applied ' + applied + ' times')
  })

  it('does not apply a losing candidate to the real instance', () => {
    // The wrapper must leave no trace: only the winning plugin's rule
    // lands, and the wrapper's own side effect never fires on the
    // instance that is returned.
    let wrapperCalls = 0
    const wrapper = function Fake(src) {
      wrapperCalls++
      return src
    }
    const plugin = function fake(tn) {
      tn.rule('winner', (rs) => rs.open([{ s: [] }]))
      return tn
    }
    wrapper.Fake = wrapper
    wrapper.fake = plugin
    const makeInstance = makeLoader((name) => {
      if ('@tabnas/parser' === name) return require('@tabnas/parser')
      return wrapper
    })
    const tn = makeInstance({ name: 'fake', languageId: 'fake' })
    assert.ok(Object.keys(tn.rule()).includes('winner'))
    // The wrapper is only ever called against the throwaway probe.
    assert.ok(wrapperCalls <= 1, 'wrapper hit the real instance')
  })
})
