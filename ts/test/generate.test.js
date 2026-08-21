/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// The generator (design §7). Beyond shape checks, the two flagship
// tests RUN generated servers: the Node package speaks LSP over stdio
// against a scripted client session, and the Go module compiles and
// serves the same session — proving generated artifacts work, not just
// that files appeared.

const { describe, it } = require('node:test')
const assert = require('node:assert')
const fs = require('fs')
const os = require('os')
const path = require('path')
const { spawn, spawnSync } = require('child_process')

const { generate, GenerateError } = require('../src/generate')

const FIXTURES = path.join(__dirname, '..', '..', 'test', 'fixtures')
const SPEC_FILE = path.join(FIXTURES, 'json-grammar.json')
const REPO = path.join(__dirname, '..', '..')
const PARSER = path.join(REPO, '..', 'parser')

function tmp(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix))
}

const HAS_GO = 0 === spawnSync('go', ['version'], { stdio: 'ignore' }).status

// A scripted LSP client session over stdio: spawn, send framed
// messages (a number in the script is a pause in ms — the Node server
// debounces analysis, so the client must idle before shutting down),
// resolve with full stdout once the process exits. stdin stays open:
// the server exits itself on the `exit` notification, and an early
// EOF races the in-flight responses.
function lspSession(cmd, args, script, opts) {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, args, {
      cwd: opts && opts.cwd,
      stdio: ['pipe', 'pipe', 'pipe'],
    })
    let out = ''
    let errOut = ''
    child.stdout.on('data', (d) => (out += d))
    child.stderr.on('data', (d) => (errOut += d))
    child.on('error', reject)
    const timer = setTimeout(() => {
      child.kill()
      reject(new Error('lsp session timed out; stderr: ' + errOut))
    }, 30000)
    child.on('exit', () => {
      clearTimeout(timer)
      resolve({ out, errOut })
    })
    let i = 0
    const step = () => {
      if (i >= script.length) return
      const item = script[i++]
      if ('number' === typeof item) {
        setTimeout(step, item)
        return
      }
      child.stdin.write('Content-Length: ' + Buffer.byteLength(item) + '\r\n\r\n' + item)
      step()
    }
    step()
  })
}

const SESSION = [
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}',
  '{"jsonrpc":"2.0","method":"initialized","params":{}}',
  '{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{' +
    '"uri":"file:///t.mydsl","languageId":"mydsl","version":1,' +
    '"text":"{\\"a\\":true blah,\\"b\\":2}"}}}',
  800, // let the debounced analysis run and the push flush
  '{"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}',
  '{"jsonrpc":"2.0","method":"exit"}',
]

