/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// The dynamic-add lanes (design §6) and the grammar firewall (design
// §10). The L2 lane is exercised with the shared strict-JSON
// GrammarSpec fixture — the same pure-data grammar the Go port loads,
// which is what makes it the standing proof of the lane.

const { describe, it } = require('node:test')
const assert = require('node:assert')
const fs = require('fs')
const path = require('path')

const parserMod = require('@tabnas/parser')

const {
  makeLoader,
  firewallSpec,
  resolveSandboxed,
  LoadError,
} = require('../src/loaders')
const { normalize } = require('../src/registry')

const FIXTURES = path.join(__dirname, '..', '..', 'test', 'fixtures')
const JSON_GRAMMAR = JSON.parse(
  fs.readFileSync(path.join(FIXTURES, 'json-grammar.json'), 'utf8'))

// Loader instances have recovery on, so parse returns { value, errors }
// (the recovery-mode result shape); unwrap for value assertions.
function val(out) {
  assert.deepStrictEqual(out.errors, [], 'clean parse expected')
  return out.value
}

describe('lsp-firewall', () => {
  it('accepts the shared strict-JSON grammar', () => {
    assert.deepStrictEqual(firewallSpec(JSON_GRAMMAR, parserMod), [])
  })

  it('refuses prototype-pollution keys anywhere in the tree', () => {
    // JSON.parse creates __proto__ as a real own property; the scan
    // must see it even though it hides behind the inherited accessor.
    const gs = JSON.parse('{"rule":{"val":{"open":[{"s":"#NR","u":{"__proto__":{"x":1}}}]}}}')
    const issues = firewallSpec(gs, parserMod)
    assert.ok(issues.some((i) => /forbidden key '__proto__'/.test(i.message)),
      JSON.stringify(issues))
  })

  it('refuses a ref bag: live functions are not JSON', () => {
    const issues = firewallSpec({ ref: {}, rule: {} }, parserMod)
    assert.ok(issues.some((i) => '$.ref' === i.path))
  })

  it('refuses plugins inside grammar options', () => {
    const issues = firewallSpec({ options: { plugins: [] } }, parserMod)
    assert.ok(issues.some((i) => '$.options.plugins' === i.path))
  })

  it('refuses non-builtin function refs in alt positions', () => {
    const gs = { rule: { val: { open: [{ s: '#NR', a: '@evil' }] } } }
    const issues = firewallSpec(gs, parserMod)
    assert.ok(issues.some((i) => /unknown function reference '@evil'/.test(i.message)))
  })

  it('passes serialized regexes and escapes in options, refuses bare refs', () => {
    const ok = firewallSpec({
      options: { x: '@@literal', y: '@SKIP', z: '@/ab+/i' },
    }, parserMod)
    assert.deepStrictEqual(ok, [])
    const bad = firewallSpec({ options: { x: '@custom' } }, parserMod)
    assert.ok(bad.some((i) => '$.options.x' === i.path))
  })

  it('gates the builtin schema version', () => {
    const issues = firewallSpec({ v: 999, rule: {} }, parserMod)
    assert.ok(issues.some((i) => '$.v' === i.path), JSON.stringify(issues))
  })

  it('caps the rule count', () => {
    const rule = {}
    for (let i = 0; i < 5001; i++) rule['r' + i] = { open: [{ s: '#NR' }] }
    const issues = firewallSpec({ rule }, parserMod)
    assert.ok(issues.some((i) => /more than 5000/.test(i.message)))
  })
})

