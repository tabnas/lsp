/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Grammar registry: document -> grammar resolution for the unified
// language server. Entries come from three sources, merged with
// precedence workspace > user > bundled (design §3, admin
// notes/2026-08-17-unified-lsp-design.md).
//
// A registry entry is a tabnas.plugin.json descriptor plus the
// LSP-specific fields the descriptors do not carry yet (languageId,
// grammarKind, pluginKind, syncGroups, semanticTokens, lexStream) —
// supplied by the overrides table in data/registry.json until the
// ax-descriptor extension (plan C1) lands them per repo.

const path = require('path')

// Load the generated bundle (tools/gen-registry.js).
function loadBundled(file) {
  const bundle = require(file || path.join(__dirname, '..', 'data', 'registry.json'))
  return bundle.entries.map(normalize)
}

function normalize(e) {
  return {
    name: e.name,
    languageId: e.languageId || (e.name || '').replace(/^@tabnas\//, ''),
    extensions: e.extensions || [],
    mediaTypes: e.mediaTypes || [],
    base: e.base || null,
    pluginKind: e.pluginKind || 'grammar', // grammar | compiler | modifier
    grammarKind: e.grammarKind || 'closure', // data | compiled | closure | imperative | external
    lexStream: e.lexStream || 'clean', // clean | speculative
    syncGroups: e.syncGroups || null,
    semanticTokens: e.semanticTokens || {},
    outlineRules: e.outlineRules || null,
    errorCodes: e.errorCodes || [],
    enabled: false !== e.enabled,
    // How to load: { module } | { spec } | { grammar } — the loader
    // (loaders.js) dispatches on this.
    load: e.load || { module: e.name },
    options: e.options || {},
    stack: e.stack || null, // explicit plugin stack override
    // Provenance, stamped by the host: which config tier supplied this
    // entry ('bundled' | 'user' | 'workspace') and, for workspace
    // entries, the folder its file paths resolve against (sandboxed).
    _source: e._source || 'bundled',
    _dir: e._dir || null,
  }
}

class Registry {
  constructor(bundled, workspace, user) {
    // Precedence: workspace > user > bundled; keyed by languageId.
    this.entries = new Map()
    for (const list of [bundled || [], user || [], workspace || []]) {
      for (const raw of list) {
        const e = normalize(raw)
        this.entries.set(e.languageId, e)
      }
    }
  }

  get(languageId) {
    return this.entries.get(languageId)
  }

  // Resolve a document to an entry. The client languageId wins only
  // when it names an ENABLED entry (editors send generic ids like
  // 'plaintext' for unknown extensions); otherwise fall back to the
  // most-specific extension match. Ties surface for diagnostics.
  resolve(languageId, uri) {
    const direct = this.entries.get(languageId)
    if (direct && direct.enabled && 'modifier' !== direct.pluginKind) {
      return { entry: direct, via: 'languageId' }
    }

    const ext = extOf(uri)
    if (!ext) return { entry: null, via: null }

    let best = null
    let ambiguous = []
    for (const e of this.entries.values()) {
      if (!e.enabled || 'modifier' === e.pluginKind) continue
      for (const x of e.extensions) {
        if (x.toLowerCase() === ext) {
          if (best && best.entry !== e) ambiguous.push(e)
          else best = { entry: e, via: 'extension' }
        }
      }
    }
    if (best && 0 < ambiguous.length) {
      best.ambiguous = [best.entry.languageId, ...ambiguous.map((e) => e.languageId)]
    }
    return best || { entry: null, via: null }
  }

  all() {
    return [...this.entries.values()]
  }
}

function extOf(uri) {
  const m = /(\.[^./\\]+)$/.exec(uri || '')
  return m ? m[1].toLowerCase() : null
}

module.exports = { Registry, loadBundled, normalize }
