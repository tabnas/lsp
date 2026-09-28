/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// The generator (design §7). Beyond shape checks, the three flagship
// tests RUN generated servers: the Node package speaks LSP over stdio
// against a scripted client session, and the Go module and the Rust
// crate compile and serve the same session — proving generated
// artifacts work, not just that files appeared.

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

// The Go e2e case compiles and runs the generated Go server. Gated on
// the toolchain being present AND not explicitly suppressed: hosted CI
// runner images ship SOME Go, so a bare probe would run this against
// an unpinned runner-image version in jobs that never asked for Go —
// the lsp ci workflow sets TABNAS_LSP_GO_E2E=0 in its node job and
// owns Go coverage in a pinned ci-go job.
const HAS_GO = '0' !== process.env.TABNAS_LSP_GO_E2E &&
  0 === spawnSync('go', ['version'], { stdio: 'ignore' }).status

// The Rust e2e case, gated the same way and for the same reason: hosted
// runner images ship SOME Rust too, so the node and Go jobs set
// TABNAS_LSP_RUST_E2E=0 and the ci-rust job owns the case under the
// pinned MSRV toolchain.
const HAS_CARGO = '0' !== process.env.TABNAS_LSP_RUST_E2E &&
  0 === spawnSync('cargo', ['--version'], { stdio: 'ignore' }).status

// `cargo build` of a generated crate, reporting every 15 s: a cold build
// compiles the engine and tabnas-lsp and runs for a minute or more, and
// a silent one cannot be told from a hung one. The target dir is named
// explicitly so a CARGO_TARGET_DIR in the environment cannot move the
// binary the session then runs.
function cargoBuild(cwd, targetDir) {
  return new Promise((resolve, reject) => {
    const started = Date.now()
    const child = spawn('cargo', ['build', '--target-dir', targetDir], {
      cwd,
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    let log = ''
    let last = 'starting'
    const take = (d) => {
      log += d
      const lines = String(d).split('\n').map((l) => l.trim()).filter(Boolean)
      if (0 < lines.length) last = lines[lines.length - 1]
    }
    child.stdout.on('data', take)
    child.stderr.on('data', take)
    const beat = setInterval(() => {
      process.stderr.write('generate-rust: cargo build ' +
        Math.round((Date.now() - started) / 1000) + ' s, percentage unknown; ' +
        last + '\n')
    }, 15000)
    const timer = setTimeout(() => child.kill(), 540000)
    const done = () => {
      clearInterval(beat)
      clearTimeout(timer)
    }
    child.on('error', (e) => {
      done()
      reject(e)
    })
    child.on('exit', (code) => {
      done()
      resolve({ code, log })
    })
  })
}

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

    // Dependencies are pinned to the exact resolvable versions — a
    // generated server freezes behavior, and a range lets npm move it.
    const pkg = JSON.parse(fs.readFileSync(path.join(out, 'server', 'package.json'), 'utf8'))
    assert.equal(pkg.dependencies['@tabnas/lsp'],
      require('../package.json').version)
    assert.equal(pkg.dependencies['@tabnas/parser'],
      require(path.join(PARSER, 'ts', 'package.json')).version)

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
        languageId: 'x', runtime: 'zig',
      }),
      /node, go, rust exist today.*pure-GrammarSpec lane and the C ABI/)
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

  it('plugin-linked entries: sane identifiers, metadata, honest go.mod', () => {
    const regDir = tmp('gen-goreg-')
    const REG = path.join(regDir, 'registry.json')
    fs.writeFileSync(REG, JSON.stringify({
      entries: [{
        name: '@tabnas/foo-lang', languageId: 'foo-lang',
        extensions: ['.fl'], grammarKind: 'closure',
        syncGroups: ['end', 'comma'],
        semanticTokens: { '#KEY': 'property' },
      }],
    }))
    const gen = (extra) => {
      const out = tmp('gen-goent-')
      generate(Object.assign({
        out, input: { entry: 'foo-lang' }, runtime: 'go', editors: [],
        registryFile: REG,
        goPlugin: 'github.com/tabnas/foolang/go/v2',
      }, extra))
      return {
        mainGo: fs.readFileSync(path.join(out, 'server', 'main.go'), 'utf8'),
        goMod: fs.readFileSync(path.join(out, 'server', 'go.mod'), 'utf8'),
      }
    }

    const { mainGo, goMod } = gen({})
    // 'foo-lang' must not become the invalid identifier 'Foo-lang'.
    assert.ok(mainGo.includes('plugin.FooLang'), mainGo)
    // Registry recovery/highlighting metadata reaches the Go entry —
    // dropping SyncGroups changes where the generated server recovers.
    assert.ok(mainGo.includes('SyncGroups: []string{"end", "comma"}'), mainGo)
    assert.ok(mainGo.includes('SemanticTokens: map[string]string{"#KEY": "property"}'))
    // The /v2 semantic-import suffix survives in the import path, and
    // no fabricated v0.0.0 require appears — go mod tidy resolves the
    // plugin from the import (or a replace) unless a version is given.
    assert.ok(mainGo.includes('"github.com/tabnas/foolang/go/v2"'))
    assert.ok(!goMod.includes('v0.0.0'), goMod)
    assert.ok(!goMod.includes('github.com/tabnas/foolang'), goMod)

    const pinned = gen({ goPluginVersion: 'v2.1.0' })
    assert.ok(pinned.goMod.includes('github.com/tabnas/foolang/go/v2 v2.1.0'))
  })
})

