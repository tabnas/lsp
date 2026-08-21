/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Grammar registry: document -> grammar resolution for the unified
// language server. Entries come from three sources with precedence
// workspace > user > bundled (design §3, admin
// notes/2026-08-17-unified-lsp-design.md).
//
// Workspace entries are FOLDER-SCOPED: they carry the folder they came
// from (`_dir`) and apply only to documents inside it, resolved by
// path containment with the deepest folder winning. Two roots
// declaring the same languageId therefore keep their own grammars —
// a flat languageId-keyed map made the last-loaded folder capture the
// first folder's documents (review catch on #1). User and bundled
// entries are global and keyed by languageId as before.
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
    // entry ('bundled' | 'user' | 'workspace').
    _source: e._source || 'bundled',
    // _dir is the SANDBOX BASE: the folder a workspace entry's relative
    // grammar paths resolve against (loaders.js resolveSandboxed), and
    // part of the instance cache key.
    _dir: e._dir || null,
    // _scope is the ROUTING SCOPE and is deliberately separate: the
    // folder whose documents this entry serves, or null for "anywhere".
    // Conflating the two broke client-supplied entries — they need a
    // sandbox base (some folder) but are session-wide, so scoping them
    // to that base made them invisible in every other workspace folder.
    // Defaults to _dir so a folder manifest stays folder-scoped.
    _scope: '_scope' in e ? e._scope : (e._dir || null),
  }
}

// The filesystem path of a document URI, null for non-file documents
// (untitled:, vscode-notebook-cell:, …) — workspace scoping applies
// only to documents that live in a folder.
function fsPathOf(uri) {
  const m = /^file:\/\/[^/]*(\/.*)$/.exec(uri || '')
  if (!m) return null
  let p = decodeURIComponent(m[1])
  // Windows drive form: /c:/dir -> c:/dir
  if (/^\/[A-Za-z]:/.test(p)) p = p.slice(1)
  return p
}

// Containment over the two path shapes this server actually holds.
// fsPathOf() always yields forward slashes; the folder side comes from
// url.fileURLToPath, which yields BACKSLASHES on win32 — so on Windows
// the two sides never matched and folder-scoped routing was dead. The
// dir side is normalised to forward slashes before comparing.
function contains(dir, fsPath) {
  if (null == dir || null == fsPath) return false
  const d = String(dir).replace(/\\/g, '/').replace(/\/+$/, '')
  return fsPath === d || fsPath.startsWith(d + '/')
}

class Registry {
  constructor(bundled, workspace, user) {
    // Global tiers, keyed by languageId: user beats bundled.
    this.global = new Map()
    for (const list of [bundled || [], user || []]) {
      for (const raw of list) {
        const e = normalize(raw)
        this.global.set(e.languageId, e)
      }
    }
    // Workspace tier: folder-scoped, ordered as given.
    this.workspace = (workspace || []).map(normalize)
  }

  get(languageId) {
    return this.global.get(languageId)
  }

  // Resolve a document to an entry.
  //
  // Workspace entries win for documents inside their folder — matched
  // by languageId first, then by extension, deepest folder first.
  // Outside any workspace folder (or when none matches), the client
  // languageId wins only when it names an ENABLED global entry
  // (editors send generic ids like 'plaintext' for unknown
  // extensions); otherwise fall back to the most-specific extension
  // match. Ties surface for diagnostics.
  resolve(languageId, uri) {
    const fsPath = fsPathOf(uri)
    const ext = extOf(uri)

    // Workspace candidates, most specific first: entries scoped to a
    // folder containing this document (deepest folder wins), then
    // unscoped session-wide entries. An unscoped entry still applies to
    // a non-file document (untitled:, vscode-notebook-cell:), which a
    // folder-scoped one never can.
    const usable = this.workspace.filter(
      (e) => e.enabled && 'modifier' !== e.pluginKind)
    const scoped = null == fsPath ? [] : usable
      .filter((e) => null != e._scope && contains(e._scope, fsPath))
      .sort((a, b) => String(b._scope).length - String(a._scope).length)
    const candidates = scoped.concat(usable.filter((e) => null == e._scope))

    if (0 < candidates.length) {
      const byId = candidates.find((e) => e.languageId === languageId)
      if (byId) return { entry: byId, via: 'workspace:languageId' }
      const byExt = candidates.find((e) =>
        null != ext && e.extensions.some((x) => x.toLowerCase() === ext))
      if (byExt) return { entry: byExt, via: 'workspace:extension' }
    }

    const direct = this.global.get(languageId)
    if (direct && direct.enabled && 'modifier' !== direct.pluginKind) {
      return { entry: direct, via: 'languageId' }
    }

    if (!ext) return { entry: null, via: null }

    let best = null
    let ambiguous = []
    for (const e of this.global.values()) {
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
    return [...this.global.values(), ...this.workspace]
  }
}

function extOf(uri) {
  const m = /(\.[^./\\]+)$/.exec(uri || '')
  return m ? m[1].toLowerCase() : null
}

module.exports = { Registry, loadBundled, normalize, fsPathOf }
