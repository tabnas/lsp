/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// The language-server generator (design §7): given ONE grammar — a
// fleet registry entry, a plugin module, a serialized GrammarSpec, or
// BNF-dialect text — emit a standalone single-language server plus the
// editor plugins needed to use it. The server runtime follows where
// the grammar can execute:
//
//   input \ runtime |  node                     |  go                       |  rust
//   ----------------+---------------------------+---------------------------+---------------------------
//   entry           |  static reg. over this pkg|  pure-data: embed spec    |  pure-data: embed spec
//   module (npm)    |  package + module dep     |  ✗ (pass --go-plugin)     |  ✗ (pass --rust-plugin)
//   spec (JSON)     |  embed + L2 load          |  go:embed, engine-only dep|  include_str!, lsp-only dep
//   grammar (BNF)   |  compile at server start  |  pre-compile, then embed  |  pre-compile, then embed
//
// Emitted wrappers contain no pipeline logic: they pin WHAT is served,
// not HOW — a fix in this package reaches every generated Node server
// on update, and the Go module and the Rust crate on rebuild.
//
// `--unified` instead generates the multi-language editor plugins for
// the whole bundled registry over `tabnas-lsp` itself (this is how the
// repo's own VS Code extension is built rather than hand-maintained).

const fs = require('fs')
const path = require('path')

const { loadBundled, normalize } = require('./registry')
const { firewallSpec, compileGrammarText, LoadError, DIALECTS } = require('./loaders')

const ALL_EDITORS = ['vscode', 'nvim', 'emacs', 'sublime', 'helix', 'kate', 'zed']

