/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Instance cache and quarantine identity (design §3/§6). Both of these
// are about the CACHE KEY: it has to name the grammar an entry loads,
// not merely the language it serves, because a languageId is not unique
// across a multi-root session by design.

const { describe, it } = require('node:test')
const assert = require('node:assert')

const { SerialInstances } = require('../src/instances')

// A stand-in grammar instance. `made` counts construction so a test can
// tell a cache HIT from a fresh load, and `tag` identifies which entry
// produced it.
function maker(log) {
  return (entry) => {
    log.push(entry.grammar)
    return { tag: entry.grammar, parse: () => ({}), sub: () => {} }
  }
}

function entry(languageId, grammar, extra) {
  return Object.assign({ languageId, grammar, options: {} }, extra)
}

describe('lsp-instances-identity', () => {
  it('an unscoped and a folder-scoped entry do not share a cache slot', () => {
    // Same languageId, same options, same _dir — and different
    // grammars. This is the ordinary shape of a session where the
    // client passes initializationOptions.languages (session-wide,
    // _scope null, _dir = workspaceFolders[0]) and that same first
    // folder also carries a .tabnas/lsp.json entry for the id.
    //
    // Keyed on (languageId, options, dir) these collided, so whichever
    // document was opened first decided which grammar served BOTH
    // routes for the rest of the session.
    const log = []
    const inst = new SerialInstances(maker(log))

    const fromInit = entry('mydsl', 'init.json', { _dir: '/ws/a', _scope: null })
    const fromManifest = entry('mydsl', 'manifest.json', { _dir: '/ws/a', _scope: '/ws/a' })

    const a = inst.get(fromInit, '/ws/a')
    const b = inst.get(fromManifest, '/ws/a')

    assert.equal(a.tag, 'init.json')
    assert.equal(b.tag, 'manifest.json', 'the scoped entry got the unscoped grammar')
    assert.deepEqual(log, ['init.json', 'manifest.json'])

    // ...and each still caches on its own key.
    assert.equal(inst.get(fromInit, '/ws/a').tag, 'init.json')
    assert.equal(log.length, 2, 'a second get rebuilt instead of hitting cache')
  })

  it('invalidating one entry leaves another folder quarantine intact', () => {
    // Quarantine is per (entry, folder). Hot-reload used to clear every
    // failure whose key began with the languageId, so editing a healthy
    // grammar in one folder released a DIFFERENT folder's quarantined
    // grammar back into service, where it resumed throwing on every
    // keystroke until it earned its way back out.
    const broken = entry('mydsl', 'broken.json', { _dir: '/ws/a', _scope: '/ws/a' })
    const healthy = entry('mydsl', 'healthy.json', { _dir: '/ws/b', _scope: '/ws/b' })

    const inst = new SerialInstances((e) => {
      if ('broken.json' === e.grammar) throw new Error('bad grammar')
      return { tag: e.grammar, parse: () => ({}), sub: () => {} }
    })

    // Drive the broken one into quarantine.
    for (let i = 0; i < 10; i++) {
      try { inst.get(broken, '/ws/a') } catch (e) { /* expected */ }
    }
    assert.ok(inst.quarantined(broken, '/ws/a'), 'never quarantined')

    // An unrelated grammar changes in the OTHER folder.
    inst.invalidate(healthy)

    assert.ok(
      inst.quarantined(broken, '/ws/a'),
      'an unrelated folder\'s reload released the quarantined grammar')
  })

  it('invalidating an entry does clear its own quarantine', () => {
    // The converse, so the narrowing above cannot be "fixed" by never
    // clearing anything: reloading the grammar that failed must still
    // give it another chance.
    const broken = entry('mydsl', 'broken.json', { _dir: '/ws/a', _scope: '/ws/a' })
    const inst = new SerialInstances(() => { throw new Error('bad grammar') })

    for (let i = 0; i < 10; i++) {
      try { inst.get(broken, '/ws/a') } catch (e) { /* expected */ }
    }
    assert.ok(inst.quarantined(broken, '/ws/a'))

    inst.invalidate(broken)
    assert.equal(inst.quarantined(broken, '/ws/a'), false,
      'reloading the failing grammar did not clear its own quarantine')
  })
})
