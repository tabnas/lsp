#!/usr/bin/env node
/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Generate data/registry.json from fleet tabnas.plugin.json
// descriptors (the mcp/ts/tools/gen-data.js pattern: sibling
// checkouts, one and two levels up, dedupe by name, sort), merged with
// the overrides table below for the fields descriptors do not carry
// yet (plan C1). Each override is deleted as C1 lands per repo.

const fs = require('fs')
const path = require('path')

// languageId / kind / capability overrides (design §3). enabled:false
// = editor-collision policy default-off (entrenched incumbents).
const OVERRIDES = {
  '@tabnas/json': { enabled: false },
  '@tabnas/jsonc': { enabled: false },
  '@tabnas/css': { enabled: false },
  '@tabnas/markdown': { enabled: false, grammarKind: 'external' },
  '@tabnas/yaml': { enabled: false },
  '@tabnas/c': { enabled: false, grammarKind: 'external' },
  '@tabnas/toml': { enabled: false },
  '@tabnas/xml': { enabled: false },
  '@tabnas/ini': { enabled: false },
  '@tabnas/proto': { enabled: false, grammarKind: 'compiled' },
  '@tabnas/chess': { languageId: 'pgn' },
  '@tabnas/csv': {},
  '@tabnas/json5': {},
  '@tabnas/jsonl': {},
  '@tabnas/zon': {},
  '@tabnas/feed': { pluginKind: 'grammar' },
  '@tabnas/abnf': { pluginKind: 'compiler' },
  '@tabnas/ebnf': { pluginKind: 'compiler' },
  '@tabnas/gbnf': { pluginKind: 'compiler' },
  '@tabnas/bnf': { pluginKind: 'compiler' },
  '@tabnas/expr': { pluginKind: 'modifier', grammarKind: 'imperative' },
  '@tabnas/hoover': { pluginKind: 'modifier', grammarKind: 'imperative' },
  '@tabnas/debug': { pluginKind: 'modifier', grammarKind: 'imperative' },
  '@tabnas/directive': { pluginKind: 'modifier' },
  '@tabnas/multisource': { pluginKind: 'modifier' },
  '@tabnas/path': { pluginKind: 'modifier' },
}

function findDescriptors(root) {
  const found = new Map()
  const levels = [root, path.join(root, '..')]
  for (const base of levels) {
    let names = []
    try {
      names = fs.readdirSync(base)
    } catch (e) {
      continue
    }
    for (const n of names) {
      const p = path.join(base, n, 'tabnas.plugin.json')
      try {
        const d = JSON.parse(fs.readFileSync(p, 'utf8'))
        if (d.name && !found.has(d.name)) found.set(d.name, d)
      } catch (e) {
        // not a plugin repo
      }
    }
  }
  return [...found.values()].sort((a, b) => a.name.localeCompare(b.name))
}

// Fields a descriptor may carry once plan C1 has landed for its repo
// (tasks/ax-descriptor.sh preserves exactly these through
// regeneration). A descriptor that carries one is AUTHORITATIVE for it:
// the override table is interim scaffolding, and ADR-10 says derive,
// never duplicate. Without this the table would silently shadow the
// descriptor the moment C1 rolled out, which is the failure that would
// look like C1 having no effect.
const LSP_FIELDS = [
  'languageId',
  'grammarKind',
  'pluginKind',
  'syncGroups',
  'semanticTokens',
  'lexStream',
  'optionsSchema',
]

// `enabled` is deliberately NOT in that list. It encodes the editor
// collision policy — whether tabnas should claim a language an
// entrenched extension already owns — which is a host decision, not a
// fact about the grammar. A plugin must not be able to enable itself.

function entryFor(d, redundant) {
  const out = {
    name: d.name,
    extensions: d.extensions || [],
    mediaTypes: d.mediaTypes || [],
    base: d.base || null,
    errorCodes: d.errorCodes || [],
  }
  const ov = OVERRIDES[d.name] || {}
  for (const k of Object.keys(ov)) out[k] = ov[k]
  for (const f of LSP_FIELDS) {
    if (undefined === d[f]) continue
    if (f in ov) {
      redundant.push(d.name + '.' + f)
    }
    out[f] = d[f]
  }
  return out
}

function main() {
  // Default to the promoted layout — this package is the `ts/` half of
  // the `lsp` repo, sitting beside its sibling repos in a fleet
  // checkout, so the fleet root is two levels above the package
  // (ts/tools -> ts -> lsp -> fleet). On a lone checkout with no
  // sibling repos the guard below makes that a clear error rather than
  // an empty registry.
  const root = process.argv[2] || path.join(__dirname, '..', '..', '..')
  const descriptors = findDescriptors(root)

  // Finding nothing means the fleet is not where we looked, not that
  // the fleet is empty — a wrong root is the likely cause, and this
  // staging layout is not where the promoted repo will sit. Writing
  // the empty result would replace a good registry with one that
  // routes no language at all, and the server would then serve
  // nothing while reporting no error. Refuse instead.
  if (0 === descriptors.length) {
    console.error(
      'gen-registry: no tabnas.plugin.json found under ' + root + '\n' +
        'Pass the fleet root explicitly: node tools/gen-registry.js <root>\n' +
        'Refusing to overwrite data/registry.json with an empty registry.',
    )
    process.exitCode = 1
    return
  }

  const redundant = []
  const entries = descriptors.map((d) => entryFor(d, redundant))
  const out = { generated: 'tools/gen-registry.js', count: entries.length, entries }
  const file = path.join(__dirname, '..', 'data', 'registry.json')
  fs.writeFileSync(file, JSON.stringify(out, null, 2) + '\n')
  console.log('wrote ' + file + ' (' + entries.length + ' entries)')
  // Naming them is how the interim table actually gets deleted rather
  // than quietly outliving the migration it was meant to bridge.
  if (0 < redundant.length) {
    console.log(
      'descriptors now carry these — delete from OVERRIDES: ' +
        redundant.join(', '),
    )
  }
}

if (require.main === module) main()
module.exports = { findDescriptors, entryFor, OVERRIDES, LSP_FIELDS }
