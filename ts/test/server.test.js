/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Protocol-layer tests, driven through startServer with a stub
// connection: the handlers are where document state and routing
// actually meet, and every defect pinned here shipped past a green
// suite because nothing exercised that layer.

const { describe, it } = require('node:test')
const assert = require('node:assert')
const fs = require('fs')
const os = require('os')
const path = require('path')
const { pathToFileURL } = require('url')

const { startServer } = require('../src/server')

const FIXTURES = path.join(__dirname, '..', '..', 'test', 'fixtures')
const SPEC_FILE = path.join(FIXTURES, 'json-grammar.json')

// A stub vscode-languageserver connection: captures the handlers the
// server registers and the diagnostics it pushes.
function stubConnection() {
  const h = {}
  const conn = {
    handlers: h,
    diagnostics: [],
    logs: [],
    console: {
      error: (m) => conn.logs.push(m),
      warn: (m) => conn.logs.push(m),
    },
    client: { register: () => Promise.resolve() },
    languages: { semanticTokens: { on: (fn) => (h.semanticTokens = fn) } },
    listen: () => {},
    sendDiagnostics: (d) => conn.diagnostics.push(d),
    onInitialize: (fn) => (h.initialize = fn),
    onInitialized: (fn) => (h.initialized = fn),
    onDidOpenTextDocument: (fn) => (h.didOpen = fn),
    onDidChangeTextDocument: (fn) => (h.didChange = fn),
    onDidCloseTextDocument: (fn) => (h.didClose = fn),
    onDidChangeWatchedFiles: (fn) => (h.didChangeWatched = fn),
    onCompletion: (fn) => (h.completion = fn),
    onDocumentSymbol: (fn) => (h.documentSymbol = fn),
    onHover: (fn) => (h.hover = fn),
    onRequest: (m, fn) => (h['req:' + m] = fn),
  }
  return conn
}

function start(initParams, opts) {
  const connection = stubConnection()
  const server = startServer(Object.assign({ connection }, opts))
  connection.handlers.initialize(initParams || {})
  return { connection, server, h: connection.handlers }
}

describe('lsp-server-didchange', () => {
  it('a full replacement mixed with a ranged edit keeps the line index live', () => {
    // A didChange array may legally carry a full replacement followed
    // by ranged edits. The replacement branch used to leave the stored
    // document (and its line index) on the PREVIOUS text, so the next
    // ranged edit computed offsets against a document that no longer
    // existed — silently corrupting the text here, and panicking the
    // Go port on the same input (go TestDidChangeMixedFullAndRanged).
    // The edit must change the LINE STRUCTURE for this to bite in
    // JavaScript: String.substring clamps out-of-range offsets, so a
    // single-line case silently produces the right answer by luck. The
    // Go port has no such forgiveness and panics outright — see
    // go TestDidChangeMixedFullAndRanged.
    const { server, h } = start({})
    const uri = 'file:///t.jsonf'
    h.didOpen({
      textDocument: {
        uri, languageId: 'jsonf', version: 1,
        text: 'aaa\nbbb\nccc', // line starts 0, 4, 8
      },
    })
    h.didChange({
      textDocument: { uri, version: 2 },
      contentChanges: [
        // Full replacement: line starts become 0, 2.
        { text: 'x\ny' },
        // A ranged edit addressed against THAT text: start of line 1.
        {
          range: { start: { line: 1, character: 0 }, end: { line: 1, character: 0 } },
          text: 'Z',
        },
      ],
    })
    // Against the stale index, line 1 resolved to offset 4 (past the
    // end of 'x\ny'), appending: 'x\nyZ'.
    assert.equal(server.docs.get(uri).text, 'x\nZy')
  })

  it('consecutive ranged edits still compose', () => {
    const { server, h } = start({})
    const uri = 'file:///t.jsonf'
    h.didOpen({
      textDocument: { uri, languageId: 'jsonf', version: 1, text: '[]' },
    })
    h.didChange({
      textDocument: { uri, version: 2 },
      contentChanges: [
        { range: { start: { line: 0, character: 1 }, end: { line: 0, character: 1 } }, text: '1' },
        { range: { start: { line: 0, character: 2 }, end: { line: 0, character: 2 } }, text: ',2' },
      ],
    })
    assert.equal(server.docs.get(uri).text, '[1,2]')
  })
})

