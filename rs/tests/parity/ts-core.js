/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// The TypeScript half of the random differential sweep that
// rs/tests/parity_sweep.rs drives (run it from there; its module docs
// say how). It generates the documents, seeded, so both runtimes see the
// same text, and answers each one with the CANONICAL pipeline,
// ts/src/core.js over the shared grammar: the LSP results the Rust port
// must reproduce, and beside them the engine's own view of the parse, in
// units neither engine owns, so a mismatch can be told apart as the
// port's (the engines agree, the pipelines do not) or the engines' (they
// do not agree on the parse to begin with).
//
//   node ts-core.js N SEED > cases.json
//
// Progress goes to stderr, a line per 50 documents. Nothing is written
// but standard output, and nothing is read but the shared fixtures and
// the ts/ package.

const fs = require('fs')
const path = require('path')

const ROOT = path.join(__dirname, '..', '..', '..')
const TS = path.join(ROOT, 'ts')
const { Tabnas } = require(require.resolve('@tabnas/parser', { paths: [TS] }))
const { Doc } = require(path.join(TS, 'src', 'documents'))
const { SerialInstances } = require(path.join(TS, 'src', 'instances'))
const core = require(path.join(TS, 'src', 'core'))
const { normalize } = require(path.join(TS, 'src', 'registry'))

const SPEC = JSON.parse(
  fs.readFileSync(path.join(ROOT, 'test', 'fixtures', 'json-grammar.json'), 'utf8'))

const N = Number(process.argv[2] || 300)
let seed = Number(process.argv[3] || 1) >>> 0 || 1

// ---------------------------------------------------------------------
// The entries: the conformance suite's `jsonf`; the same grammar with an
// entry's own outline rules and semantic-token overrides, so the sweep
// covers the per-entry maps as well as the defaults; and the same grammar
// FAIL-FAST, recovery off, the path where a parse error ends the parse
// and the analysis is `failed`. Documents take the three in turn.
// rs/tests/parity_sweep.rs builds the same three (`entries`).

const ENTRIES = {
  plain: normalize({
    name: 'jsonf', languageId: 'jsonf', extensions: ['.jsonf'], grammarKind: 'data',
  }),
  custom: normalize({
    name: 'jsonx', languageId: 'jsonx', extensions: ['.jsonx'], grammarKind: 'data',
    outlineRules: { pair: 'Field', elem: 'Item' },
    semanticTokens: { '#ST': 'property', '#VL': 'type', '#CA': 'macro' },
  }),
  failfast: normalize({
    name: 'jsonff', languageId: 'jsonff', extensions: ['.jsonff'], grammarKind: 'data',
  }),
}
const WHICH = ['plain', 'custom', 'failfast']

const instances = new SerialInstances((entry) => {
  const tn = new Tabnas({ parse: { recover: { enabled: 'jsonff' !== entry.languageId } } })
  tn.grammar(SPEC)
  entry._inst = tn
  return tn
})

// ---------------------------------------------------------------------
// The generator: a linear congruential sequence, so a seed names a sweep.

function rnd() {
  seed = (Math.imul(seed, 1103515245) + 12345) >>> 0
  return (seed >>> 1) / 0x80000000
}
const pick = (a) => a[Math.floor(rnd() * a.length)]

// The engine's defaults, which the shared grammar keeps, lex comments
// (`#` and `//` to the end of the line, `/* */` across lines), single
// and backtick quotes and hex numbers, so all of those are documents
// this grammar reads, and the `comments` category leans on them.
const COMMENTS = ['# c\n', '// note\n', '/* c */', '/* two\nlines */', '/* é𝄞 */', '#\r\n']
const LINE_BREAKS = ['\n', '\r\n', '\r', '\n  ', '\r\n\t']
const MULTI_BYTE = ['é', '𝄞', '日本', '😀z', 'ñ𝄞ñ', 'Ω', ' ']

function ws(cat) {
  const r = rnd()
  if ('comments' === cat && r < 0.3) return pick([' ', '']) + pick(COMMENTS)
  if ('multi-line' === cat && r < 0.4) return pick(LINE_BREAKS)
  if (r < 0.55) return ''
  if (r < 0.75) return ' '
  if (r < 0.85) return pick(LINE_BREAKS)
  if (r < 0.92) return pick(['  ', '\t'])
  return pick(COMMENTS)
}

