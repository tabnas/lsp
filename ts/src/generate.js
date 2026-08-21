/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// The language-server generator (design §7): given ONE grammar — a
// fleet registry entry, a plugin module, a serialized GrammarSpec, or
// BNF-dialect text — emit a standalone single-language server plus the
// editor plugins needed to use it. The server runtime follows where
// the grammar can execute:
//
//   input \ runtime |  node                     |  go
//   ----------------+---------------------------+--------------------------
//   entry           |  static reg. over this pkg|  pure-data: embed spec
//   module (npm)    |  package + module dep     |  ✗ (pass --go-plugin)
//   spec (JSON)     |  embed + L2 load          |  go:embed, engine-only dep
//   grammar (BNF)   |  compile at server start  |  pre-compile, then embed
//
// Emitted wrappers contain no pipeline logic: they pin WHAT is served,
// not HOW — a fix in this package reaches every generated Node server
// on update, and the Go module on rebuild.
//
// `--unified` instead generates the multi-language editor plugins for
// the whole bundled registry over `tabnas-lsp` itself (this is how the
// repo's own VS Code extension is built rather than hand-maintained).

const fs = require('fs')
const path = require('path')

const { loadBundled, normalize } = require('./registry')
const { firewallSpec, compileGrammarText, LoadError, DIALECTS } = require('./loaders')

const ALL_EDITORS = ['vscode', 'nvim', 'emacs', 'sublime', 'helix', 'kate', 'zed']

// Manifest of generator-owned files, written into every output: it is
// what lets a REgeneration delete files the previous run produced that
// this run did not (a removed language's Zed dir, a dropped editor) —
// overwrite-only regeneration leaves stale artifacts that still ship.
const MANIFEST = '.tabnas-lsp-gen.json'

class GenerateError extends Error {
  constructor(message) {
    super(message)
    this.name = 'GenerateError'
  }
}

// An exported Go identifier from a language id: ids commonly contain
// hyphens ('foo-lang'), which a naive capitalize turns into invalid
// source ('Foo-lang').
function goIdent(id) {
  const ident = String(id).split(/[^A-Za-z0-9]+/)
    .filter((s) => 0 < s.length)
    .map((s) => s.charAt(0).toUpperCase() + s.slice(1))
    .join('')
  return /^[A-Za-z]/.test(ident) ? ident : null
}

// The exact installed version of a dependency, so generated servers
// freeze what they were generated against; `fallback` when the package
// is not resolvable at generation time. Two probes: the package.json
// subpath (blocked by an `exports` map that does not list it — the
// engine's does not), then the fleet's exported VERSION const, which
// every tabnas package carries and version-tests against package.json.
function depVersion(req, name, fallback) {
  try {
    const pkg = req(name + '/package.json')
    if (pkg && 'string' === typeof pkg.version) return pkg.version
  } catch (e) {
    // exports-mapped or not installed — try the VERSION const
  }
  try {
    const mod = req(name)
    if (mod && 'string' === typeof mod.VERSION) return mod.VERSION
  } catch (e) {
    // not installed here — the fallback range documents the intent
  }
  return fallback
}

// generate(opts) -> { files: [relpath...], out }
//
// opts:
//   out         target directory (created; must be empty or absent
//               unless force)
//   input       { entry } | { module } | { spec } | { grammar }
//               (file paths for spec/grammar)
//   languageId  served language id (defaulted from the input where
//               derivable)
//   extensions  ['.x', ...]
//   runtime     'node' | 'go'   (default 'node')
//   editors     subset of vscode,nvim,emacs,sublime,helix,kate,zed;
//               [] for none (default: all)
//   unified     editor plugins for the whole bundled registry
//   goModule    module path for the generated go.mod
//   goPlugin    Go import path of the plugin package (closure/
//               imperative grammars)
//   goPluginFunc exported plugin func name (default: CamelCase id)
//   goReplace   ['mod=../dir', ...] replace directives for local dev
//   registryFile bundled registry override (tests)
//   requireFn   module resolver override (tests)
function generate(opts) {
  const out = opts.out
  if (!out) throw new GenerateError('out directory required')
  const runtime = opts.runtime || 'node'
  if ('node' !== runtime && 'go' !== runtime) {
    throw new GenerateError(
      "unknown runtime '" + runtime + "': node and go exist today; other " +
        'engine ports arrive via the pure-GrammarSpec lane and the C ABI ' +
        '(design §7.4)')
  }
  const editors = null == opts.editors ? ALL_EDITORS : opts.editors
  for (const e of editors) {
    if (!ALL_EDITORS.includes(e)) {
      throw new GenerateError(
        "unknown editor '" + e + "' (known: " + ALL_EDITORS.join(', ') + ')')
    }
  }

  const files = new Map() // relpath -> content

  if (opts.unified) {
    generateUnified(opts, editors, files)
  } else {
    const lang = resolveInput(opts)
    if ('node' === runtime) emitNodeServer(lang, files, opts.requireFn || require)
    else emitGoServer(lang, opts, files)
    // Editors launch servers from their own working directory, so the
    // command is the PATH name for both runtimes — the README says how
    // to put the built binary there; vscode's serverPath setting takes
    // an absolute path override.
    const bin = lang.id + '-lsp'
    emitEditors(editors, [lang], bin, files, 'editors/')
    files.set('README.md', readmeSingle(lang, runtime, editors))
  }

  writeFiles(out, files)
  return { files: [...files.keys()].sort(), out }
}

