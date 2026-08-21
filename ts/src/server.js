/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Protocol front-end: a thin vscode-languageserver wiring over core.js
// (design §11). Incremental sync, version-stamped push diagnostics,
// per-language routing via the registry, workspace-registered grammars
// (the dynamic-add lanes, design §6), and the tabnas/status custom
// request.
//
// The protocol library is required lazily so core.js stays testable
// without it (and so a browser build can substitute
// vscode-languageserver/browser).

const fs = require('fs')
const path = require('path')
const url = require('url')

const { Registry, loadBundled } = require('./registry')
const { DocumentStore } = require('./documents')
const { SerialInstances } = require('./instances')
const { makeLoader, LoadError } = require('./loaders')
const core = require('./core')

const DEBOUNCE_MS = 150

// Name of the per-workspace-folder grammar manifest.
const WORKSPACE_MANIFEST = '.tabnas/lsp.json'

function folderPathOf(uriOrPath) {
  if (null == uriOrPath) return null
  if (/^file:/.test(uriOrPath)) {
    try {
      return url.fileURLToPath(uriOrPath)
    } catch (e) {
      return null
    }
  }
  return uriOrPath
}

// Read a folder's .tabnas/lsp.json manifest: { languages: [entry...] }.
// Entries are stamped workspace-sourced with the folder as their
// sandbox dir; a malformed manifest is reported, never fatal.
function readWorkspaceManifest(folder, report) {
  const file = path.join(folder, WORKSPACE_MANIFEST)
  let text
  try {
    text = fs.readFileSync(file, 'utf8')
  } catch (e) {
    return [] // no manifest — the common case
  }
  try {
    const manifest = JSON.parse(text)
    const languages = Array.isArray(manifest.languages) ? manifest.languages : []
    return languages.map((e) =>
      Object.assign({}, e, { _source: 'workspace', _dir: folder }))
  } catch (e) {
    if (report) report(file + ': ' + e.message)
    return []
  }
}

