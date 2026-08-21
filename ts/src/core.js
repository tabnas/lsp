/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// The parse pipeline (design §5): ONE debounced parse per change, with
// the mux collectors installed, yields diagnostics, semantic tokens,
// and outline together. Protocol-independent — server.js is a thin
// front-end over this module, keeping the byte-parity discipline with
// the CLI/MCP surfaces possible.

// Default engine-token -> LSP semantic-token-type map, sourced from
// railroad's CANON key set (engine-standard tokens only; #ID is
// per-plugin and comes from registry overrides), plus the prefix
// conventions for non-jsonic token schemes (design §5).
const DEFAULT_TOKEN_TYPES = {
  '#ST': 'string',
  '#NR': 'number',
  '#CM': 'comment',
  '#VL': 'keyword',
  '#TX': 'string',
  '#OB': 'operator',
  '#CB': 'operator',
  '#OS': 'operator',
  '#CS': 'operator',
  '#CL': 'operator',
  '#CA': 'operator',
}

const PREFIX_TYPES = [
  [/^KW_/, 'keyword'],
  [/^LIT_/, 'string'],
  [/^TRIVIA_/, 'comment'],
  [/^PP_/, 'macro'],
  [/^PUNC_/, 'operator'],
  [/^ID$|^#ID$/, 'variable'],
]

// The fixed superset legend (design §11): hot-added grammars never
// force re-registration.
const LEGEND = [
  'string', 'number', 'comment', 'keyword', 'operator', 'variable',
  'macro', 'type', 'property',
]

function tokenType(name, overrides) {
  if (overrides && overrides[name]) return overrides[name]
  if (DEFAULT_TOKEN_TYPES[name]) return DEFAULT_TOKEN_TYPES[name]
  for (const [re, type] of PREFIX_TYPES) {
    if (re.test(name)) return type
  }
  return null
}

// Structural rules for the outline, v1 rule-name filter (design §5);
// per-entry override via entry.semanticTokens/outlineRules.
const DEFAULT_OUTLINE_RULES = { map: 'Object', list: 'Array' }

// Run one parse and derive every artifact. `inst` must have recovery
// enabled (the pipeline still works fail-fast pre-P0 via the caller's
// last-good handling — a thrown error yields diagnostics only).
function analyze(instances, inst, entry, doc) {
  const lexEvents = []
  const ruleEvents = []

  const collector = {
    lex: (tkn) => {
      if (0 <= tkn.sI) lexEvents.push(tkn)
    },
    ruleDone: (rule, ctx, done) => {
      ruleEvents.push({
        i: rule.i,
        name: rule.name,
        state: done.state,
        forced: !!done.forced,
        r: done.alt ? done.alt.r : '',
        o0: 0 < rule.oN ? { sI: rule.o0.sI, rI: rule.o0.rI, cI: rule.o0.cI, len: rule.o0.len } : null,
        c0: 0 < rule.cN ? { sI: rule.c0.sI, rI: rule.c0.rI, cI: rule.c0.cI, len: rule.c0.len } : null,
      })
    },
  }

  let value = undefined
  let errors = []
  let failed = null
  try {
    const out = instances.parse(inst, doc.text, collector)
    if (out && 'object' === typeof out && 'errors' in out && Array.isArray(out.errors)) {
      value = out.value
      errors = out.errors
    } else {
      value = out
    }
  } catch (e) {
    failed = e
    if (e && e.internal) errors = [e]
    else throw e
  }

  return {
    value,
    errors,
    failed: null != failed,
    diagnostics: diagnostics(errors, entry, doc),
    semanticTokens:
      'clean' === entry.lexStream ? semanticTokens(lexEvents, entry, doc) : null,
    outline: outline(ruleEvents, entry, doc),
  }
}

function diagnostics(errors, entry, doc) {
  const out = []
  for (const err of errors) {
    let j
    try {
      j = JSON.parse(JSON.stringify(err))
    } catch (e) {
      continue
    }
    const d = {
      range: doc.rangeFrom(j.row, j.col, j.pos, j.len),
      severity: 1, // Error
      code: j.code,
      source: 'tabnas' + (entry ? ':' + entry.languageId : ''),
      message: j.message + (j.hint ? '\n\n' + j.hint : ''),
    }
    // Registry pages exist for known codes (engine + declared owners).
    if (j.code && 'unknown' !== j.code) {
      d.codeDescription = { href: 'https://tabnas.dev/errors/' + j.code }
    }
    out.push(d)
  }
  return out
}

// Reconstruct final tokens per the documented lex-trace contract:
// newest event per position wins, spans shadow interior positions.
function reconcile(lexEvents) {
  const out = []
  const claimed = []
  for (let i = lexEvents.length - 1; 0 <= i; i--) {
    const t = lexEvents[i]
    const len = Math.max(1, t.len | 0)
    let shadowed = false
    for (const [s, e] of claimed) {
      if (t.sI >= s && t.sI < e) { shadowed = true; break }
    }
    if (shadowed) continue
    claimed.push([t.sI, t.sI + len])
    out.push(t)
  }
  return out.sort((a, b) => a.sI - b.sI)
}

function semanticTokens(lexEvents, entry, doc) {
  const overrides = entry ? entry.semanticTokens : null
  const inst = entry && entry._inst
  const data = []
  let prevLine = 0
  let prevChar = 0
  for (const t of reconcile(lexEvents)) {
    const name = t.name || (inst && String(inst.token(t.tin))) || ''
    const type = tokenType(name, overrides)
    if (null == type) continue
    const typeI = LEGEND.indexOf(type)
    if (typeI < 0) continue
    const line = Math.max(0, t.rI - 1)
    const char = Math.max(0, t.cI - 1)
    const len = Math.max(1, t.len | 0)
    const dLine = line - prevLine
    const dChar = 0 === dLine ? char - prevChar : char
    if (dLine < 0 || (0 === dLine && dChar < 0)) continue // out-of-order guard
    data.push(dLine, dChar, len, typeI, 0)
    prevLine = line
    prevChar = char
  }
  return { data, legend: LEGEND }
}

function outline(ruleEvents, entry, doc) {
  const rules = Object.assign({}, DEFAULT_OUTLINE_RULES, entry && entry.outlineRules)
  const open = new Map()
  const symbols = []
  for (const e of ruleEvents) {
    if (null == rules[e.name]) continue
    if ('o' === e.state && e.o0) {
      open.set(e.i, e)
    } else if ('c' === e.state) {
      const o = open.get(e.i)
      open.delete(e.i)
      if (o && (e.c0 || e.forced)) {
        const start = { line: o.o0.rI - 1, character: o.o0.cI - 1 }
        const endTok = e.c0 || o.o0
        const end = {
          line: endTok.rI - 1,
          character: endTok.cI - 1 + Math.max(1, endTok.len | 0),
        }
        symbols.push({
          name: rules[e.name],
          kind: 'Array' === rules[e.name] ? 18 : 19, // SymbolKind.Array/Object
          range: { start, end },
          selectionRange: { start, end: start },
          _span: [o.o0.sI, endTok.sI],
        })
      }
    }
  }
  // Nest by span containment.
  symbols.sort((a, b) => a._span[0] - b._span[0] || b._span[1] - a._span[1])
  const roots = []
  const stack = []
  for (const s of symbols) {
    s.children = []
    while (0 < stack.length && !(stack[stack.length - 1]._span[0] <= s._span[0] && s._span[1] <= stack[stack.length - 1]._span[1])) {
      stack.pop()
    }
    if (0 < stack.length) stack[stack.length - 1].children.push(s)
    else roots.push(s)
    stack.push(s)
  }
  for (const s of symbols) delete s._span
  return roots
}

// Tokens the engine may name as legal continuations that a user can
// never type. #ZZ is end-of-source: the engine returns it whenever the
// prefix parses (that is how it says "this document is already
// valid"), so it reaches completion on nearly every keystroke in a
// permissive grammar. #AA is the match-any sentinel, and #BD the
// bad-token marker — both are engine-internal.
const SENTINEL_TOKENS = new Set(['#ZZ', '#AA', '#BD'])

// Completion via the engine's continuation primitive (A6), with
// friendly labels for fixed tokens.
function completion(inst, entry, doc, position) {
  const prefix = doc.text.substring(0, doc.offsetAt(position))
  let cont
  try {
    cont = inst.continuations(prefix)
  } catch (e) {
    return []
  }
  const items = []
  for (const name of cont.tokens) {
    if (SENTINEL_TOKENS.has(name)) continue
    const fixedSrc = fixedSource(inst, name)
    items.push({
      label: fixedSrc || name,
      kind: fixedSrc ? 24 : 14, // Operator : Keyword
      detail: name,
      insertText: fixedSrc || undefined,
    })
  }
  return items
}

function fixedSource(inst, name) {
  try {
    const tin = inst.token(name)
    const src = inst.fixed(tin)
    return 'string' === typeof src ? src : null
  } catch (e) {
    return null
  }
}

module.exports = {
  analyze,
  completion,
  diagnostics,
  semanticTokens,
  outline,
  reconcile,
  tokenType,
  LEGEND,
  DEFAULT_TOKEN_TYPES,
}