describe('lsp-server-routing', () => {
  // A client-supplied language (initializationOptions.languages) is
  // session-wide. Scoping it to workspaceFolders[0] made it dead in
  // every other root of a multi-root session, and dead everywhere when
  // the client sent no folders at all.
  const LANG = {
    languageId: 'mydsl',
    extensions: ['.mydsl'],
    load: { spec: SPEC_FILE },
  }

  it('routes in every folder of a multi-root session', () => {
    const { server } = start({
      workspaceFolders: [
        { uri: 'file:///ws/alpha' },
        { uri: 'file:///ws/beta' },
      ],
      initializationOptions: { languages: [LANG] },
    })
    const reg = server.registry()
    assert.equal(reg.resolve('mydsl', 'file:///ws/alpha/x.mydsl').entry.languageId, 'mydsl')
    assert.equal(reg.resolve('mydsl', 'file:///ws/beta/x.mydsl').entry.languageId, 'mydsl',
      'client-supplied language dead outside folder[0]')
  })

  it('routes with no workspace folders at all, and for non-file documents', () => {
    const { server } = start({ initializationOptions: { languages: [LANG] } })
    const reg = server.registry()
    assert.equal(reg.resolve('mydsl', 'file:///anywhere/x.mydsl').entry.languageId, 'mydsl')
    assert.equal(reg.resolve('mydsl', 'untitled:Untitled-1').entry.languageId, 'mydsl',
      'unscoped entries must serve non-file documents too')
  })

  it('a folder manifest stays scoped to its folder', () => {
    // pathToFileURL, not 'file://' + dir. On Windows a temp dir is
    // `C:\Users\...`, so concatenating produces `file://C:\Users\...`
    // — a URI whose authority is `c` and whose path is nothing of the
    // sort. Routing then matched nothing and `.entry` was null. It only
    // ever looked right because this suite had never run on Windows:
    // the applied ci.yml targeted a layout that does not exist, so
    // every run died at `npm i` before a test body executed.
    const alpha = fs.mkdtempSync(path.join(os.tmpdir(), 'lsp-ws-'))
    const alphaUri = pathToFileURL(alpha).href
    fs.mkdirSync(path.join(alpha, '.tabnas'), { recursive: true })
    fs.writeFileSync(path.join(alpha, '.tabnas', 'lsp.json'), JSON.stringify({
      languages: [{ languageId: 'scoped', extensions: ['.scoped'], load: { spec: SPEC_FILE } }],
    }))
    const { server } = start({
      workspaceFolders: [{ uri: alphaUri }, { uri: 'file:///ws/beta' }],
    })
    const reg = server.registry()
    assert.equal(
      reg.resolve('scoped', alphaUri + '/x.scoped').entry.languageId, 'scoped')
    assert.equal(
      reg.resolve('scoped', 'file:///ws/beta/x.scoped').entry, null,
      'a folder manifest must not capture another folder')
  })

  it('completion survives a grammar that fails to load', () => {
    // instances.get RETHROWS a load failure; onCompletion used to let
    // it escape, surfacing an InternalError to the client on every
    // keystroke of a broken grammar.
    const { connection, h } = start({
      initializationOptions: {
        languages: [{
          languageId: 'broken', extensions: ['.broken'],
          load: { spec: { rule: { top: { open: [{ s: '#NR', a: '@notabuiltin' }] } } } },
        }],
      },
    })
    const uri = 'file:///t.broken'
    h.didOpen({ textDocument: { uri, languageId: 'broken', version: 1, text: '1' } })
    let items
    assert.doesNotThrow(() => {
      items = h.completion({ textDocument: { uri }, position: { line: 0, character: 1 } })
    })
    assert.deepStrictEqual(items, [])
    assert.ok(connection.logs.some((m) => /grammar load failed/.test(m)),
      'the failure should be logged, not thrown: ' + JSON.stringify(connection.logs))
  })
})