function startServer(opts) {
  const lsp = require('vscode-languageserver/node')
  const connection = opts?.connection || lsp.createConnection(lsp.ProposedFeatures.all)

  // Late-bound loader options: initializationOptions arrive after the
  // loader is constructed; the same object is mutated at initialize.
  const loaderOpts = { trust: {} }

  // A generated single-language server passes `entries` — the exact
  // list it serves — replacing the bundled fleet registry entirely.
  const baseEntries = () =>
    opts?.entries ? opts.entries : loadBundled(opts?.registryFile)

  let registry = new Registry(
    baseEntries(),
    opts?.workspaceEntries,
    opts?.userEntries,
  )
  const docs = new DocumentStore()
  const instances = new SerialInstances(makeLoader(opts?.require, loaderOpts))
  const timers = new Map()
  const lastGood = new Map() // uri -> { version, analysis }
  let workspaceFolders = []
  let initLangs = []

  // Build (or rebuild) the registry's workspace tier from
  // initializationOptions.languages plus each folder's .tabnas/lsp.json
  // (dynamic add, design §6). Called at initialize and again whenever a
  // manifest changes — invalidating instances alone would keep serving
  // the OLD entry objects: edited grammar paths and options would be
  // ignored, added languages never routed, removed ones never dropped.
  function rebuildRegistry() {
    for (const e of registry ? registry.all() : []) {
      if ('workspace' === e._source) instances.invalidate(e)
    }
    const workspace = []
    const defaultDir = workspaceFolders[0] || process.cwd()
    for (const e of initLangs) {
      workspace.push(Object.assign(
        { _source: 'workspace', _dir: defaultDir }, e))
    }
    for (const folder of workspaceFolders) {
      workspace.push(...readWorkspaceManifest(folder, (msg) =>
        connection.console.warn('tabnas-lsp: workspace manifest ignored: ' + msg)))
    }
    registry = new Registry(
      baseEntries(),
      (opts?.workspaceEntries || []).concat(workspace),
      opts?.userEntries,
    )
    for (const e of registry.all()) {
      if ('workspace' === e._source) instances.invalidate(e)
    }
  }

  connection.onInitialize((params) => {
    const init = params?.initializationOptions || {}
    if (true === init.trustWorkspaceModules) {
      loaderOpts.trust.workspaceModules = true
    }

    workspaceFolders = (params?.workspaceFolders || [])
      .map((f) => folderPathOf(f.uri))
      .filter(Boolean)
    if (0 === workspaceFolders.length && params?.rootUri) {
      const root = folderPathOf(params.rootUri)
      if (root) workspaceFolders = [root]
    }

    initLangs = Array.isArray(init.languages) ? init.languages : []
    rebuildRegistry()

    return {
      capabilities: {
        textDocumentSync: {
          openClose: true,
          change: lsp.TextDocumentSyncKind.Incremental,
        },
        completionProvider: { triggerCharacters: [':', ',', '{', '[', '"'] },
        documentSymbolProvider: true,
        semanticTokensProvider: {
          legend: { tokenTypes: core.LEGEND, tokenModifiers: [] },
          full: true,
        },
        hoverProvider: true,
      },
    }
  })

  connection.onInitialized(() => {
    // Watch workspace grammar sources so the L4 dev loop works: an
    // edited spec/BNF file rebuilds its instance and re-analyzes open
    // documents. Registration is best-effort — plain clients without
    // dynamic registration simply do not get hot reload.
    const watched = registry.all().filter(
      (e) => 'workspace' === e._source &&
        (null != e.load?.grammar || 'string' === typeof e.load?.spec))
    if (0 === watched.length) return
    try {
      connection.client.register(lsp.DidChangeWatchedFilesNotification.type, {
        watchers: [
          { globPattern: '**/' + WORKSPACE_MANIFEST },
          { globPattern: '**/*.{abnf,ebnf,gbnf}' },
          { globPattern: '**/*.json' },
        ],
      }).catch(() => {})
    } catch (e) {
      // client does not support dynamic registration
    }
  })

  connection.onDidChangeWatchedFiles((p) => {
    const changedPaths = (p?.changes || [])
      .map((c) => folderPathOf(c.uri))
      .filter(Boolean)
    if (0 === changedPaths.length) return

    // A changed manifest means the workspace TIER changed — added,
    // removed, or re-configured languages — so the registry itself is
    // rebuilt, and every open document re-resolves and re-analyzes.
    const manifestChanged = workspaceFolders.some((folder) =>
      changedPaths.includes(path.join(folder, WORKSPACE_MANIFEST)))
    if (manifestChanged) {
      rebuildRegistry()
      for (const doc of docs.docs.values()) {
        lastGood.delete(doc.uri)
        schedule(doc.uri)
      }
      return
    }

    // Otherwise: a grammar source file changed — rebuild that entry's
    // instance and re-analyze its documents.
    for (const entry of registry.all()) {
      if ('workspace' !== entry._source || null == entry._dir) continue
      const file = entry.load?.grammar ||
        ('string' === typeof entry.load?.spec ? entry.load.spec : null)
      if (null == file) continue
      const abs = path.resolve(entry._dir, file)
      if (changedPaths.includes(abs)) {
        instances.invalidate(entry)
        for (const doc of docs.docs.values()) {
          if (entryFor(doc) === entry) schedule(doc.uri)
        }
      }
    }
  })

  function entryFor(doc) {
    const { entry } = registry.resolve(doc.languageId, doc.uri)
    return entry
  }

  function schedule(uri) {
    clearTimeout(timers.get(uri))
    timers.set(
      uri,
      setTimeout(() => {
        timers.delete(uri)
        run(uri)
      }, DEBOUNCE_MS),
    )
  }

  function run(uri) {
    const doc = docs.get(uri)
    if (!doc) return
    const entry = entryFor(doc)
    if (!entry) return
    let inst
    try {
      inst = instances.get(entry)
    } catch (e) {
      const detail = e instanceof LoadError ? e.message : String(e && e.message)
      connection.console.error('tabnas-lsp: grammar load failed: ' + detail)
      return
    }
    if (!inst) return // quarantined
    let analysis
    try {
      analysis = core.analyze(instances, inst, entry, doc)
    } catch (e) {
      instances.recordFailure(entry)
      connection.console.error('tabnas-lsp: analysis failed (' + entry.languageId + '): ' + e.message)
      return
    }
    // Version-stamped push: stale results never land on newer content.
    connection.sendDiagnostics({
      uri,
      version: doc.version,
      diagnostics: analysis.diagnostics,
    })
    if (!analysis.failed) {
      lastGood.set(uri, { version: doc.version, analysis })
    }
  }

  // Serve from the current version, else last-good ONLY at the same
  // version (edit-transformation of cached spans is tracked work; the
  // stale-range review rule says suppress rather than mis-highlight).
  function currentAnalysis(uri) {
    const doc = docs.get(uri)
    const lg = lastGood.get(uri)
    if (doc && lg && lg.version === doc.version) return lg.analysis
    return null
  }

  connection.onDidOpenTextDocument((p) => {
    const d = p.textDocument
    docs.open(d.uri, d.languageId, d.version, d.text)
    schedule(d.uri)
  })

  connection.onDidChangeTextDocument((p) => {
    const doc = docs.get(p.textDocument.uri)
    if (!doc) return
    let text = doc.text
    for (const change of p.contentChanges) {
      if (null == change.range) {
        text = change.text
      } else {
        const start = doc.offsetAt(change.range.start)
        const end = doc.offsetAt(change.range.end)
        text = text.substring(0, start) + change.text + text.substring(end)
        doc.update(text, doc.version) // keep line index fresh mid-loop
      }
    }
    doc.update(text, p.textDocument.version)
    lastGood.delete(p.textDocument.uri) // suppress stale structural results
    schedule(p.textDocument.uri)
  })

  connection.onDidCloseTextDocument((p) => {
    docs.close(p.textDocument.uri)
    lastGood.delete(p.textDocument.uri)
    connection.sendDiagnostics({ uri: p.textDocument.uri, diagnostics: [] })
  })

  connection.onCompletion((p) => {
    const doc = docs.get(p.textDocument.uri)
    if (!doc) return []
    const entry = entryFor(doc)
    if (!entry) return []
    const inst = instances.get(entry)
    if (!inst) return []
    return core.completion(inst, entry, doc, p.position)
  })

  connection.onDocumentSymbol((p) => {
    const a = currentAnalysis(p.textDocument.uri)
    return a ? a.outline : []
  })

  connection.languages.semanticTokens.on((p) => {
    const a = currentAnalysis(p.textDocument.uri)
    return { data: a && a.semanticTokens ? a.semanticTokens.data : [] }
  })

  connection.onHover((p) => {
    const doc = docs.get(p.textDocument.uri)
    const a = currentAnalysis(p.textDocument.uri)
    if (!doc || !a) return null
    return null // v1: hover ships with tokenDesc wiring (tracked)
  })

  connection.onRequest('tabnas/status', () => ({
    languages: registry.all().map((e) => ({
      languageId: e.languageId,
      enabled: e.enabled,
      source: e._source,
      quarantined: instances.quarantined(e),
      lexStream: e.lexStream,
    })),
  }))

  connection.listen()
  return { connection, registry: () => registry, docs, instances }
}

module.exports = { startServer, makeLoader, DEBOUNCE_MS, WORKSPACE_MANIFEST, readWorkspaceManifest }