// ---------------------------------------------------------------------
// Input resolution: everything becomes { id, extensions, kind, ... }.

function resolveInput(opts) {
  const input = opts.input || {}
  const req = opts.requireFn || require

  if (input.entry) {
    const entries = loadBundled(opts.registryFile)
    const entry = entries.find((e) => e.languageId === input.entry)
    if (!entry) {
      throw new GenerateError(
        "no bundled registry entry for '" + input.entry + "' (known: " +
          entries.map((e) => e.languageId).join(', ') + ')')
    }
    return {
      id: entry.languageId,
      extensions: opts.extensions || entry.extensions,
      kind: 'entry',
      entry,
    }
  }

  if (input.module) {
    const id = opts.languageId ||
      String(input.module).replace(/^@[^/]+\//, '').replace(/[^\w-]/g, '')
    return {
      id,
      extensions: needExtensions(opts, id),
      kind: 'module',
      module: input.module,
    }
  }

  if (input.spec) {
    const specText = fs.readFileSync(input.spec, 'utf8')
    const spec = JSON.parse(specText)
    // Generation-time firewall: a generator that emits a server around
    // a poisoned grammar is just a slower way to load it (design §7.2).
    const parserMod = req('@tabnas/parser')
    const issues = firewallSpec(spec, parserMod)
    if (0 < issues.length) {
      throw new GenerateError(
        'spec failed the firewall:\n  ' +
          issues.map((i) => i.path + ': ' + i.message).join('\n  '))
    }
    const id = opts.languageId ||
      path.basename(input.spec).replace(/\.[^.]*$/, '').replace(/[^\w-]/g, '')
    return {
      id,
      extensions: needExtensions(opts, id),
      kind: 'spec',
      specText: JSON.stringify(spec, null, 1) + '\n',
    }
  }

  if (input.grammar) {
    const ext = path.extname(input.grammar).toLowerCase()
    if (!DIALECTS[ext]) {
      throw new GenerateError(
        "unknown grammar dialect '" + ext + "' (known: " +
          Object.keys(DIALECTS).join(', ') + ')')
    }
    const id = opts.languageId ||
      path.basename(input.grammar).replace(/\.[^.]*$/, '').replace(/[^\w-]/g, '')
    return {
      id,
      extensions: needExtensions(opts, id),
      kind: 'grammar',
      grammarFile: input.grammar,
      grammarText: fs.readFileSync(input.grammar, 'utf8'),
      dialect: ext.slice(1),
      dialectPkg: DIALECTS[ext],
    }
  }

  throw new GenerateError(
    'input required: one of { entry }, { module }, { spec }, { grammar }')
}

function needExtensions(opts, id) {
  if (opts.extensions && 0 < opts.extensions.length) return opts.extensions
  return ['.' + id]
}

// ---------------------------------------------------------------------
// Node server package.

function emitNodeServer(lang, files, req) {
  // Exact versions where resolvable: a generated server freezes
  // grammar + engine behavior (design §7.1), and a range lets a later
  // npm install move it without regeneration.
  const deps = {
    '@tabnas/lsp': depVersion(req, '@tabnas/lsp',
      require('../package.json').version),
    // Fleet convention (admin/publish.sh): every @tabnas peer/floor is
    // ">=0" so installs resolve the latest published engine. Only an
    // exact version we RESOLVED is worth pinning; a typed floor is a
    // guess, and ">=0.9.0" was one that no published parser satisfied.
    '@tabnas/parser': depVersion(req, '@tabnas/parser', '>=0'),
  }
  const entry = {
    name: lang.id,
    languageId: lang.id,
    extensions: lang.extensions,
    enabled: true,
  }
  let dataFile = null

  if ('entry' === lang.kind) {
    Object.assign(entry, {
      name: lang.entry.name,
      base: lang.entry.base,
      pluginKind: lang.entry.pluginKind,
      grammarKind: lang.entry.grammarKind,
      lexStream: lang.entry.lexStream,
      syncGroups: lang.entry.syncGroups,
      semanticTokens: lang.entry.semanticTokens,
      // A branded server serves its language by construction — the
      // registry's editor-collision default applies to the UNIFIED
      // server, not to a server someone generated for exactly this
      // language.
      enabled: true,
      load: lang.entry.load,
      options: lang.entry.options,
      stack: lang.entry.stack,
    })
    deps[lang.entry.name] = depVersion(req, lang.entry.name, '*')
    if (lang.entry.base && 'grammar' === lang.entry.pluginKind) {
      deps[lang.entry.base] = depVersion(req, lang.entry.base, '*')
    }
  } else if ('module' === lang.kind) {
    entry.name = lang.module
    entry.load = { module: lang.module }
    deps[lang.module] = depVersion(req, lang.module, '*')
  } else if ('spec' === lang.kind) {
    entry.grammarKind = 'data'
    entry.load = { spec: 'grammar.json' }
    dataFile = ['server/grammar.json', lang.specText]
  } else if ('grammar' === lang.kind) {
    entry.grammarKind = 'compiled'
    entry.load = { grammar: path.basename(lang.grammarFile) }
    deps[lang.dialectPkg] = depVersion(req, lang.dialectPkg, '*')
    dataFile = ['server/' + path.basename(lang.grammarFile), lang.grammarText]
  }

  files.set('server/package.json', JSON.stringify({
    name: lang.id + '-lsp',
    version: '0.1.0',
    description: 'Language server for ' + lang.id + ' (generated by tabnas-lsp-gen)',
    license: 'MIT',
    bin: { [lang.id + '-lsp']: 'server.js' },
    dependencies: deps,
  }, null, 2) + '\n')

  files.set('server/server.js', [
    '#!/usr/bin/env node',
    "/* Generated by tabnas-lsp-gen. The pipeline lives in @tabnas/lsp;",
    ' * this wrapper only pins what is served. Regenerate rather than',
    ' * grow it. */',
    "'use strict'",
    '',
    "const { startServer } = require('@tabnas/lsp/src/server')",
    '',
    'const entry = ' + JSON.stringify(entry, null, 2),
    '',
    '// Grammar files resolve against this package, sandboxed.',
    'entry._dir = __dirname',
    '',
    'startServer({ entries: [entry] })',
    '',
  ].join('\n'))

  if (dataFile) files.set(dataFile[0], dataFile[1])
}

// ---------------------------------------------------------------------
// Go server module.

// Registry metadata that must survive into the generated Go Entry:
// dropping SyncGroups changes where recovery resynchronizes, dropping
// the semantic-token map or outline rules changes what the editor
// shows — the generated server would quietly disagree with the
// canonical TS server about the same grammar.
function goEntryMeta(entry) {
  const goStringMap = (m) => 'map[string]string{' +
    Object.keys(m).sort().map((k) =>
      JSON.stringify(k) + ': ' + JSON.stringify(m[k])).join(', ') + '}'
  const meta = []
  if (entry.syncGroups && 0 < entry.syncGroups.length) {
    meta.push(['SyncGroups', '[]string{' +
      entry.syncGroups.map((s) => JSON.stringify(s)).join(', ') + '}'])
  }
  if (entry.lexStream && 'clean' !== entry.lexStream) {
    meta.push(['LexStream', JSON.stringify(entry.lexStream)])
  }
  if (entry.semanticTokens && 0 < Object.keys(entry.semanticTokens).length) {
    meta.push(['SemanticTokens', goStringMap(entry.semanticTokens)])
  }
  if (entry.outlineRules && 0 < Object.keys(entry.outlineRules).length) {
    meta.push(['OutlineRules', goStringMap(entry.outlineRules)])
  }
  return meta
}

function emitGoServer(lang, opts, files) {
  if ('module' === lang.kind) {
    throw new GenerateError(
      'a TypeScript plugin module cannot run in a Go server. For a ' +
        'grammar that lives in Go, pass --go-plugin <import-path>; for a ' +
        'pure-data grammar, pass --spec.')
  }

  const goModule = opts.goModule || 'example.com/' + lang.id + '-lsp'

  // NO hardcoded requires. A version written here is a guess about what
  // is published, and a guess that is wrong makes the generated module
  // unbuildable: `go mod tidy` fails outright on a require the proxy
  // 404s. This generator emitted `github.com/tabnas/lsp/go v0.1.0` and
  // `github.com/tabnas/parser/go v0.9.0`, and NEITHER has ever been
  // published — every generated Go server was dead on arrival.
  //
  // `go mod tidy` resolves both from the imports in main.go, which is
  // the only shape that self-heals as the fleet releases. The same rule
  // already governs the plugin module below; it now governs all three.
  // A version the CALLER supplies is not a guess, so it is pinned. This
  // is how a generated module becomes reproducible: with nothing pinned,
  // `go mod tidy` resolves from whatever the proxy serves at the moment
  // it runs, so the same generator output can build against a different
  // engine next month and quietly parse differently — which is the
  // opposite of what design.md §7.1 promises. Omitting the requires is
  // still the DEFAULT, because the alternative this code shipped with
  // was a guessed version that 404'd and made every generated server
  // unbuildable. Pinned when known, floating when not, and the emitted
  // comment says which of the two happened.
  const pins = [
    [opts.goPlugin, opts.goPluginVersion],
    ['github.com/tabnas/lsp/go', opts.goLspVersion],
    ['github.com/tabnas/parser/go', opts.goParserVersion],
  ].filter(([mod, ver]) => mod && ver)

  const requires = []
  for (const [mod, ver] of pins) {
    // The import path is preserved exactly, /vN suffixes included.
    requires.push('require ' + mod + ' ' + ver)
  }
  if (0 < pins.length) requires.push('')

  requires.push(
    0 < pins.length
      ? '// The requirements above are pinned; anything else is resolved'
      : '// Requirements are resolved by `go mod tidy` from the imports in',
    0 < pins.length
      ? '// by `go mod tidy` from the imports in main.go — run it before'
      : '// main.go — run it before the first build (the README says so).',
    0 < pins.length
      ? '// the first build (the README says so).'
      : '// Pass --go-lsp-version/--go-parser-version to pin them instead.',
    '// For unreleased local checkouts, add replace directives (or pass',
    '// --go-replace to the generator):',
  )
  for (const r of opts.goReplace || []) {
    const [mod, dir] = String(r).split('=')
    requires.push('replace ' + mod + ' => ' + dir)
  }
  if (0 === (opts.goReplace || []).length) {
    requires.push('// replace github.com/tabnas/lsp/go => ../lsp/go')
  }

  files.set('server/go.mod', [
    'module ' + goModule,
    '',
    'go 1.24',
    '',
    ...requires,
    '',
  ].join('\n'))

  const metaLines = lang.entry ? goEntryMeta(lang.entry) : []

  let spec = null
  if ('spec' === lang.kind) {
    spec = lang.specText
  } else if ('grammar' === lang.kind) {
    // Pre-compile BNF to a pure spec at generation time (the C-ABI
    // precedent: pre-compile -> L2), so the Go module depends only on
    // the engine.
    const req = opts.requireFn || require
    const compiled = compileGrammarText(req, lang.grammarFile, lang.grammarText)
    const parserMod = req('@tabnas/parser')
    const issues = firewallSpec(compiled, parserMod)
    if (0 < issues.length) {
      throw new GenerateError(
        'compiled grammar failed the firewall:\n  ' +
          issues.map((i) => i.path + ': ' + i.message).join('\n  '))
    }
    spec = JSON.stringify(compiled, null, 1) + '\n'
  } else if ('entry' === lang.kind) {
    if (opts.goPlugin) {
      spec = null // plugin-linked below
    } else if ('data' === lang.entry.grammarKind && lang.entry.load &&
      lang.entry.load.spec && 'string' !== typeof lang.entry.load.spec) {
      spec = JSON.stringify(lang.entry.load.spec, null, 1) + '\n'
    } else {
      throw new GenerateError(
        "entry '" + lang.id + "' is grammarKind: " + lang.entry.grammarKind +
          ' — its grammar is live code, which a Go server cannot load ' +
          'from npm. Pass --go-plugin <import-path> naming its Go twin, ' +
          'or --spec with a pure-data grammar.')
    }
  }

  if (null != spec) {
    files.set('server/grammar.json', spec)
    files.set('server/main.go', [
      '// Generated by tabnas-lsp-gen. The pipeline lives in',
      '// github.com/tabnas/lsp/go; this wrapper only pins what is served.',
      'package main',
      '',
      'import (',
      '\t_ "embed"',
      '\t"log"',
      '',
      '\tlsp "github.com/tabnas/lsp/go"',
      ')',
      '',
      '//go:embed grammar.json',
      'var grammarJSON []byte',
      '',
      'func main() {',
      '\tentry, makeInstance := lsp.EntryFromSpecJSON(',
      '\t\t' + JSON.stringify(lang.id) + ',',
      '\t\t[]string{' + lang.extensions.map((e) => JSON.stringify(e)).join(', ') + '},',
      '\t\tgrammarJSON,',
      '\t)',
      ...metaLines.map(([f, v]) => '\tentry.' + f + ' = ' + v),
      '\tif err := lsp.Serve(lsp.Config{',
      '\t\tEntries:      []*lsp.Entry{entry},',
      '\t\tMakeInstance: makeInstance,',
      '\t}); err != nil {',
      '\t\tlog.Fatal(err)',
      '\t}',
      '}',
      '',
    ].join('\n'))
  } else {
    const fn = opts.goPluginFunc || goIdent(lang.id)
    if (null == fn) {
      throw new GenerateError(
        "cannot derive a Go plugin function name from '" + lang.id +
          "' — pass --go-plugin-func")
    }
    files.set('server/main.go', [
      '// Generated by tabnas-lsp-gen. The pipeline lives in',
      '// github.com/tabnas/lsp/go; this wrapper only pins what is served.',
      'package main',
      '',
      'import (',
      '\t"log"',
      '',
      '\tlsp "github.com/tabnas/lsp/go"',
      '\ttabnas "github.com/tabnas/parser/go"',
      '\tplugin ' + JSON.stringify(opts.goPlugin),
      ')',
      '',
      'func main() {',
      '\tentry := &lsp.Entry{',
      '\t\tName:       ' + JSON.stringify(lang.id) + ',',
      '\t\tLanguageID: ' + JSON.stringify(lang.id) + ',',
      '\t\tExtensions: []string{' + lang.extensions.map((e) => JSON.stringify(e)).join(', ') + '},',
      '\t\tEnabled:    true,',
      ...metaLines.map(([f, v]) => '\t\t' + f + ': ' + v + ','),
      '\t}',
      '\tmakeInstance := func(e *lsp.Entry) (*tabnas.Tabnas, error) {',
      '\t\ttn := lsp.NewInstance(e)',
      '\t\tif err := tn.Use(plugin.' + fn + '); err != nil {',
      '\t\t\treturn nil, err',
      '\t\t}',
      '\t\treturn tn, nil',
      '\t}',
      '\tif err := lsp.Serve(lsp.Config{',
      '\t\tEntries:      []*lsp.Entry{entry},',
      '\t\tMakeInstance: makeInstance,',
      '\t}); err != nil {',
      '\t\tlog.Fatal(err)',
      '\t}',
      '}',
      '',
    ].join('\n'))
  }
}

// ---------------------------------------------------------------------
// Unified mode: editor plugins for the whole bundled registry over the
// tabnas-lsp bin itself.

function generateUnified(opts, editors, files) {
  const entries = loadBundled(opts.registryFile)
    .map(normalize)
    .filter((e) => e.enabled && 'modifier' !== e.pluginKind &&
      0 < e.extensions.length)
  const langs = entries.map((e) => ({ id: e.languageId, extensions: e.extensions }))
  // No server/ half in unified mode, so the editor dirs sit at the out
  // root (the repo's own editors/ is exactly this output).
  emitEditors(editors, langs, 'tabnas-lsp', files, '')
  files.set('README.md', readmeUnified(langs, editors))
}

// ---------------------------------------------------------------------
// Editor plugins. Everything derives from (languages, bin): languages
// with their extensions, and the server command. `prefix` places the
// per-editor dirs — 'editors/' beside a server/ half, '' at the out
// root in unified mode.

function emitEditors(editors, langs, bin, files, prefix) {
  const view = {
    set: (rel, content) => files.set(prefix + rel, content),
  }
  for (const editor of editors) {
    EDITOR_EMITTERS[editor](langs, bin, view)
  }
}

const EDITOR_EMITTERS = {
  vscode(langs, bin, files) {
    const single = 1 === langs.length
    const name = single ? langs[0].id + '-lsp-vscode' : 'tabnas-lsp-vscode'
    // Per-extension setting key: two installed branded extensions must
    // not share (and fight over) one `tabnas.serverPath`; the unified
    // extension keeps the plain key.
    const settingKey = single ? 'tabnas.' + langs[0].id + '.serverPath' : 'tabnas.serverPath'
    files.set('vscode/package.json', JSON.stringify({
      name,
      displayName: single ? langs[0].id + ' (tabnas)' : 'tabnas languages',
      description: (single
        ? 'Language support for ' + langs[0].id
        : 'Language support for tabnas grammars') +
        ' (generated by tabnas-lsp-gen)',
      version: '0.1.0',
      publisher: 'REPLACE-WITH-YOUR-PUBLISHER',
      license: 'MIT',
      engines: { vscode: '^1.85.0' },
      categories: ['Programming Languages'],
      main: './extension.js',
      activationEvents: langs.map((l) => 'onLanguage:' + l.id),
      contributes: {
        languages: langs.map((l) => ({
          id: l.id,
          extensions: l.extensions,
          aliases: [l.id],
          configuration: './language-configuration.json',
        })),
        configuration: {
          title: single ? langs[0].id : 'tabnas',
          properties: {
            [settingKey]: {
              type: 'string',
              default: bin,
              description: 'Command that starts the language server (--stdio is appended).',
            },
          },
        },
      },
      dependencies: { 'vscode-languageclient': '^9.0.0' },
    }, null, 2) + '\n')

    files.set('vscode/extension.js', [
      "/* Generated by tabnas-lsp-gen. */",
      "'use strict'",
      "const vscode = require('vscode')",
      "const { LanguageClient } = require('vscode-languageclient/node')",
      '',
      'let client',
      '',
      'function activate() {',
      "  const command = vscode.workspace.getConfiguration('tabnas')",
      '    .get(' + JSON.stringify(settingKey.replace(/^tabnas\./, '')) + ') || ' + JSON.stringify(bin),
      '  client = new LanguageClient(',
      '    ' + JSON.stringify(single ? langs[0].id : 'tabnas') + ',',
      '    ' + JSON.stringify((single ? langs[0].id : 'tabnas') + ' language server') + ',',
      "    { command, args: ['--stdio'] },",
      '    {',
      '      documentSelector: [',
      langs.map((l) => "        { language: " + JSON.stringify(l.id) + " },").join('\n'),
      '      ],',
      '    },',
      '  )',
      '  client.start()',
      '}',
      '',
      'function deactivate() {',
      '  return client ? client.stop() : undefined',
      '}',
      '',
      'module.exports = { activate, deactivate }',
      '',
    ].join('\n'))

    // jsonic-family defaults; adjust for the grammar's own comment and
    // bracket forms.
    files.set('vscode/language-configuration.json', JSON.stringify({
      comments: { lineComment: '#', blockComment: ['/*', '*/'] },
      brackets: [['{', '}'], ['[', ']'], ['(', ')']],
      autoClosingPairs: [
        { open: '{', close: '}' },
        { open: '[', close: ']' },
        { open: '(', close: ')' },
        { open: '"', close: '"', notIn: ['string'] },
        { open: "'", close: "'", notIn: ['string'] },
      ],
      surroundingPairs: [['{', '}'], ['[', ']'], ['(', ')'], ['"', '"'], ["'", "'"]],
    }, null, 2) + '\n')

    files.set('vscode/.vscodeignore', 'node_modules/**\n')
  },

  nvim(langs, bin, files) {
    const lines = [
      '-- Generated by tabnas-lsp-gen. Neovim >= 0.11 (vim.lsp.config).',
      '-- For older Neovim, adapt to nvim-lspconfig custom-server setup.',
      '',
      'vim.filetype.add({ extension = {',
    ]
    for (const l of langs) {
      for (const x of l.extensions) {
        lines.push('  [' + JSON.stringify(x.replace(/^\./, '')) + '] = ' +
          JSON.stringify(l.id) + ',')
      }
    }
    lines.push('} })', '')
    const name = 1 === langs.length ? langs[0].id + '_lsp' : 'tabnas_lsp'
    lines.push(
      'vim.lsp.config[' + JSON.stringify(name) + '] = {',
      '  cmd = { ' + JSON.stringify(bin) + ", '--stdio' },",
      '  filetypes = { ' + langs.map((l) => JSON.stringify(l.id)).join(', ') + ' },',
      "  root_markers = { '.git' },",
      '}',
      'vim.lsp.enable(' + JSON.stringify(name) + ')',
      '',
    )
    files.set('nvim/tabnas.lua', lines.join('\n'))
  },

  emacs(langs, bin, files) {
    const name = 1 === langs.length ? langs[0].id : 'tabnas'
    const lines = [
      ';; Generated by tabnas-lsp-gen. Eglot (built into Emacs 29+).',
      '',
    ]
    for (const l of langs) {
      const mode = l.id + '-mode'
      lines.push(
        ';; A minimal major mode so eglot has something to attach to.',
        '(define-derived-mode ' + mode + ' prog-mode "' + l.id + '")',
        ...l.extensions.map((x) =>
          "(add-to-list 'auto-mode-alist '(\"\\\\" + x + "\\\\'\" . " + mode + '))'),
        "(with-eval-after-load 'eglot",
        "  (add-to-list 'eglot-server-programs",
        "               '(" + mode + ' . ("' + bin + '" "--stdio"))))',
        '',
      )
    }
    files.set('emacs/' + name + '-lsp.el', lines.join('\n'))
  },

  sublime(langs, bin, files) {
    const clients = {}
    for (const l of langs) {
      clients[l.id + '-lsp'] = {
        enabled: true,
        command: [bin, '--stdio'],
        // Sublime selects by syntax scope; without a dedicated syntax
        // definition, scope by file extension via the selector below
        // needs a syntax that claims these extensions. See README.
        selector: 'source.' + l.id,
      }
    }
    files.set('sublime/LSP.sublime-settings', JSON.stringify(
      { clients }, null, 2) + '\n')
  },

  helix(langs, bin, files) {
    const name = 1 === langs.length ? langs[0].id + '-lsp' : 'tabnas-lsp'
    const lines = [
      '# Generated by tabnas-lsp-gen. Merge into ~/.config/helix/languages.toml',
      '',
      '[language-server.' + name + ']',
      'command = ' + JSON.stringify(bin),
      'args = ["--stdio"]',
      '',
    ]
    for (const l of langs) {
      lines.push(
        '[[language]]',
        'name = ' + JSON.stringify(l.id),
        'scope = ' + JSON.stringify('source.' + l.id),
        'file-types = [' + l.extensions.map((x) =>
          JSON.stringify(x.replace(/^\./, ''))).join(', ') + ']',
        'language-servers = [' + JSON.stringify(name) + ']',
        '',
      )
    }
    files.set('helix/languages.toml', lines.join('\n'))
  },

  kate(langs, bin, files) {
    const servers = {}
    for (const l of langs) {
      servers[l.id] = {
        command: [bin, '--stdio'],
        rootIndicationFileNames: ['.git'],
        highlightingModeRegex: '^' + l.id + '$',
      }
    }
    files.set('kate/lspclient-settings.json', JSON.stringify(
      { servers }, null, 2) + '\n')
  },

  zed(langs, bin, files) {
    // Zed language extensions are declarative for the language itself,
    // but binding a language server needs the extension's (small) Rust
    // shim — scaffold the declarative half and document the rest.
    const name = 1 === langs.length ? langs[0].id : 'tabnas'
    files.set('zed/extension.toml', [
      '# Generated by tabnas-lsp-gen — scaffold. Binding the language',
      '# server requires the extension Rust shim; see README.md.',
      'id = ' + JSON.stringify(name + '-lsp'),
      'name = ' + JSON.stringify(name + ' (tabnas)'),
      'version = "0.1.0"',
      'schema_version = 1',
      '',
      '[language_servers.' + name + '-lsp]',
      'name = ' + JSON.stringify(name + ' LSP'),
      'languages = [' + langs.map((l) => JSON.stringify(l.id)).join(', ') + ']',
      '',
    ].join('\n'))
    for (const l of langs) {
      files.set('zed/languages/' + l.id + '/config.toml', [
        'name = ' + JSON.stringify(l.id),
        'grammar = ' + JSON.stringify(l.id),
        'path_suffixes = [' + l.extensions.map((x) =>
          JSON.stringify(x.replace(/^\./, ''))).join(', ') + ']',
        '',
      ].join('\n'))
    }
    files.set('zed/README.md', [
      '# Zed extension scaffold',
      '',
      'Zed language extensions declare languages in TOML but bind',
      'language servers through a small Rust shim (`src/lib.rs`',
      'implementing `zed_extension_api`), and highlighting needs a',
      'tree-sitter grammar reference. This scaffold carries the',
      'declarative half; wire the shim to launch `' + bin + ' --stdio`.',
      'See https://zed.dev/docs/extensions/languages',
      '',
    ].join('\n'))
  },
}

// ---------------------------------------------------------------------
// READMEs.

function editorList(editors, prefix) {
  return editors.map((e) => '- `' + (prefix || '') + e + '/`').join('\n')
}

function readmeSingle(lang, runtime, editors) {
  const build = 'node' === runtime
    ? ['```', 'cd server && npm install', 'npx ' + lang.id + '-lsp --stdio', '```',
      '',
      'Installing the package (`npm install -g ./server`, or publishing',
      'it) puts the `' + lang.id + '-lsp` command on PATH, which is what',
      'the generated editor configurations launch.']
    : ['```', 'cd server && go mod tidy && go build -o ' + lang.id + '-lsp .', '```',
      '',
      'Put the built binary on PATH (`go install .` with GOBIN on PATH,',
      'or copy it into a PATH directory): the generated editor',
      'configurations launch `' + lang.id + '-lsp` by name — editors do',
      'not run servers from the build directory. The VS Code setting',
      '`tabnas.' + lang.id + '.serverPath` accepts an absolute path',
      'instead.']
  return [
    '# ' + lang.id + ' language server',
    '',
    'Generated by `tabnas-lsp-gen` (from `@tabnas/lsp`). The server is a',
    'thin wrapper over the tabnas LSP pipeline: diagnostics with',
    'multi-error recovery, completion, semantic tokens, and outline are',
    'derived from the grammar itself — regenerate rather than edit.',
    '',
    '## Server (' + runtime + ')',
    '',
    ...build,
    '',
    'The server speaks LSP over stdio.',
    '',
    '## Editors',
    '',
    editorList(editors, 'editors/'),
    '',
    'Each directory contains the plugin or configuration fragment for',
    'that editor, wired to launch the server above. VS Code: `cd',
    'editors/vscode && npm install`, then package with `vsce` or run via',
    'the Extension Development Host. Sublime needs a syntax definition',
    'claiming the file extensions for its selector to match.',
    '',
  ].join('\n')
}

function readmeUnified(langs, editors) {
  return [
    '# tabnas unified language server — editor plugins',
    '',
    'Generated by `tabnas-lsp-gen --unified` from the bundled registry.',
    'The server is `tabnas-lsp --stdio` (from `@tabnas/lsp`); these',
    'plugins register it for every enabled registry language:',
    '',
    langs.map((l) => '- `' + l.id + '` (' + l.extensions.join(', ') + ')').join('\n'),
    '',
    'Languages with entrenched incumbent support (json, yaml, css, …)',
    'are default-off in the registry and deliberately absent here —',
    'enable them per workspace instead (design §5, collision policy).',
    '',
    '## Editors',
    '',
    editorList(editors),
    '',
  ].join('\n')
}

// ---------------------------------------------------------------------

function writeFiles(out, files) {
  // The manifest lists this run's files (itself excluded, sorted, no
  // timestamps — regeneration must be byte-stable for the staleness
  // gates). It is read back on the NEXT run to delete generator-owned
  // files that run no longer produces; only manifested files are ever
  // deleted, so user files beside the output are never touched.
  const list = [...files.keys()].sort()
  files.set(MANIFEST, JSON.stringify(
    { generated: 'tabnas-lsp-gen', files: list }, null, 1) + '\n')

  let previous = []
  try {
    const m = JSON.parse(fs.readFileSync(path.join(out, MANIFEST), 'utf8'))
    if (Array.isArray(m.files)) previous = m.files
  } catch (e) {
    // no previous run
  }

  // The manifest is a FILE ON DISK, so it is input, not a trusted
  // record: it can be hand-edited, merged badly, or written by an older
  // version. Every entry is therefore contained to `out` before
  // anything is unlinked. Without this, an entry like
  // '../precious/keep.txt' deleted a file outside the output directory,
  // and the prune loop below — which stopped only on EXACT equality
  // with `out` — then climbed past it, rmdir'ing ancestors until one
  // was non-empty. Non-string entries are dropped for the same reason.
  const resolvedOut = path.resolve(out)

  // Lexical containment is necessary but NOT sufficient: path.resolve
  // normalises `..` and nothing else, so it cannot see a symlink. With
  // `out/link` pointing outside the tree, `link/victim` passes every
  // string test here while unlinkSync follows `link` straight out of
  // it — and generated output is routinely a checked-out project, so
  // the symlink is attacker-supplied in exactly the case that matters.
  // The ANCESTOR is what has to be real: unlink does not follow a
  // symlink at the final component (it removes the link itself), so
  // resolving the containing directory closes the hole.
  let realOut = resolvedOut
  try { realOut = fs.realpathSync(resolvedOut) } catch (e) { /* new tree */ }
  const under = (p, root) => p === root || p.startsWith(root + path.sep)
  // Same rule for the prune loop below: it climbs from a deleted file's
  // directory, so a symlinked ancestor would let rmdir walk out too.
  const realDirUnder = (dir, root) => {
    try { return under(fs.realpathSync(dir), root) } catch (e) { return false }
  }

  const inside = (rel) => {
    if ('string' !== typeof rel) return false
    const abs = path.resolve(out, rel) // an absolute rel resolves to itself
    if (abs === resolvedOut || !abs.startsWith(resolvedOut + path.sep)) {
      return false
    }
    let realDir
    try {
      realDir = fs.realpathSync(path.dirname(abs))
    } catch (e) {
      return false // cannot resolve it, so cannot vouch for it
    }
    return under(realDir, realOut)
  }
  const stale = previous.filter((rel) => inside(rel) && !files.has(rel))

  for (const rel of stale) {
    try {
      fs.unlinkSync(path.join(out, rel))
    } catch (e) {
      // already gone
    }
  }
  // Prune directories the deletions emptied. Every `dir` here descends
  // from `out` by construction (stale is contained), so the equality
  // stop is sound; the containment re-check is belt-and-braces.
  for (const rel of stale) {
    let dir = path.dirname(path.join(out, rel))
    while (realDirUnder(dir, realOut) && path.resolve(dir) !== resolvedOut &&
      path.resolve(dir).startsWith(resolvedOut + path.sep)) {
      try {
        fs.rmdirSync(dir) // fails (kept) unless empty
      } catch (e) {
        break
      }
      dir = path.dirname(dir)
    }
  }

  for (const [rel, content] of files) {
    const abs = path.join(out, rel)
    fs.mkdirSync(path.dirname(abs), { recursive: true })
    fs.writeFileSync(abs, content)
  }
}

module.exports = { generate, GenerateError, ALL_EDITORS }