function str(cat) {
  const body = 'multi-byte' === cat && rnd() < 0.6
    ? pick(MULTI_BYTE) + pick(['', 'a', ' ', pick(MULTI_BYTE)])
    : pick(['a', 'key', 'x y', 'a\\nb', '', 'é', '𝄞', '\\u00e9', 'q'])
  const quote = rnd() < 0.85 ? '"' : pick(["'", '`'])
  return quote + body + quote
}

function scalar(cat) {
  return pick(['1', '-2.5', 'true', 'false', 'null', str(cat), str(cat), '0', '1e3', '0x1F', '12.0'])
}

function val(cat, d) {
  const r = rnd()
  if (d > 3 || (d > 0 && r < 0.3) || (0 === d && r < 0.05)) return scalar(cat)
  const n = Math.floor(rnd() * 5)
  const parts = []
  if (r < 0.6) {
    for (let i = 0; i < n; i++) {
      parts.push(ws(cat) + str(cat) + ws(cat) + ':' + ws(cat) + val(cat, d + 1) + ws(cat))
    }
    return '{' + parts.join(',') + '}'
  }
  for (let i = 0; i < n; i++) parts.push(ws(cat) + val(cat, d + 1) + ws(cat))
  return '[' + parts.join(',') + ']'
}

const JUNK = [
  'blah', ',', ':', '}', ']', '{', '[', '"', "'", 'q', '𝄞', '#', ',,', '1 2',
  '/*', '\r', '\n', 'tru', '"é', '::', '@',
]
const BREAKERS = [
  // truncate
  (t) => t.slice(0, Math.floor(rnd() * t.length)),
  // insert junk
  (t) => {
    const i = Math.floor(rnd() * (t.length + 1))
    return t.slice(0, i) + pick(JUNK) + t.slice(i)
  },
  // delete one unit
  (t) => {
    const i = Math.floor(rnd() * t.length)
    return t.slice(0, i) + t.slice(i + 1)
  },
]

// A document is TEXT: a lone surrogate (a pair cut by a breaker) cannot
// cross into Rust, so such a candidate is dropped and drawn again.
const LONE_SURROGATE =
  /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/

const CATEGORIES = ['valid', 'invalid', 'multi-error', 'multi-byte', 'multi-line', 'comments']

// Errors as a RECOVERING parse counts them, whichever entry the document
// is for, so a fail-fast document is chosen by the same rule.
function errorsOf(text) {
  const inst = instances.get(ENTRIES.plain)
  const out = inst.parse(text)
  return out && Array.isArray(out.errors) ? out.errors.length : 0
}

function candidate(cat) {
  let t = ws(cat) + val(cat, 0) + ws(cat)
  if ('invalid' === cat) t = pick(BREAKERS)(t)
  else if ('multi-error' === cat) {
    const k = 2 + Math.floor(rnd() * 3)
    for (let i = 0; i < k; i++) t = pick(BREAKERS.slice(1))(t)
  } else if ('valid' !== cat && rnd() < 0.5) {
    t = pick(BREAKERS)(t)
  }
  return t
}

// Each category's document satisfies what the category names, or is
// drawn again (bounded, so a seed cannot loop).
function accepts(cat, entry, t) {
  if (LONE_SURROGATE.test(t)) return false
  switch (cat) {
    case 'valid': return 0 === errorsOf(t)
    case 'invalid': return 1 <= errorsOf(t)
    case 'multi-error': return 2 <= errorsOf(t)
    case 'multi-byte': return /[^\x00-\x7f]/.test(t)
    case 'multi-line': return /[\r\n]/.test(t)
    case 'comments': return /#|\/\/|\/\*/.test(t)
  }
  return true
}

function documentOf(cat, entry) {
  let t = candidate(cat)
  for (let tries = 0; tries < 200 && !accepts(cat, entry, t); tries++) t = candidate(cat)
  return t
}

// Completion positions: a line of the text (lines are `\n`-delimited, as
// every store here counts them) and a character within the line's
// content, never past it (the protocol clamps there and the TypeScript
// store does not: a recorded defect, not a parity question) and never
// between the two units of a surrogate pair (a Rust string cannot be cut
// there). One in three is the end of the text, where an editor's cursor
// usually is while typing.
function positionsOf(text) {
  const lines = text.split('\n')
  const out = []
  for (let k = 0; k < 3; k++) {
    if (0 === k && rnd() < 0.34) {
      const last = lines.length - 1
      out.push({ line: last, character: lines[last].length })
      continue
    }
    const line = Math.floor(rnd() * lines.length)
    const content = lines[line].replace(/\r$/, '')
    let character = Math.floor(rnd() * (content.length + 1))
    const unit = content.charCodeAt(character - 1)
    if (0xd800 <= unit && unit <= 0xdbff) character--
    out.push({ line, character })
  }
  return out
}