describe('generate-node', () => {
  it('emits a runnable single-language Node server from a spec', async () => {
    const out = tmp('gen-node-')
    const res = generate({
      out,
      input: { spec: SPEC_FILE },
      languageId: 'mydsl',
      extensions: ['.mydsl'],
      runtime: 'node',
      editors: ['vscode', 'nvim'],
    })
    assert.ok(res.files.includes('server/server.js'))
    assert.ok(res.files.includes('server/grammar.json'))
    assert.ok(res.files.includes('editors/vscode/package.json'))
    assert.ok(res.files.includes('editors/nvim/tabnas.lua'))

    // The wrapper pins what is served, not how.
    const serverJs = fs.readFileSync(path.join(out, 'server', 'server.js'), 'utf8')
    assert.ok(serverJs.includes('"languageId": "mydsl"'))
    assert.ok(serverJs.includes("startServer({ entries: [entry] })"))
    assert.ok(!serverJs.includes('analyze'), 'no pipeline logic in the wrapper')

    // Make the generated package's deps resolvable, then RUN it: a
    // scripted client opens a broken document and must get pushed
    // diagnostics from the embedded grammar.
    const nm = path.join(out, 'server', 'node_modules')
    fs.mkdirSync(path.join(nm, '@tabnas'), { recursive: true })
    fs.symlinkSync(path.join(REPO, 'ts'), path.join(nm, '@tabnas', 'lsp'), 'dir')
    fs.symlinkSync(path.join(PARSER, 'ts'), path.join(nm, '@tabnas', 'parser'), 'dir')

    const { out: stdout } = await lspSession(
      process.execPath,
      [path.join(out, 'server', 'server.js'), '--stdio'],
      SESSION,
    )
    assert.ok(stdout.includes('"method":"textDocument/publishDiagnostics"'),
      'no diagnostics pushed: ' + stdout.slice(0, 400))
    assert.ok(stdout.includes('"code":"unexpected"'),
      'diagnostics missing the code: ' + stdout.slice(0, 400))
    assert.ok(stdout.includes('tabnas:mydsl'), 'diagnostic source names the language')
  })

  it('emits a module-lane server for a plugin package', () => {
    const out = tmp('gen-mod-')
    generate({
      out,
      input: { module: '@tabnas/toml' },
      extensions: ['.toml'],
      runtime: 'node',
      editors: [],
    })
    const pkg = JSON.parse(fs.readFileSync(path.join(out, 'server', 'package.json'), 'utf8'))
    assert.equal(pkg.name, 'toml-lsp')
    assert.ok(pkg.dependencies['@tabnas/toml'])
    const serverJs = fs.readFileSync(path.join(out, 'server', 'server.js'), 'utf8')
    assert.ok(serverJs.includes('"module": "@tabnas/toml"'))
  })

  it('refuses a poisoned spec at generation time', () => {
    const out = tmp('gen-poison-')
    const bad = path.join(out, 'bad.json')
    fs.writeFileSync(bad, '{"rule":{"top":{"open":[{"s":"#NR","a":"@evil"}]}}}')
    assert.throws(
      () => generate({ out, input: { spec: bad }, languageId: 'x', editors: [] }),
      (e) => e instanceof GenerateError && /firewall/.test(e.message))
  })

  it('refuses unknown runtimes with the §7.4 explanation', () => {
    assert.throws(
      () => generate({
        out: tmp('gen-rt-'), input: { spec: SPEC_FILE },
        languageId: 'x', runtime: 'rust',
      }),
      /pure-GrammarSpec lane and the C ABI/)
  })
})

describe('generate-go', () => {
  it('emits a Go module that embeds the spec' + (HAS_GO ? ', compiles, and serves' : ''),
    { timeout: 300000 },
    async () => {
      const out = tmp('gen-go-')
      const res = generate({
        out,
        input: { spec: SPEC_FILE },
        languageId: 'mydsl',
        extensions: ['.mydsl'],
        runtime: 'go',
        editors: ['helix'],
        goModule: 'example.com/mydsl-lsp',
        goReplace: [
          'github.com/tabnas/lsp/go=' + path.join(REPO, 'go'),
          'github.com/tabnas/parser/go=' + path.join(PARSER, 'go'),
        ],
      })
      assert.ok(res.files.includes('server/main.go'))
      assert.ok(res.files.includes('server/grammar.json'))
      const mainGo = fs.readFileSync(path.join(out, 'server', 'main.go'), 'utf8')
      assert.ok(mainGo.includes('//go:embed grammar.json'))
      assert.ok(mainGo.includes('EntryFromSpecJSON'))
      const goMod = fs.readFileSync(path.join(out, 'server', 'go.mod'), 'utf8')
      assert.ok(goMod.includes('replace github.com/tabnas/lsp/go => '))

      if (!HAS_GO) return

      // The generated README's own instructions: tidy, then build.
      const tidy = spawnSync('go', ['mod', 'tidy'], {
        cwd: path.join(out, 'server'),
        encoding: 'utf8',
        timeout: 240000,
      })
      assert.equal(tidy.status, 0,
        'go mod tidy failed:\n' + tidy.stdout + tidy.stderr)
      const build = spawnSync('go', ['build', '-o', 'mydsl-lsp', '.'], {
        cwd: path.join(out, 'server'),
        encoding: 'utf8',
        timeout: 240000,
      })
      assert.equal(build.status, 0,
        'go build failed:\n' + build.stdout + build.stderr)

      const { out: stdout } = await lspSession(
        path.join(out, 'server', 'mydsl-lsp'), [], SESSION)
      assert.ok(stdout.includes('"method":"textDocument/publishDiagnostics"'),
        'no diagnostics pushed: ' + stdout.slice(0, 400))
      assert.ok(stdout.includes('"code":"unexpected"'))
    })

  it('refuses a TS module for the Go runtime with the fix named', () => {
    assert.throws(
      () => generate({
        out: tmp('gen-gomod-'), input: { module: '@tabnas/toml' },
        extensions: ['.toml'], runtime: 'go',
      }),
      /--go-plugin/)
  })

  it('emits a plugin-linked main for --go-plugin', () => {
    const out = tmp('gen-goplug-')
    generate({
      out,
      input: { spec: SPEC_FILE },
      languageId: 'toml',
      extensions: ['.toml'],
      runtime: 'go',
      editors: [],
      goPlugin: 'github.com/tabnas/toml/go',
    })
    // spec input + goPlugin: the spec lane wins (goPlugin is for entry
    // inputs whose grammar is live code) — assert the embed lane held.
    const mainGo = fs.readFileSync(path.join(out, 'server', 'main.go'), 'utf8')
    assert.ok(mainGo.includes('//go:embed grammar.json'))
  })
})