describe('lsp-loader-spec', () => {
  it('L2: loads the shared grammar from an inline object', () => {
    const makeInstance = makeLoader()
    const entry = normalize({
      name: 'jsonspec',
      languageId: 'jsonspec',
      grammarKind: 'data',
      load: { spec: JSON_GRAMMAR },
    })
    const tn = makeInstance(entry)
    // deepEqual, not deepStrictEqual: the @object$ builtin builds
    // null-prototype objects (hardening), which strict compares reject.
    assert.deepEqual(val(tn.parse('{"a":[1,2]}')), { a: [1, 2] })
  })

  it('L2: loads a spec from a sandboxed file path', () => {
    const makeInstance = makeLoader()
    const entry = normalize({
      name: 'jsonspec',
      languageId: 'jsonspec',
      load: { spec: 'fixtures/json-grammar.json' },
      _source: 'workspace',
      _dir: path.join(__dirname, '..', '..', 'test'),
    })
    const tn = makeInstance(entry)
    assert.deepEqual(val(tn.parse('[true,null]')), [true, null])
  })

  it('L2: recovery is on — a broken document yields errors, not a throw', () => {
    const makeInstance = makeLoader()
    const entry = normalize({
      name: 'jsonspec', languageId: 'jsonspec', load: { spec: JSON_GRAMMAR },
    })
    const tn = makeInstance(entry)
    const out = tn.parse('{"a":true blah,"b":2}')
    assert.ok(out && 'object' === typeof out && Array.isArray(out.errors),
      'recovery-mode result shape: ' + JSON.stringify(out))
    assert.ok(1 <= out.errors.length)
  })

  it('L2: a poisoned spec is refused before any engine load', () => {
    const makeInstance = makeLoader()
    const entry = normalize({
      name: 'bad', languageId: 'bad',
      load: { spec: JSON.parse('{"rule":{"__proto__":{"open":[]}}}') },
    })
    assert.throws(() => makeInstance(entry), (e) =>
      e instanceof LoadError && /firewall/.test(e.message))
  })

  it('sandbox: a spec path may not escape its folder', () => {
    assert.throws(
      () => resolveSandboxed('../../../etc/passwd', '/tmp/ws/project'),
      /escapes its workspace folder/)
    assert.throws(
      () => resolveSandboxed('/etc/passwd', '/tmp/ws/project'),
      /escapes its workspace folder/)
    // Sibling-prefix folders must not pass the check by string prefix.
    assert.throws(
      () => resolveSandboxed('../project-evil/x.json', '/tmp/ws/project'),
      /escapes its workspace folder/)
    assert.equal(
      resolveSandboxed('sub/x.json', '/tmp/ws/project'),
      path.join('/tmp/ws/project', 'sub', 'x.json'))
  })
})

describe('lsp-loader-trust', () => {
  it('workspace module loads are refused without the trust flag', () => {
    const makeInstance = makeLoader((name) => {
      if ('@tabnas/parser' === name) return parserMod
      return function anyPlugin() {}
    })
    const entry = normalize({
      name: 'wsmod', languageId: 'wsmod',
      load: { module: 'some-npm-module' },
      _source: 'workspace', _dir: '/tmp/ws',
    })
    assert.throws(() => makeInstance(entry), /Refused.*trustWorkspaceModules/s)
  })

  it('the trust flag is read late, so initialize can grant it', () => {
    const loaderOpts = { trust: {} }
    const makeInstance = makeLoader((name) => {
      if ('@tabnas/parser' === name) return parserMod
      return function plain(tn) {
        tn.rule('plained', (rs) => rs.open([{ s: [] }]))
        return tn
      }
    }, loaderOpts)
    const entry = normalize({
      name: 'wsmod', languageId: 'wsmod',
      load: { module: 'some-npm-module' },
      _source: 'workspace', _dir: '/tmp/ws',
    })
    assert.throws(() => makeInstance(entry), /Refused/)
    loaderOpts.trust.workspaceModules = true // what initialize does
    const tn = makeInstance(entry)
    assert.ok(Object.keys(tn.rule()).includes('plained'))
  })

  it('bundled module entries need no trust flag', () => {
    const makeInstance = makeLoader((name) => {
      if ('@tabnas/parser' === name) return parserMod
      return function plain(tn) { return tn }
    })
    const entry = normalize({ name: 'fleetmod', languageId: 'fleetmod' })
    assert.ok(makeInstance(entry))
  })
})