// ---------------------------------------------------------------------
// The engine's view, in code points: offsets, columns and lengths in a
// unit neither engine owns (UTF-16 here; bytes and scalar values in
// Rust). It is the RAW event stream the pipeline consumes, before any
// pipeline code runs (the lex events before reconciliation, the rule
// events, the errors as the engine serializes them, the value), so the
// Rust side can do two things with it: compare it with its own engine's
// stream, which says whether the two ENGINES agree, and feed it through
// its own pipeline functions, which says whether the two PIPELINES agree
// on identical input whatever the engines do.

function codePoints(text, utf16) {
  return Array.from(text.slice(0, utf16)).length
}

// A token as [name, si, ri, ci, len, src]: `si` the code-point offset,
// `ci` the 1-based column in code points since the engine's last column
// reset (the `cI - 1` UTF-16 units before the token, counted in code
// points), `len` the code points of the `len` units the token covers.
function point(inst, text, t) {
  const si = t.sI
  const units = Math.max(0, (t.cI | 0) - 1)
  return [
    t.name || String(inst.token(t.tin)),
    codePoints(text, si),
    t.rI,
    1 + Array.from(text.slice(Math.max(0, si - units), si)).length,
    Array.from(text.substr(si, Math.max(0, t.len | 0))).length,
    t.src,
  ]
}

function engineView(inst, text) {
  const lex = []
  const rules = []
  const collector = {
    lex: (tkn) => { if (0 <= tkn.sI) lex.push(point(inst, text, tkn)) },
    ruleDone: (rule, ctx, done) => {
      rules.push([
        rule.i, rule.name, done.state, !!done.forced, done.alt ? done.alt.r : '',
        0 < rule.oN ? point(inst, text, rule.o0) : null,
        0 < rule.cN ? point(inst, text, rule.c0) : null,
      ])
    },
  }
  let out
  try {
    out = instances.parse(inst, text, collector)
  } catch (e) {
    out = { value: undefined, errors: [e] }
  }
  const errors = (out && Array.isArray(out.errors) ? out.errors : []).map((e) => {
    const j = JSON.parse(JSON.stringify(e))
    const pos = 0 <= j.pos ? j.pos : 0
    const units = Math.max(0, (j.col | 0) - 1)
    return {
      code: j.code, message: j.message, hint: j.hint || '',
      row: j.row,
      col: 1 + Array.from(text.slice(Math.max(0, pos - units), pos)).length,
      pos: codePoints(text, pos),
      len: j.len,
    }
  })
  const value = out && 'object' === typeof out && 'errors' in out ? out.value : out
  return { value: undefined === value ? null : value, errors, lex, rules }
}

// ---------------------------------------------------------------------

const cases = []
const t0 = Date.now()
for (let n = 0; n < N; n++) {
  const cat = CATEGORIES[n % CATEGORIES.length]
  const which = WHICH[Math.floor(n / CATEGORIES.length) % WHICH.length]
  const entry = ENTRIES[which]
  const inst = instances.get(entry)
  const text = documentOf(cat, entry)
  const positions = positionsOf(text)
  const doc = new Doc('file:///t.' + entry.languageId, entry.languageId, 1, text)

  let lsp
  try {
    const a = core.analyze(instances, inst, entry, doc)
    lsp = {
      failed: a.failed,
      diagnostics: a.diagnostics,
      outline: a.outline,
      data: a.semanticTokens ? a.semanticTokens.data : null,
    }
  } catch (e) {
    lsp = { threw: String(e && (e.code || e.message)) }
  }
  const completions = positions.map((p) => core.completion(inst, entry, doc, p))
  const continuations = positions.map((p) => {
    try {
      return inst.continuations(text.substring(0, doc.offsetAt(p))).tokens
    } catch (e) {
      return null
    }
  })

  cases.push({
    n, category: cat, entry: which, text, positions,
    lsp, completions,
    engine: engineView(inst, text), continuations,
  })
  if (0 === (n + 1) % 50 || n + 1 === N) {
    process.stderr.write(
      `ts-core: ${n + 1} of ${N} (${Math.round((100 * (n + 1)) / N)}%), ${Date.now() - t0} ms\n`)
  }
}

const version = JSON.parse(fs.readFileSync(
  path.join(path.dirname(require.resolve('@tabnas/parser', { paths: [TS] })), '..', 'package.json'),
  'utf8')).version

process.stdout.write(JSON.stringify({ engine: version, cases }))