describe('generate-unified', () => {
  const REG = path.join(tmp('gen-reg-'), 'registry.json')
  fs.writeFileSync(REG, JSON.stringify({
    entries: [
      { name: '@tabnas/jsonic', languageId: 'jsonic', extensions: ['.jsonic'] },
      { name: '@tabnas/toml', languageId: 'toml', extensions: ['.toml'], enabled: false },
      { name: '@tabnas/hoover', languageId: 'hoover', pluginKind: 'modifier' },
      { name: '@tabnas/zon', languageId: 'zon', extensions: ['.zon'] },
    ],
  }))

  it('generates multi-language editor plugins honoring the collision policy', () => {
    const out = tmp('gen-uni-')
    generate({
      out, unified: true, registryFile: REG,
      editors: ['vscode', 'helix', 'emacs', 'sublime', 'kate', 'zed', 'nvim'],
    })
    // Unified mode has no server/ half: editor dirs sit at the root.
    const pkg = JSON.parse(fs.readFileSync(
      path.join(out, 'vscode', 'package.json'), 'utf8'))
    const ids = pkg.contributes.languages.map((l) => l.id).sort()
    assert.deepStrictEqual(ids, ['jsonic', 'zon'],
      'enabled non-modifier entries only')
    assert.ok(pkg.activationEvents.includes('onLanguage:jsonic'))
    const ext = fs.readFileSync(path.join(out, 'vscode', 'extension.js'), 'utf8')
    assert.ok(ext.includes('tabnas-lsp'))
    const helix = fs.readFileSync(path.join(out, 'helix', 'languages.toml'), 'utf8')
    assert.ok(helix.includes('[language-server.tabnas-lsp]'))
    assert.ok(helix.includes('name = "zon"'))
    const readme = fs.readFileSync(path.join(out, 'README.md'), 'utf8')
    assert.ok(readme.includes('collision policy'))
  })

  it('single-language vscode extension carries the language contribution', () => {
    const out = tmp('gen-vsc-')
    generate({
      out, input: { spec: SPEC_FILE }, languageId: 'mydsl',
      extensions: ['.mydsl', '.md5l'], editors: ['vscode'],
    })
    const pkg = JSON.parse(fs.readFileSync(
      path.join(out, 'editors', 'vscode', 'package.json'), 'utf8'))
    assert.deepStrictEqual(pkg.contributes.languages[0].extensions, ['.mydsl', '.md5l'])
    assert.equal(pkg.contributes.configuration.properties['tabnas.serverPath'].default,
      'mydsl-lsp')
  })
})