describe('lsp-loader-grammar', () => {
  // L3 dispatch and compile, with a mock dialect package: the real
  // dialect packages are separate repos; the lane's contract is what
  // is under test here.
  const TINY_SPEC = JSON.parse(
    '{"clear":true,"options":{"rule":{"start":"top"}},' +
      '"rule":{"top":{"open":[{"s":"#NR"}]}}}')

  function mockRequire(overrides) {
    return (name) => {
      if ('@tabnas/parser' === name) return parserMod
      if (overrides && name in overrides) {
        const v = overrides[name]
        if (v instanceof Error) throw v
        return v
      }
      throw new Error('Cannot find module ' + name)
    }
  }

  function grammarEntry(file, dir) {
    return normalize({
      name: 'mydsl', languageId: 'mydsl',
      load: { grammar: file },
      _source: 'workspace', _dir: dir,
    })
  }

  it('compiles a .abnf file through the dialect package', () => {
    const seen = {}
    const dir = fs.mkdtempSync(path.join(require('os').tmpdir(), 'lsp-l3-'))
    fs.writeFileSync(path.join(dir, 'my.abnf'), 'top = 1*DIGIT\n')
    const makeInstance = makeLoader(mockRequire({
      '@tabnas/abnf': {
        abnfConvert: (src, opts) => {
          seen.src = src
          seen.opts = opts
          return TINY_SPEC
        },
        toPureSpec: (spec) => {
          seen.pure = true
          return spec
        },
      },
    }))
    const tn = makeInstance(grammarEntry('my.abnf', dir))
    assert.equal(seen.src, 'top = 1*DIGIT\n')
    assert.equal(seen.opts.builtins, true, 'compiled with builtins: true')
    assert.equal(seen.pure, true, 'lowered via toPureSpec')
    // The tiny mock grammar recognizes without building a value; a
    // clean errors list is what proves the compiled spec installed.
    assert.deepStrictEqual(tn.parse('7').errors, [])
  })

  it('dispatches per dialect, never try-them-all', () => {
    const dir = fs.mkdtempSync(path.join(require('os').tmpdir(), 'lsp-l3-'))
    fs.writeFileSync(path.join(dir, 'my.ebnf'), 'x')
    const abnfCalled = []
    const makeInstance = makeLoader(mockRequire({
      '@tabnas/abnf': { abnfConvert: () => { abnfCalled.push(1); return TINY_SPEC } },
      '@tabnas/ebnf': { ebnfConvert: () => TINY_SPEC },
    }))
    makeInstance(grammarEntry('my.ebnf', dir))
    assert.deepStrictEqual(abnfCalled, [], 'abnf must not see .ebnf text')
  })

  it('reports a missing dialect package usefully', () => {
    const dir = fs.mkdtempSync(path.join(require('os').tmpdir(), 'lsp-l3-'))
    fs.writeFileSync(path.join(dir, 'my.gbnf'), 'x')
    const makeInstance = makeLoader(mockRequire({}))
    assert.throws(() => makeInstance(grammarEntry('my.gbnf', dir)),
      /dialect package not installed: @tabnas\/gbnf/)
  })

  it('refuses an unknown grammar extension', () => {
    const dir = fs.mkdtempSync(path.join(require('os').tmpdir(), 'lsp-l3-'))
    fs.writeFileSync(path.join(dir, 'my.pegjs'), 'x')
    const makeInstance = makeLoader(mockRequire({}))
    assert.throws(() => makeInstance(grammarEntry('my.pegjs', dir)),
      /unknown grammar dialect/)
  })

  it('firewalls the compiled spec too', () => {
    const dir = fs.mkdtempSync(path.join(require('os').tmpdir(), 'lsp-l3-'))
    fs.writeFileSync(path.join(dir, 'my.abnf'), 'x')
    const makeInstance = makeLoader(mockRequire({
      '@tabnas/abnf': {
        abnfConvert: () => ({ rule: { top: { open: [{ s: '#NR', a: '@notbuiltin' }] } } }),
      },
    }))
    assert.throws(() => makeInstance(grammarEntry('my.abnf', dir)),
      (e) => e instanceof LoadError && /firewall/.test(e.message))
  })
})

describe('lsp-shared-fixture-drift', () => {
  it('json-grammar.json matches the engine fixture when the sibling is present', () => {
    // Derive-don't-duplicate: the shared grammar is a copy of the
    // engine's builder fixture; this gate keeps the copy honest in any
    // checkout that has the sibling (local fleet and CI both do).
    const sibling = path.join(
      __dirname, '..', '..', '..', 'parser', 'ts', 'test',
      'json-builder.fixture.json')
    let engineBytes
    try {
      engineBytes = fs.readFileSync(sibling, 'utf8')
    } catch (e) {
      return // no sibling checkout — nothing to compare against
    }
    const ours = fs.readFileSync(
      path.join(FIXTURES, 'json-grammar.json'), 'utf8')
    assert.equal(ours, engineBytes,
      'test/fixtures/json-grammar.json drifted from parser/ts/test/json-builder.fixture.json — re-copy it')
  })
})