describe('generate-rust', () => {
  const RS = path.join(REPO, 'rs')
  const cargoToml = (out) => fs.readFileSync(path.join(out, 'server', 'Cargo.toml'), 'utf8')
  const mainRs = (out) => fs.readFileSync(path.join(out, 'server', 'src', 'main.rs'), 'utf8')

  it('emits a Cargo crate that embeds the spec' +
    (HAS_CARGO ? ', builds, and serves' : ''),
  { timeout: 600000 },
  async (t) => {
    const out = tmp('gen-rs-')
    const res = generate({
      out,
      input: { spec: SPEC_FILE },
      languageId: 'mydsl',
      extensions: ['.mydsl'],
      runtime: 'rust',
      editors: ['helix'],
      // This checkout's crate, not the published one: the counterpart
      // of the Go case's replace directives.
      rustPath: ['tabnas-lsp=' + RS],
    })
    for (const f of ['server/Cargo.toml', 'server/src/main.rs',
      'server/grammar.json', 'editors/helix/languages.toml', 'README.md']) {
      assert.ok(res.files.includes(f), f + ' not emitted: ' + res.files)
    }

    // The wrapper pins what is served, not how.
    const main = mainRs(out)
    assert.ok(main.includes('include_str!("../grammar.json")'), main)
    assert.ok(main.includes(
      'entry_from_spec_json("mydsl", &[".mydsl"], GRAMMAR)'), main)
    assert.ok(main.includes('tabnas_lsp::serve(config)'), main)
    // The protocol's exit status: 0 after shutdown, 1 after exit alone or
    // a closed stream, as the repository's own binary reports it.
    assert.ok(main.includes('Ok(true) => ExitCode::SUCCESS'), main)
    assert.ok(main.includes('Ok(false) => {'), main)
    assert.ok(main.includes('the session ended without shutdown'), main)
    assert.ok(!/analy[sz]e|semantic|outline/.test(main),
      'no pipeline logic in the wrapper:\n' + main)
    assert.deepStrictEqual(
      JSON.parse(fs.readFileSync(path.join(out, 'server', 'grammar.json'), 'utf8')),
      JSON.parse(fs.readFileSync(SPEC_FILE, 'utf8')))
    const cargo = cargoToml(out)
    assert.ok(cargo.includes('tabnas-lsp = { path = ' + JSON.stringify(RS) + ' }'), cargo)
    assert.ok(!cargo.includes('[patch.'), 'a path crate needs no patch table:\n' + cargo)
    const readme = fs.readFileSync(path.join(out, 'README.md'), 'utf8')
    assert.ok(readme.includes('cargo install --path .'), readme)

    if (!HAS_CARGO) {
      t.diagnostic('cargo not installed, or TABNAS_LSP_RUST_E2E=0: the ' +
        'crate was generated and checked but not built or run')
      return
    }

    // The generated README's own instruction builds the release profile;
    // the test builds the dev profile, which compiles in half the time
    // and serves the same session.
    const server = path.join(out, 'server')
    const target = path.join(server, 'target')
    try {
      const build = await cargoBuild(server, target)
      assert.equal(build.code, 0, 'cargo build failed:\n' + build.log)
      const bin = path.join(target, 'debug',
        'mydsl-lsp' + ('win32' === process.platform ? '.exe' : ''))
      const { out: stdout } = await lspSession(bin, ['--stdio'], SESSION)
      assert.ok(stdout.includes('"method":"textDocument/publishDiagnostics"'),
        'no diagnostics pushed: ' + stdout.slice(0, 400))
      assert.ok(stdout.includes('"code":"unexpected"'),
        'diagnostics missing the code: ' + stdout.slice(0, 400))
      assert.ok(stdout.includes('tabnas:mydsl'), 'diagnostic source names the language')
    } finally {
      // A debug target is most of a gigabyte; the crate itself stays.
      fs.rmSync(target, { recursive: true, force: true })
    }
  })

  it('the default crate takes its tabnas crates from git and pins only what it is given', () => {
    const out = tmp('gen-rsgit-')
    generate({
      out, input: { spec: SPEC_FILE }, languageId: 'mydsl',
      runtime: 'rust', editors: [],
    })
    const cargo = cargoToml(out)
    assert.ok(cargo.includes('tabnas-lsp = { git = "https://github.com/tabnas/lsp" }'), cargo)
    // tabnas-lsp names the engine by sibling path, which cargo reads as a
    // package of the lsp repository; the patch table supplies it.
    assert.ok(cargo.includes('[patch."https://github.com/tabnas/lsp"]\n' +
      'tabnas = { git = "https://github.com/tabnas/parser" }'), cargo)
    assert.ok(!/\brev = /.test(cargo), 'no revision the caller did not supply:\n' + cargo)
    assert.match(cargo, /Cargo\.lock\s*\n?#?\s*records the commit/)
    assert.ok(cargo.includes('\n[workspace]\n'), 'a workspace of its own:\n' + cargo)

    // The MSRV is tabnas-lsp's own: a crate over it builds with no older
    // compiler, and rs/ is not in the npm package to be read at run time.
    const rsToml = fs.readFileSync(path.join(REPO, 'rs', 'Cargo.toml'), 'utf8')
    const msrv = /^rust-version\s*=\s*"([^"]+)"/m.exec(rsToml)[1]
    assert.ok(cargo.includes('rust-version = "' + msrv + '"'),
      'the generator\'s rust-version drifted from rs/Cargo.toml (' + msrv + '):\n' + cargo)
    // With no lock, the first build resolves crates.io afresh; the
    // MSRV-aware resolver keeps that resolution buildable at the MSRV.
    assert.ok(cargo.includes('resolver = "3"'), cargo)

    const pinnedOut = tmp('gen-rspin-')
    generate({
      out: pinnedOut, input: { spec: SPEC_FILE }, languageId: 'mydsl',
      runtime: 'rust', editors: [],
      rustLspRev: '0123abc', rustParserRev: '4567def',
    })
    const pinned = cargoToml(pinnedOut)
    assert.ok(pinned.includes(
      'tabnas-lsp = { git = "https://github.com/tabnas/lsp", rev = "0123abc" }'), pinned)
    assert.ok(pinned.includes(
      'tabnas = { git = "https://github.com/tabnas/parser", rev = "4567def" }'), pinned)
    assert.match(pinned, /pinned/)
  })

  it('supplies every optional sibling of rs/Cargo.toml, and what each names, from git', (t) => {
    const out = tmp('gen-rssib-')
    generate({
      out, input: { spec: SPEC_FILE }, languageId: 'mydsl',
      runtime: 'rust', editors: [],
    })
    const cargo = cargoToml(out)
    // The tables, as written: one per repository, the engine first.
    const tables = new Map()
    for (const block of cargo.split('\n[patch.').slice(1)) {
      const lines = block.split('\n')
      const source = /^"([^"]+)"\]/.exec(lines[0])[1]
      const names = []
      for (const line of lines.slice(1)) {
        const dep = /^([A-Za-z0-9_-]+) = \{ git = "([^"]+)"/.exec(line)
        if (!dep) break
        names.push({ crate: dep[1], git: dep[2] })
      }
      tables.set(source, names)
    }
    // Every optional path dependency of rs/Cargo.toml, under the lsp table,
    // from its own repository; nothing else is optional there.
    const rsToml = fs.readFileSync(path.join(REPO, 'rs', 'Cargo.toml'), 'utf8')
    const optional = [...rsToml.matchAll(
      /^(tabnas-[a-z0-9]+) = \{ path = "\.\.\/\.\.\/([a-z0-9]+)\/rs", optional = true \}/gm)]
      .map((m) => ({ crate: m[1], short: m[2] }))
    assert.ok(12 < optional.length, 'rs/Cargo.toml optional siblings: ' + optional.length)
    const lsp = tables.get('https://github.com/tabnas/lsp')
    assert.ok(lsp, 'no lsp table:\n' + cargo)
    assert.deepStrictEqual(lsp[0], { crate: 'tabnas', git: 'https://github.com/tabnas/parser' })
    for (const { crate, short } of optional) {
      assert.ok(lsp.some((d) => d.crate === crate && d.git === 'https://github.com/tabnas/' + short),
        crate + ' missing from the lsp table:\n' + cargo)
    }
    assert.equal(lsp.length, 1 + optional.length, 'the lsp table names more than rs/Cargo.toml:\n' + cargo)
    // Each sibling has a table naming the engine and its own siblings;
    // checked against the sibling checkouts where the fleet layout has
    // them, and against the generator's map otherwise.
    let checked = 0
    for (const [source, names] of tables) {
      if (source === 'https://github.com/tabnas/lsp') continue
      const short = source.slice('https://github.com/tabnas/'.length)
      assert.deepStrictEqual(names[0], { crate: 'tabnas', git: 'https://github.com/tabnas/parser' }, source)
      const manifest = path.join(REPO, '..', short, 'rs', 'Cargo.toml')
      if (!fs.existsSync(manifest)) continue
      const deps = fs.readFileSync(manifest, 'utf8').split(/^\[/m)
        .find((section) => section.startsWith('dependencies]')) || ''
      const wanted = [...deps.matchAll(/^(tabnas(?:-[a-z0-9]+)?) = \{ path = "\.\.\/\.\.\/([a-z0-9]+)\/rs"/gm)]
        .map((m) => m[1]).sort()
      assert.deepStrictEqual(names.map((d) => d.crate).sort(), wanted,
        short + ': the generator\'s sibling map drifted from ' + manifest)
      checked++
    }
    if (0 === checked) t.diagnostic('no sibling checkouts beside this one: the sibling map was not checked against them')
    // A sibling taken from a path needs no table, and neither does what
    // only it names.
    const local = tmp('gen-rssibl-')
    generate({
      out: local, input: { spec: SPEC_FILE }, languageId: 'mydsl',
      runtime: 'rust', editors: [], rustPath: ['tabnas-ini=../ini/rs'],
    })
    const localCargo = cargoToml(local)
    assert.ok(!localCargo.includes('[patch."https://github.com/tabnas/ini"]'), localCargo)
    assert.ok(!localCargo.includes('[patch."https://github.com/tabnas/hoover"]'), localCargo)
    assert.ok(localCargo.includes('[patch."https://github.com/tabnas/jsonic"]'), localCargo)
  })

  it('refuses a TS module for the Rust runtime with the fix named', () => {
    assert.throws(
      () => generate({
        out: tmp('gen-rsmod-'), input: { module: '@tabnas/toml' },
        extensions: ['.toml'], runtime: 'rust',
      }),
      /--rust-plugin <crate>/)
  })

  it('an entry whose grammar is live code needs its crate', () => {
    assert.throws(
      () => generate({
        out: tmp('gen-rslive-'), input: { entry: 'toml' },
        runtime: 'rust', editors: [],
      }),
      /live code, which a Rust server cannot load from npm\. Pass --rust-plugin <crate>/)
  })

  it('plugin-linked entries: the crate, its layers, and the metadata', () => {
    const regDir = tmp('gen-rsreg-')
    const REG = path.join(regDir, 'registry.json')
    fs.writeFileSync(REG, JSON.stringify({
      entries: [
        {
          name: '@tabnas/foo-lang', languageId: 'foo-lang',
          extensions: ['.fl'], grammarKind: 'closure',
          base: '@tabnas/jsonic',
          syncGroups: ['end', 'comma'],
          semanticTokens: { '#KEY': 'property' },
        },
        { name: '@tabnas/jsonic', base: '@tabnas/json' },
        { name: '@tabnas/json', base: null },
      ],
    }))
    const gen = (extra) => {
      const out = tmp('gen-rsent-')
      generate(Object.assign({
        out, input: { entry: 'foo-lang' }, runtime: 'rust', editors: [],
        registryFile: REG, rustPlugin: 'tabnas-foolang',
      }, extra))
      return { main: mainRs(out), cargo: cargoToml(out) }
    }

    const { main, cargo } = gen({})
    // The grammar is linked, under the entry's own name.
    assert.ok(main.includes('loader.link("foo-lang", Arc::new(tabnas_foolang::make));'), main)
    // Registry recovery/highlighting metadata reaches the Rust entry, as
    // it reaches the Go one.
    assert.ok(main.includes(
      'entry.sync_groups = Some(vec!["end".to_string(), "comma".to_string()]);'), main)
    // (rustfmt breaks a lone tuple across lines; compare without layout.)
    assert.ok(main.replace(/\s+/g, '').replace(/,\)/g, ')').includes(
      'entry.semantic_tokens=Some(std::collections::HashMap::from([' +
        '("#KEY".to_string(),"property".to_string())]));'), main)
    assert.ok(main.includes('entry.extensions = vec![".fl".to_string()];'), main)
    // The crate comes from the fleet's repository by name, and the
    // grammars it is layered on (the registry's base chain) are
    // supplied to it, each with a table of its own for the next.
    assert.ok(cargo.includes(
      'tabnas-foolang = { git = "https://github.com/tabnas/foolang" }'), cargo)
    assert.ok(cargo.includes('[patch."https://github.com/tabnas/foolang"]\n' +
      'tabnas = { git = "https://github.com/tabnas/parser" }\n' +
      'tabnas-jsonic = { git = "https://github.com/tabnas/jsonic" }'), cargo)
    assert.ok(cargo.includes('[patch."https://github.com/tabnas/jsonic"]\n' +
      'tabnas = { git = "https://github.com/tabnas/parser" }\n' +
      'tabnas-json = { git = "https://github.com/tabnas/json" }'), cargo)
    assert.ok(cargo.includes('[patch."https://github.com/tabnas/json"]\n' +
      'tabnas = { git = "https://github.com/tabnas/parser" }\n'), cargo)
    assert.ok(!/\brev = /.test(cargo), cargo)

    const pinned = gen({ rustPluginRev: '89abcde', rustPluginFn: 'make_json' })
    assert.ok(pinned.cargo.includes(
      'tabnas-foolang = { git = "https://github.com/tabnas/foolang", rev = "89abcde" }'),
    pinned.cargo)
    assert.ok(pinned.main.includes('Arc::new(tabnas_foolang::make_json)'), pinned.main)

    // A local checkout resolves its own siblings by path, so it needs no
    // patch table. The layer under it keeps its table here, because
    // tabnas-lsp, still from git, names jsonic as a fleet sibling too.
    const local = gen({ rustPath: ['tabnas-foolang=../foolang/rs'] })
    assert.ok(local.cargo.includes('tabnas-foolang = { path = "../foolang/rs" }'), local.cargo)
    assert.ok(!local.cargo.includes('[patch."https://github.com/tabnas/foolang"]'), local.cargo)
    assert.ok(local.cargo.includes('[patch."https://github.com/tabnas/jsonic"]'), local.cargo)
    // With tabnas-lsp from a path as well, nothing needs a table.
    const allLocal = gen({ rustPath: ['tabnas-foolang=../foolang/rs', 'tabnas-lsp=' + RS] })
    assert.ok(!allLocal.cargo.includes('[patch.'), allLocal.cargo)

    // Outside the fleet's naming, the repository must be given.
    assert.throws(() => gen({ rustPlugin: 'foolang' }), /--rust-plugin-git/)
    const own = gen({ rustPlugin: 'foolang', rustPluginGit: 'https://example.com/foolang' })
    assert.ok(own.cargo.includes('foolang = { git = "https://example.com/foolang" }'), own.cargo)
    assert.ok(own.main.includes('Arc::new(foolang::make)'), own.main)
  })

  it('a BNF grammar is compiled at generation time and embedded, through the firewall', () => {
    const dir = tmp('gen-rsbnf-')
    const grammarFile = path.join(dir, 'mydsl.abnf')
    fs.writeFileSync(grammarFile, 'top = 1*DIGIT\n')
    const spec = JSON.parse(fs.readFileSync(SPEC_FILE, 'utf8'))
    const compiler = (compiled) => (name) => '@tabnas/abnf' === name
      ? { abnfConvert: () => compiled }
      : require(name)

    const out = path.join(dir, 'out')
    const res = generate({
      out, input: { grammar: grammarFile }, runtime: 'rust', editors: [],
      requireFn: compiler(spec),
    })
    assert.ok(res.files.includes('server/grammar.json'), res.files)
    assert.ok(!res.files.includes('server/mydsl.abnf'),
      'the grammar text is compiled, not shipped')
    assert.deepStrictEqual(
      JSON.parse(fs.readFileSync(path.join(out, 'server', 'grammar.json'), 'utf8')), spec)
    assert.ok(mainRs(out).includes(
      'entry_from_spec_json("mydsl", &[".mydsl"], GRAMMAR)'))
    // Only tabnas-lsp is depended on: the dialect compiler ran here, not
    // in the server (the patch tables below the dependencies name the
    // dialect crates because tabnas-lsp's manifest does, feature or not).
    const deps = cargoToml(out).split('\n[patch.')[0]
    assert.ok(!/abnf|bnf/.test(deps), deps)

    assert.throws(
      () => generate({
        out: path.join(dir, 'bad'), input: { grammar: grammarFile },
        runtime: 'rust', editors: [],
        requireFn: compiler({ rule: { top: { open: [{ s: '#NR', a: '@evil' }] } } }),
      }),
      (e) => e instanceof GenerateError && /compiled grammar failed the firewall/.test(e.message))
  })

  it('names cargo cannot build are refused, and strings are Rust-escaped', () => {
    const gen = (extra) => {
      const out = tmp('gen-rsname-')
      generate(Object.assign({
        out, input: { spec: SPEC_FILE }, runtime: 'rust', editors: [],
      }, extra))
      return { main: mainRs(out), cargo: cargoToml(out) }
    }
    // A package name cannot start with a digit; the binary keeps the id.
    assert.throws(() => gen({ languageId: '9lang' }), /--rust-crate/)
    const named = gen({ languageId: '9lang', rustCrate: 'lang9-lsp' })
    assert.ok(named.cargo.includes('name = "lang9-lsp"'), named.cargo)
    assert.ok(named.cargo.includes('[[bin]]\nname = "9lang-lsp"'), named.cargo)
    // The binary is the command editors launch: a plain file name, and
    // one cargo takes as a target name (it refuses a `.` or a `+`).
    assert.throws(() => gen({ languageId: 'my dsl' }), /--language-id/)
    assert.throws(() => gen({ languageId: 'a/b' }), /--language-id/)
    assert.throws(() => gen({ languageId: 'c++' }), /--language-id/)
    assert.throws(() => gen({ languageId: 'a.b' }), /--language-id/)
    assert.ok(gen({ languageId: 'c_v2-x' }).cargo.includes('[[bin]]\nname = "c_v2-x-lsp"'))
    // Unknown crates and malformed overrides name what is accepted.
    assert.throws(() => gen({ languageId: 'x', rustPath: ['nope=/x'] }),
      /--rust-path 'nope=\/x'.*tabnas-lsp, tabnas/)
    assert.throws(() => gen({ languageId: 'x', rustPath: ['tabnas-lsp'] }), /--rust-path/)
    // Extensions reach Rust source as Rust literals, not JSON ones.
    const esc = gen({ languageId: 'x', extensions: ['.a"b', '.c\\d', '.e\u0001'] })
    assert.ok(esc.main.includes('&[".a\\"b", ".c\\\\d", ".e\\u{1}"]'), esc.main)
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
    // Per-extension setting key: two installed branded extensions must
    // not fight over one shared tabnas.serverPath.
    assert.equal(
      pkg.contributes.configuration.properties['tabnas.mydsl.serverPath'].default,
      'mydsl-lsp')
    const ext = fs.readFileSync(path.join(out, 'editors', 'vscode', 'extension.js'), 'utf8')
    assert.ok(ext.includes("'mydsl.serverPath'") || ext.includes('"mydsl.serverPath"'), ext)
  })

  it('a manifest cannot delete outside the output directory', () => {
    // The manifest is a file on disk, so it is INPUT: hand-edited,
    // badly merged, or written by an older version. An entry like
    // '../precious/keep.txt' used to be unlinked, and the prune loop —
    // which stopped only on exact equality with `out` — then climbed
    // past `out`, rmdir'ing ancestors until one was non-empty.
    const root = tmp('gen-esc-')
    const out = path.join(root, 'out')
    const precious = path.join(root, 'precious')
    fs.mkdirSync(out, { recursive: true })
    fs.mkdirSync(precious, { recursive: true })
    const keep = path.join(precious, 'keep.txt')
    fs.writeFileSync(keep, 'do not delete me')
    fs.writeFileSync(path.join(out, '.tabnas-lsp-gen.json'), JSON.stringify({
      generated: 'tabnas-lsp-gen',
      files: ['../precious/keep.txt', path.join(root, 'precious', 'keep.txt'), 42, null],
    }))

    generate({ out, input: { spec: SPEC_FILE }, languageId: 'mydsl', editors: [] })

    assert.ok(fs.existsSync(keep), 'a manifest entry escaped the output directory')
    assert.ok(fs.existsSync(precious), 'the prune loop climbed out of the output directory')
  })

  it('supplied engine versions are pinned in the generated go.mod', () => {
    // Omitting requires keeps the module BUILDABLE (a guessed version
    // 404s and tidy fails outright), but it is not reproducible: tidy
    // resolves from whatever the proxy serves when it runs, so the same
    // output can build against a different engine later and parse
    // differently. A caller that knows the versions — the release wave
    // does — can pin them.
    const out = tmp('gen-gopin-')
    generate({
      out,
      input: { spec: SPEC_FILE },
      languageId: 'mydsl',
      runtime: 'go',
      editors: [],
      goLspVersion: 'v0.3.1',
      goParserVersion: 'v0.9.4',
    })
    const mod = fs.readFileSync(path.join(out, 'server', 'go.mod'), 'utf8')
    assert.match(mod, /require github\.com\/tabnas\/lsp\/go v0\.3\.1/)
    assert.match(mod, /require github\.com\/tabnas\/parser\/go v0\.9\.4/)
    assert.match(mod, /pinned/)

    // ...and the default still names no version at all.
    const bare = tmp('gen-gobare-')
    generate({
      out: bare,
      input: { spec: SPEC_FILE },
      languageId: 'mydsl',
      runtime: 'go',
      editors: [],
    })
    const bareMod = fs.readFileSync(path.join(bare, 'server', 'go.mod'), 'utf8')
    assert.equal(/^require /m.test(bareMod), false, bareMod)
  })

  it('a manifest cannot delete through a symlinked directory', () => {
    // Lexical containment is not enough. path.resolve normalises `..`
    // and nothing else, so with `out/link` pointing outside the tree an
    // entry like `link/victim` passed every string test while
    // unlinkSync followed `link` straight out. Generated output is
    // routinely a checked-out project, which is exactly where an
    // attacker-supplied symlink comes from.
    const root = tmp('gen-symlink-')
    const out = path.join(root, 'out')
    const outside = path.join(root, 'outside')
    fs.mkdirSync(out, { recursive: true })
    fs.mkdirSync(outside, { recursive: true })
    const victim = path.join(outside, 'victim.txt')
    fs.writeFileSync(victim, 'do not delete me')

    try {
      fs.symlinkSync(outside, path.join(out, 'link'), 'dir')
    } catch (e) {
      return // no symlink privilege (unprivileged win32) — nothing to assert
    }

    fs.writeFileSync(path.join(out, '.tabnas-lsp-gen.json'), JSON.stringify({
      generated: 'tabnas-lsp-gen',
      files: ['link/victim.txt'],
    }))

    generate({ out, input: { spec: SPEC_FILE }, languageId: 'mydsl', editors: [] })

    assert.ok(fs.existsSync(victim),
      'a manifest entry deleted through a symlinked directory')
    assert.ok(fs.existsSync(outside),
      'the prune loop climbed out through a symlinked directory')
  })

  it('the generated go.mod names no unpublished version', () => {
    // Every hardcoded version here was a guess, and both guesses were
    // wrong: github.com/tabnas/lsp/go v0.1.0 and parser/go v0.9.0 have
    // never been published, so `go mod tidy` — the documented first
    // step — failed outright and every generated Go server was dead on
    // arrival. Requirements come from `go mod tidy` over main.go's
    // imports instead.
    const out = tmp('gen-gomod-')
    generate({
      out, input: { spec: SPEC_FILE }, languageId: 'mydsl',
      runtime: 'go', editors: [],
    })
    const goMod = fs.readFileSync(path.join(out, 'server', 'go.mod'), 'utf8')
    assert.ok(!/^\s*require\s+github\.com\/tabnas\/(lsp|parser)\/go\s/m.test(goMod),
      'go.mod pins a fleet module version the generator only guessed:\n' + goMod)
    assert.ok(!goMod.includes('v0.9.0') && !goMod.includes('v0.1.0'), goMod)
    // main.go still imports them, which is what tidy resolves from.
    const mainGo = fs.readFileSync(path.join(out, 'server', 'main.go'), 'utf8')
    assert.ok(mainGo.includes('github.com/tabnas/lsp/go'))
  })

  it('regeneration deletes files the previous run owned', () => {
    // Overwrite-only regeneration leaves a removed language's files in
    // place — stale artifacts that still ship, and a staleness gate
    // that can never pass. The manifest is what makes deletion safe:
    // only files a previous run listed are ever removed.
    const regDir = tmp('gen-reg2-')
    const REG_AB = path.join(regDir, 'ab.json')
    const REG_A = path.join(regDir, 'a.json')
    fs.writeFileSync(REG_AB, JSON.stringify({
      entries: [
        { name: '@tabnas/jsonic', languageId: 'jsonic', extensions: ['.jsonic'] },
        { name: '@tabnas/zon', languageId: 'zon', extensions: ['.zon'] },
      ],
    }))
    fs.writeFileSync(REG_A, JSON.stringify({
      entries: [
        { name: '@tabnas/jsonic', languageId: 'jsonic', extensions: ['.jsonic'] },
      ],
    }))
    const out = tmp('gen-clean-')
    generate({ out, unified: true, registryFile: REG_AB, editors: ['zed', 'helix'] })
    assert.ok(fs.existsSync(path.join(out, 'zed', 'languages', 'zon', 'config.toml')))

    generate({ out, unified: true, registryFile: REG_A, editors: ['zed', 'helix'] })
    assert.ok(!fs.existsSync(path.join(out, 'zed', 'languages', 'zon')),
      'stale zon language dir survived regeneration')
    const manifest = JSON.parse(fs.readFileSync(
      path.join(out, '.tabnas-lsp-gen.json'), 'utf8'))
    assert.ok(!manifest.files.some((f) => f.includes('zon')))
    const helix = fs.readFileSync(path.join(out, 'helix', 'languages.toml'), 'utf8')
    assert.ok(!helix.includes('zon'))
  })
})
