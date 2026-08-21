/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Staleness gate for the committed editors/ tree: it is GENERATED
// (`tabnas-lsp-gen --unified --out editors`, or `make gen-editors`),
// never hand-edited — derive, don't duplicate. A registry change that
// alters the served language set must land with the regenerated
// plugins, or routing and editor wiring silently disagree.

const { describe, it } = require('node:test')
const assert = require('node:assert')
const fs = require('fs')
const os = require('os')
const path = require('path')

const { generate } = require('../src/generate')

const REPO = path.join(__dirname, '..', '..')

function walk(dir, base, out) {
  for (const name of fs.readdirSync(dir)) {
    const p = path.join(dir, name)
    const rel = null == base ? name : base + '/' + name
    if (fs.statSync(p).isDirectory()) walk(p, rel, out)
    else out.set(rel, fs.readFileSync(p, 'utf8'))
  }
  return out
}

describe('gen-editors-staleness', () => {
  it('committed editors/ matches a fresh --unified generation', () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'gen-stale-'))
    generate({ out: tmp, unified: true })
    const fresh = walk(tmp, null, new Map())
    const committed = walk(path.join(REPO, 'editors'), null, new Map())
    assert.deepStrictEqual(
      [...committed.keys()].sort(), [...fresh.keys()].sort(),
      'editors/ file set drifted — run: make gen-editors')
    for (const [rel, content] of fresh) {
      assert.equal(committed.get(rel), content,
        'editors/' + rel + ' is stale — run: make gen-editors')
    }
  })
})
