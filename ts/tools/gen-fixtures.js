#!/usr/bin/env node
/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Generate diagnostics conformance fixtures from the fleet's existing
// test/spec TSV corpus (plan B5): every ERROR:<code> row becomes a
// fixture { languageId, input, expect: { codes: [code] } }. These are
// the cross-runtime parity contract for the Go server (plan P2) —
// generated, never hand-authored (derive-don't-duplicate).

const fs = require('fs')
const path = require('path')

function unescapeTSV(s) {
  return s.replace(/\\n/g, '\n').replace(/\\r/g, '\r').replace(/\\t/g, '\t')
}

function collect(root) {
  const fixtures = []
  let repos = []
  try {
    repos = fs.readdirSync(root)
  } catch (e) {
    return fixtures
  }
  for (const repo of repos) {
    const specDir = path.join(root, repo, 'test', 'spec')
    let files = []
    try {
      files = fs.readdirSync(specDir).filter((f) => f.endsWith('.tsv'))
    } catch (e) {
      continue
    }
    for (const f of files) {
      const lines = fs.readFileSync(path.join(specDir, f), 'utf8').split('\n')
      for (let i = 1; i < lines.length; i++) {
        const cols = lines[i].split('\t')
        if (cols.length < 2) continue
        const m = /^ERROR:([a-z_0-9]+)/.exec(cols[1] || '')
        if (m) {
          fixtures.push({
            languageId: repo,
            file: f,
            row: i + 1,
            input: unescapeTSV(cols[0]),
            expect: { codes: [m[1]] },
          })
        }
      }
    }
  }
  return fixtures
}

function main() {
  // Fleet root default matches gen-registry.js: two levels above the
  // ts/ package (ts/tools -> ts -> lsp -> fleet).
  const root = process.argv[2] || path.join(__dirname, '..', '..', '..')
  const fixtures = collect(root)

  // Same refusal shape as gen-registry: a near-empty result means the
  // fleet is not where we looked (a partial checkout sees only the
  // engine's own spec dir), not that the corpus shrank. Overwriting
  // the committed 262-fixture corpus with that would silently gut the
  // cross-runtime parity contract. A real fleet checkout spans many
  // grammar repos, so require at least three before writing.
  const languages = new Set(fixtures.map((f) => f.languageId))
  if (languages.size < 3) {
    console.error(
      'gen-fixtures: only ' + languages.size + ' repo(s) with ERROR rows ' +
        'under ' + root + ' — this looks like a partial fleet checkout.\n' +
        'Pass the fleet root explicitly: node tools/gen-fixtures.js <root>\n' +
        'Refusing to overwrite data/diagnostic-fixtures.json.',
    )
    process.exitCode = 1
    return
  }

  const out = {
    generated: 'tools/gen-fixtures.js',
    count: fixtures.length,
    fixtures,
  }
  const file = path.join(__dirname, '..', 'data', 'diagnostic-fixtures.json')
  fs.writeFileSync(file, JSON.stringify(out, null, 1) + '\n')
  console.log('wrote ' + file + ' (' + fixtures.length + ' fixtures)')
}

if (require.main === module) main()
module.exports = { collect }