const RUNTIMES = ['node', 'go', 'rust']

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
//   runtime     'node' | 'go' | 'rust'   (default 'node')
//   editors     subset of vscode,nvim,emacs,sublime,helix,kate,zed;
//               [] for none (default: all)
//   unified     editor plugins for the whole bundled registry
//   goModule    module path for the generated go.mod
//   goPlugin    Go import path of the plugin package (closure/
//               imperative grammars)
//   goPluginFunc exported plugin func name (default: CamelCase id)
//   goReplace   ['mod=../dir', ...] replace directives for local dev
//   rustCrate   package name for the generated Cargo.toml (default:
//               <id>-lsp)
//   rustLspRev / rustParserRev  commits to pin tabnas-lsp and the
//               engine to (default: each repository's default branch,
//               recorded by Cargo.lock at the first build)
//   rustPlugin  the grammar's Rust crate (closure/imperative grammars)
//   rustPluginGit  its repository (default: https://github.com/tabnas/X
//               for a crate named tabnas-X)
//   rustPluginRev  a commit to pin it to
//   rustPluginFn   its constructor (default: make)
//   rustPath    ['crate=../dir', ...] local checkouts in place of the
//               git sources, for local dev
//   registryFile bundled registry override (tests)
//   requireFn   module resolver override (tests)
function generate(opts) {
  const out = opts.out
  if (!out) throw new GenerateError('out directory required')
  const runtime = opts.runtime || 'node'
  if (!RUNTIMES.includes(runtime)) {
    throw new GenerateError(
      "unknown runtime '" + runtime + "': " + RUNTIMES.join(', ') +
        ' exist today; other engine ports arrive via the pure-GrammarSpec ' +
        'lane and the C ABI (design §7.4)')
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
    else if ('go' === runtime) emitGoServer(lang, opts, files)
    else emitRustServer(lang, opts, files)
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
// The grammar a compiled server embeds: the Go module and the Rust
// crate take the same lanes (design §7.2), so they share this. Returns
// the spec text to embed, or null when the entry's grammar is live code
// the caller links in as a plugin instead (`plugin` is the Go import
// path or the Rust crate the caller was given; `how` names the runtime
// and its plugin option for the refusal).

function embeddedSpec(lang, opts, plugin, how) {
  if ('spec' === lang.kind) return lang.specText
  if ('grammar' === lang.kind) {
    // Pre-compile BNF to a pure spec at generation time (the C-ABI
    // precedent: pre-compile -> L2), so the server depends only on the
    // engine (Go) or on tabnas-lsp (Rust), and serves what the
    // canonical compiler produced.
    const req = opts.requireFn || require
    const compiled = compileGrammarText(req, lang.grammarFile, lang.grammarText)
    const parserMod = req('@tabnas/parser')
    const issues = firewallSpec(compiled, parserMod)
    if (0 < issues.length) {
      throw new GenerateError(
        'compiled grammar failed the firewall:\n  ' +
          issues.map((i) => i.path + ': ' + i.message).join('\n  '))
    }
    return JSON.stringify(compiled, null, 1) + '\n'
  }
  // 'entry'
  if (plugin) return null // plugin-linked by the caller
  if ('data' === lang.entry.grammarKind && lang.entry.load &&
    lang.entry.load.spec && 'string' !== typeof lang.entry.load.spec) {
    return JSON.stringify(lang.entry.load.spec, null, 1) + '\n'
  }
  throw new GenerateError(
    "entry '" + lang.id + "' is grammarKind: " + lang.entry.grammarKind +
      ' — its grammar is live code, which a ' + how.runtime + ' server ' +
      'cannot load from npm. Pass ' + how.plugin + ' naming its ' +
      how.runtime + ' twin, or --spec with a pure-data grammar.')
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

  const spec = embeddedSpec(lang, opts, opts.goPlugin, {
    runtime: 'Go',
    plugin: '--go-plugin <import-path>',
  })

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
// Rust server crate.

// Where the Rust crates come from. The tabnas crates are not on
// crates.io, so a generated crate names each one's repository and cargo
// finds the package inside it (tabnas-lsp lives in this repo's rs/).
const RUST_LSP_GIT = 'https://github.com/tabnas/lsp'
const RUST_PARSER_GIT = 'https://github.com/tabnas/parser'

// The edition and MSRV of tabnas-lsp (rs/Cargo.toml): a crate over it
// builds with no older compiler. generate.test.js holds RUST_VERSION to
// rs/Cargo.toml's rust-version, which the npm package does not ship.
const RUST_EDITION = '2021'
const RUST_VERSION = '1.85'

// A TOML basic string. JSON's escapes are a subset of TOML's (\uXXXX
// included), so JSON.stringify writes a valid one.
function tomlStr(s) {
  return JSON.stringify(String(s))
}

// A Rust string literal. Not JSON.stringify: Rust has no \b, \f or
// \uXXXX escapes, so control characters are written as \u{..}.
function rustStr(s) {
  let out = '"'
  for (const ch of String(s)) {
    const cp = ch.codePointAt(0)
    if ('"' === ch || '\\' === ch) out += '\\' + ch
    else if ('\n' === ch) out += '\\n'
    else if ('\r' === ch) out += '\\r'
    else if ('\t' === ch) out += '\\t'
    else if (cp < 0x20 || 0x7f === cp) out += '\\u{' + cp.toString(16) + '}'
    else out += ch
  }
  return out + '"'
}

// Cargo package names are ASCII letters, digits, `-` and `_` here (cargo
// takes any Unicode XID, but a registry or a filesystem may not), and
// cannot start with a digit.
const RUST_CRATE = /^[A-Za-z_][A-Za-z0-9_-]*$/
// A crate's constructor, as a path inside it: `make`, `make_json`,
// `grammar::make`.
const RUST_FN_PATH = /^[A-Za-z_][A-Za-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*)*$/
// The binary is the command editors launch, so it is the language id
// plus `-lsp` exactly, and must be a plain file name that cargo takes as
// a target name: letters, digits, `_` and `-` (a `.` or a `+` in a
// target name is refused by cargo before anything compiles).
const RUST_BIN = /^[A-Za-z0-9_][A-Za-z0-9_-]*$/

// What each fleet grammar crate names by sibling path beyond the engine
// (its rs/Cargo.toml `[dependencies]`): the registry's base chain says
// which grammar a crate is layered on, but a crate can name more (ini
// names hoover, feed names xml with no base at all), and a
// git checkout of the crate has to be supplied every one of them from
// its own repository. ts/test/generate.test.js holds this map to the
// sibling checkouts whenever the fleet layout has them.
const RUST_FLEET_DEPS = {
  csv: ['jsonic'], feed: ['xml'], ini: ['jsonic', 'hoover'],
  json: [], json5: ['jsonic'], jsonc: ['jsonic'], jsonic: ['json'],
  jsonl: ['json'], toml: ['jsonic'], xml: [], yaml: ['jsonic'],
  zon: ['jsonic'], hoover: [], abnf: ['bnf'], ebnf: ['bnf'], gbnf: ['bnf'],
  bnf: [],
}

// rustfmt's defaults, which the emitted Rust follows so that a `cargo
// fmt` in the generated crate changes nothing: 100 columns a line, and
// an array literal or a call's arguments go one per line once they are
// wider than 60.
const RUST_MAX_WIDTH = 100
const RUST_LIST_WIDTH = 60

// A list literal: `open` (`vec![`, `[`, `&[`), items, `close`. An item
// is an expression, or an array of expressions for a tuple.
function rustItem(i) {
  return Array.isArray(i) ? '(' + i.join(', ') + ')' : i
}

function rustFlatList(list) {
  return list.open + list.items.map(rustItem).join(', ') + list.close
}

// The width rustfmt holds to RUST_LIST_WIDTH for a literal on its own:
// its items, without the brackets.
function rustListWidth(list) {
  return list.items.map(rustItem).join(', ').length
}

// The list broken open: the opener ends the current line, the items
// sit at `indent` + 4, and the closer at `indent` starts the last line.
// One item per line, except that rustfmt packs a list whose items are
// all 10 columns or narrower onto as few lines as fit.
function rustTallList(list, indent) {
  const pad = ' '.repeat(indent + 4)
  const texts = list.items.map(rustItem)
  let rows = texts.map((t) => pad + t + ',')
  if (texts.every((t, i) => !Array.isArray(list.items[i]) && t.length <= 10)) {
    rows = []
    for (const t of texts) {
      const last = rows.length - 1
      if (0 <= last && rows[last].length + 1 + t.length + 1 <= RUST_MAX_WIDTH) {
        rows[last] += ' ' + t + ','
      } else {
        rows.push(pad + t + ',')
      }
    }
  }
  return [list.open, ...rows, ' '.repeat(indent) + list.close]
}

// `entry.<field> = <before><list><after>;` in main's body.
function rustAssign(field, before, list, after) {
  const head = '    entry.' + field + ' = ' + before
  const flat = rustFlatList(list)
  const line = head + flat + after + ';'
  // Inside `Some(..)` the whole argument is what is held to the width
  // (a call's arguments), else the literal's items are.
  const width = before.startsWith('Some(')
    ? (before.slice('Some('.length) + flat + after.slice(0, -1)).length
    : rustListWidth(list)
  if (width <= RUST_LIST_WIDTH && line.length <= RUST_MAX_WIDTH) return [line]
  // rustfmt breaks a lone tuple open rather than the list around it.
  if (1 === list.items.length && Array.isArray(list.items[0])) {
    return [head + list.open + '(',
      ...list.items[0].map((e) => '        ' + e + ','),
      '    )' + list.close + after + ';']
  }
  const tall = rustTallList(list, 4)
  return [head + tall[0], ...tall.slice(1, -1), tall[tall.length - 1] + after + ';']
}

// `let <pat> = <fn>(<args>);` in main's body, where an argument is an
// expression or a list: on one line when it fits, else the call on the
// next line, else one argument per line.
function rustLet(pat, fn, args) {
  const flatArgs = args.map((a) => 'string' === typeof a ? a : rustFlatList(a))
  const call = fn + '(' + flatArgs.join(', ') + ');'
  const line = '    let ' + pat + ' = ' + call
  const narrow = flatArgs.join(', ').length <= RUST_LIST_WIDTH &&
    args.every((a) => 'string' === typeof a || rustListWidth(a) <= RUST_LIST_WIDTH)
  if (narrow && line.length <= RUST_MAX_WIDTH) return [line]
  if (narrow && 8 + call.length <= RUST_MAX_WIDTH) {
    return ['    let ' + pat + ' =', '        ' + call]
  }
  const lines = ['    let ' + pat + ' = ' + fn + '(']
  args.forEach((a, i) => {
    const flat = flatArgs[i]
    if ('string' === typeof a ||
      (rustListWidth(a) <= RUST_LIST_WIDTH && 8 + flat.length + 1 <= RUST_MAX_WIDTH)) {
      lines.push('        ' + flat + ',')
    } else {
      const tall = rustTallList(a, 8)
      lines.push('        ' + tall[0], ...tall.slice(1, -1), tall[tall.length - 1] + ',')
    }
  })
  lines.push('    );')
  return lines
}

// Registry metadata that must survive into the generated Rust entry,
// for the reason goEntryMeta gives: the same fields, so the Rust server
// recovers, colours and outlines the grammar as the Go and Node ones do.
function rustEntryMeta(entry) {
  const owned = (s) => rustStr(s) + '.to_string()'
  const map = (m) => ({
    open: '[',
    items: Object.keys(m).sort().map((k) => [owned(k), owned(m[k])]),
    close: ']',
  })
  const hashMap = 'Some(std::collections::HashMap::from('
  const lines = []
  if (entry.syncGroups && 0 < entry.syncGroups.length) {
    lines.push(...rustAssign('sync_groups', 'Some(',
      { open: 'vec![', items: entry.syncGroups.map(owned), close: ']' }, ')'))
  }
  if (entry.lexStream && 'clean' !== entry.lexStream) {
    lines.push('    entry.lex_stream = Some(' + owned(entry.lexStream) + ');')
  }
  if (entry.semanticTokens && 0 < Object.keys(entry.semanticTokens).length) {
    lines.push(...rustAssign('semantic_tokens', hashMap, map(entry.semanticTokens), '))'))
  }
  if (entry.outlineRules && 0 < Object.keys(entry.outlineRules).length) {
    lines.push(...rustAssign('outline_rules', hashMap, map(entry.outlineRules), '))'))
  }
  return lines
}

function emitRustServer(lang, opts, files) {
  if ('module' === lang.kind) {
    throw new GenerateError(
      'a TypeScript plugin module cannot run in a Rust server. For a ' +
        'grammar with a Rust crate, pass --entry <languageId> with ' +
        '--rust-plugin <crate>; for a pure-data grammar, pass --spec.')
  }

  const bin = lang.id + '-lsp'
  if (!RUST_BIN.test(bin)) {
    throw new GenerateError(
      "cannot name a binary after '" + lang.id + "': editors launch '" + bin +
        "', which is not a name cargo gives a binary — pass a --language-id " +
        'of letters, digits, _ and -')
  }
  const crate = opts.rustCrate ||
    (lang.id + '-lsp').replace(/[^A-Za-z0-9_-]+/g, '-').replace(/-+/g, '-')
  if (!RUST_CRATE.test(crate)) {
    throw new GenerateError(
      "'" + crate + "' is not a Cargo package name (letters, digits, - " +
        'and _, not starting with a digit) — pass --rust-crate')
  }

  const spec = embeddedSpec(lang, opts, opts.rustPlugin, {
    runtime: 'Rust',
    plugin: '--rust-plugin <crate>',
  })

  // The plugin crate, for a grammar that is live code (spec and grammar
  // inputs embed their grammar whatever else is passed, as Go's do).
  let plugin = null
  if (null == spec) {
    const name = opts.rustPlugin
    if (!RUST_CRATE.test(name) || 'tabnas-lsp' === name || 'tabnas' === name) {
      throw new GenerateError(
        "--rust-plugin '" + name + "' does not name a grammar crate")
    }
    const fleet = /^tabnas-(.+)$/.exec(name)
    const git = opts.rustPluginGit ||
      (fleet ? 'https://github.com/tabnas/' + fleet[1] : null)
    if (!git) {
      throw new GenerateError(
        "cannot derive a repository for '" + name + "' (the fleet names its " +
          'crates tabnas-<name>) — pass --rust-plugin-git <url>')
    }
    const fn = opts.rustPluginFn || 'make'
    if (!RUST_FN_PATH.test(fn)) {
      throw new GenerateError(
        "--rust-plugin-fn '" + fn + "' is not a Rust path")
    }
    plugin = { crate: name, git, rev: opts.rustPluginRev, fn }
  }

  // Every source this crate names. The engine is not a direct
  // dependency: tabnas-lsp (and a grammar crate) name it by SIBLING path
  // (`../../parser/rs`), the fleet's development layout, and inside a
  // git checkout cargo reads that as "the package `tabnas` in this same
  // repository", which has none. So each git source gets a [patch]
  // table supplying the engine from its own repository, which is also
  // what makes every crate here share one engine.
  const parser = { crate: 'tabnas', git: RUST_PARSER_GIT, rev: opts.rustParserRev }
  const lsp = { crate: 'tabnas-lsp', git: RUST_LSP_GIT, rev: opts.rustLspRev }
  const direct = plugin ? [lsp, plugin] : [lsp]

  // A fleet grammar crate names the grammars it is layered on by sibling
  // path as well, and the registry's base chain says which (toml on
  // jsonic, jsonic on json): each is supplied from its own repository,
  // with a table of its own for the next. A crate that names more than
  // its chain (ini also names hoover) makes cargo report the one it
  // cannot find, and that is one more line in the table named.
  const layers = []
  if (plugin && 'entry' === lang.kind) {
    const byName = new Map(loadBundled(opts.registryFile).map((e) => [e.name, e]))
    let base = lang.entry.base
    while (base && /^@tabnas\/[A-Za-z0-9_-]+$/.test(base) &&
      !layers.some((l) => l.name === base)) {
      const short = base.slice('@tabnas/'.length)
      layers.push({ name: base, crate: 'tabnas-' + short,
        git: 'https://github.com/tabnas/' + short })
      const next = byName.get(base)
      base = next ? next.base : null
    }
  }

  // A fleet crate, from its own repository.
  const fleetCrate = (short) => ({ crate: 'tabnas-' + short,
    git: 'https://github.com/tabnas/' + short })
  const shortOf = (dep) => {
    const m = /^tabnas-(.+)$/.exec(dep.crate)
    return m && m[1] in RUST_FLEET_DEPS ? m[1] : null
  }

  // Local checkouts in place of git sources (`--rust-path crate=dir`),
  // the counterpart of Go's replace directives. A crate taken from a
  // path resolves its own siblings by path, so it needs no patch table.
  const known = [lsp.crate, parser.crate, ...(plugin ? [plugin.crate] : []),
    ...layers.map((l) => l.crate),
    ...Object.keys(RUST_FLEET_DEPS).map((short) => 'tabnas-' + short)]
  const paths = new Map()
  for (const p of opts.rustPath || []) {
    const eq = String(p).indexOf('=')
    const name = String(p).slice(0, eq)
    const dir = String(p).slice(eq + 1)
    if (eq <= 0 || '' === dir || !known.includes(name)) {
      throw new GenerateError(
        "--rust-path '" + p + "' must be <crate>=<dir> for a crate this " +
          'server names: ' + known.join(', '))
    }
    paths.set(name, dir)
  }
  const at = (dep) => paths.has(dep.crate)
    ? '{ path = ' + tomlStr(paths.get(dep.crate)) + ' }'
    : '{ git = ' + tomlStr(dep.git) +
      (dep.rev ? ', rev = ' + tomlStr(dep.rev) : '') + ' }'

  // The [patch] tables: one per git source, naming the engine and what
  // that source names by sibling path. tabnas-lsp names only the
  // engine: its optional dialect and fleet crates are path dependencies
  // too, but cargo resolves a git dependency's optional path
  // dependencies only for the features a consumer turns on, and this
  // crate turns none on (a grammar's crate is linked directly). The
  // grammar crate names the engine, the layer the registry's base chain
  // puts under it, and whatever else its manifest names by path
  // (RUST_FLEET_DEPS for a fleet crate), each of those its own the same
  // way, and a chain stops at the first crate taken from a path.
  const tables = []
  const table = (source) => {
    let found = tables.find((t) => t.source.crate === source.crate)
    if (!found) {
      found = { source, names: [parser] }
      tables.push(found)
    }
    return found
  }
  const name = (t, dep) => {
    if (!t.names.some((d) => d.crate === dep.crate)) t.names.push(dep)
  }
  if (!paths.has(lsp.crate)) table(lsp)
  if (plugin) {
    const below = new Map(layers.map((l, i) => [l.crate, layers[i + 1]]))
    below.set(plugin.crate, layers[0])
    const queue = [plugin]
    const seen = new Set()
    while (0 < queue.length) {
      const source = queue.shift()
      if (seen.has(source.crate) || paths.has(source.crate)) continue
      seen.add(source.crate)
      const own = table(source)
      const deps = []
      if (below.get(source.crate)) deps.push(below.get(source.crate))
      const short = shortOf(source)
      if (short) for (const dep of RUST_FLEET_DEPS[short]) deps.push(fleetCrate(dep))
      for (const dep of deps) {
        name(own, dep)
        queue.push(dep)
      }
    }
  }

  // What the pin comment reports: the git sources this file names.
  const gitSources = [...new Set([
    ...direct,
    ...tables.flatMap((t) => t.names),
  ])].filter((d) => !paths.has(d.crate))
  const anyPinned = gitSources.some((d) => d.rev)
  const allPinned = 0 < gitSources.length && gitSources.every((d) => d.rev)

  const cargo = [
    '# Generated by tabnas-lsp-gen. The pipeline lives in the tabnas-lsp',
    '# crate; this crate only pins what is served. Regenerate rather than',
    '# grow it.',
    '',
    '[package]',
    'name = ' + tomlStr(crate),
    'version = "0.1.0"',
    'edition = ' + tomlStr(RUST_EDITION),
    'rust-version = ' + tomlStr(RUST_VERSION),
    '# The MSRV-aware resolver: with no Cargo.lock yet, the first build',
    '# picks the newest crates.io versions that still build with the',
    '# rust-version above, not merely the newest.',
    'resolver = "3"',
    'description = ' + tomlStr('Language server for ' + lang.id +
      ' (generated by tabnas-lsp-gen)'),
    'license = "MIT"',
    'publish = false',
    '',
    '[[bin]]',
    'name = ' + tomlStr(bin),
    'path = "src/main.rs"',
    '',
    '[dependencies]',
    ...(0 < gitSources.length
      ? ['# The tabnas crates are not on crates.io, so each comes from its',
        '# repository on GitHub.']
      : ['# Local checkouts, in place of the GitHub repositories the tabnas',
        '# crates come from (they are not on crates.io).']),
    ...(allPinned
      ? ['# Every git source in this file is pinned to the commit its rev',
        '# names.']
      : anyPinned
        ? ['# A git source with a rev is pinned to that commit; one without',
          '# takes its default branch at the first build, and Cargo.lock',
          '# records the commit: keep Cargo.lock with the crate.']
        : 0 < gitSources.length
          ? ['# The first build takes each default branch and Cargo.lock',
            '# records the commit: keep Cargo.lock with the crate. Pass',
            '# --rust-lsp-rev/--rust-parser-rev' +
              (plugin ? '/--rust-plugin-rev' : '') + ' to pin commits here',
            '# instead.']
          : []),
    ...(0 < gitSources.length
      ? ['# For unreleased local checkouts, pass --rust-path <crate>=<dir> to',
        '# the generator (or write `path = "<dir>"` in place of `git`).']
      : []),
    ...direct.map((d) => d.crate + ' = ' + at(d)),
  ]
  for (const { source, names } of tables) {
    cargo.push(
      '',
      ...(source === lsp
        ? ['# tabnas-lsp names the engine by sibling path (../../parser/rs),',
          '# which cargo reads inside a git checkout as a package of that',
          '# same repository; this table supplies it from its own. Its',
          '# optional dialect and fleet crates are named the same way, and',
          '# resolved only for a feature turned on here, which none is.']
        : source === plugin
          ? ['# The grammar crate names the engine by sibling path too, and',
            '# the grammar it is layered on (the registry base chain), each',
            '# supplied here from its own repository. When cargo reports',
            '# "no matching package named ..." in one of these repositories,',
            '# that crate names one more: add it to that repository\'s table.']
          : []),
      '[patch.' + tomlStr(source.git) + ']',
      ...names.map((d) => d.crate + ' = ' + at(d)),
    )
  }
  cargo.push(
    '',
    '# A workspace of its own, so generating into a directory under another',
    '# workspace does not make cargo claim it. Delete this table to make',
    '# the crate a member of an enclosing workspace instead.',
    '[workspace]',
    '',
  )
  files.set('server/Cargo.toml', cargo.join('\n'))

  const metaLines = lang.entry ? rustEntryMeta(lang.entry) : []
  const exts = { open: '&[', items: lang.extensions.map(rustStr), close: ']' }
  const header = [
    '//! Generated by tabnas-lsp-gen. The pipeline lives in the tabnas-lsp',
    '//! crate; this wrapper only pins what is served. Regenerate rather than',
    '//! grow it.',
    '',
  ]
  const serve = [
    '    match tabnas_lsp::serve(config) {',
    '        Ok(true) => ExitCode::SUCCESS,',
    '        // The protocol: `exit` without `shutdown`, or a closed stream,',
    '        // ends the process with status 1.',
    '        Ok(false) => {',
    '            eprintln!(' + rustStr(bin + ': the session ended without shutdown') + ');',
    '            ExitCode::FAILURE',
    '        }',
    '        Err(error) => {',
    // RUST_BIN admits no brace, so the name is safe in a format string.
    '            eprintln!(' + rustStr(bin + ': {error}') + ');',
    '            ExitCode::FAILURE',
    '        }',
    '    }',
    '}',
    '',
  ]

  if (null != spec) {
    files.set('server/grammar.json', spec)
    files.set('server/src/main.rs', [
      ...header,
      'use std::process::ExitCode;',
      '',
      'use tabnas_lsp::loaders::entry_from_spec_json;',
      'use tabnas_lsp::Config;',
      '',
      '/// The grammar served: a serialized GrammarSpec, which passed the',
      '/// firewall when this crate was generated and passes it again at',
      '/// every load.',
      'const GRAMMAR: &str = include_str!("../grammar.json");',
      '',
      'fn main() -> ExitCode {',
      ...rustLet('(' + (0 < metaLines.length ? 'mut ' : '') + 'entry, make_instance)',
        'entry_from_spec_json', [rustStr(lang.id), exts, 'GRAMMAR']),
      ...metaLines,
      '    let mut config = Config::new(make_instance);',
      '    config.entries = vec![entry];',
      ...serve,
    ].join('\n'))
  } else {
    files.set('server/src/main.rs', [
      ...header,
      'use std::process::ExitCode;',
      'use std::sync::Arc;',
      '',
      'use tabnas_lsp::loaders::Loader;',
      'use tabnas_lsp::{Config, Entry};',
      '',
      'fn main() -> ExitCode {',
      '    let mut entry = Entry::new(' + rustStr(lang.id) + ');',
      '    entry.language_id = Some(' + rustStr(lang.id) + '.to_string());',
      ...rustAssign('extensions', '', {
        open: 'vec![',
        items: lang.extensions.map((x) => rustStr(x) + '.to_string()'),
        close: ']',
      }, ''),
      '    entry.enabled = Some(true);',
      ...metaLines,
      '    // The grammar is live code, linked in: its crate builds an engine',
      '    // with the grammar installed, and the loader applies the entry.',
      '    let mut loader = Loader::new();',
      '    loader.link(' + rustStr(lang.id) + ', Arc::new(' +
        plugin.crate.replace(/-/g, '_') + '::' + plugin.fn + '));',
      '    let mut config = Config::new(loader.into_make_instance());',
      '    config.entries = vec![entry];',
      ...serve,
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
      engines: { vscode: '^1.91.0' },
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
      dependencies: { 'vscode-languageclient': '^10.1.1' },
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
  const bin = lang.id + '-lsp'
  const build = 'node' === runtime
    ? ['```', 'cd server && npm install', 'npx ' + bin + ' --stdio', '```',
      '',
      'Installing the package (`npm install -g ./server`, or publishing',
      'it) puts the `' + bin + '` command on PATH, which is what',
      'the generated editor configurations launch.']
    : 'go' === runtime
      ? ['```', 'cd server && go mod tidy && go build -o ' + bin + ' .', '```',
        '',
        'Put the built binary on PATH (`go install .` with GOBIN on PATH,',
        'or copy it into a PATH directory): the generated editor',
        'configurations launch `' + bin + '` by name — editors do',
        'not run servers from the build directory. The VS Code setting',
        '`tabnas.' + lang.id + '.serverPath` accepts an absolute path',
        'instead.']
      : ['```', 'cd server && cargo install --path .', '```',
        '',
        '`cargo install` builds the release profile and puts `' + bin + '`',
        "in Cargo's bin directory (`~/.cargo/bin`, which rustup puts on",
        'PATH): the generated editor configurations launch it by name —',
        'editors do not run servers from the build directory. `cargo build',
        '--release` leaves it at `server/target/release/' + bin + '`',
        'instead, and the VS Code setting `tabnas.' + lang.id + '.serverPath`',
        'accepts that absolute path.',
        '',
        'The tabnas crates come from their GitHub repositories (they are',
        'not on crates.io). The first build takes each default branch and',
        'writes `Cargo.lock`, which records the commits: keep it with the',
        'crate, and the server builds against the same engine every time.',
        'Cargo reads the sibling paths the tabnas crates name as packages',
        'of the same repository, so `Cargo.toml` supplies each one through',
        'a `[patch]` table; when cargo reports `no matching package named',
        '...` inside a grammar crate, that crate names another tabnas crate',
        'and the fix is one more line in its table.']
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
