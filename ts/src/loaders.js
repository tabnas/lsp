/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Grammar loading: the dynamism ladder's L1/L2/L3 lanes (design §6).
// A registry entry's `load` field says how its grammar arrives:
//
//   { module: '@tabnas/toml' }        L1  live plugin code, require()d
//   { spec: './grammar.json' | {..} } L2  serialized GrammarSpec data
//   { grammar: './my.abnf' }          L3  BNF-dialect text, compiled
//
// Grammar CODE is trusted like any dependency the host installed —
// except workspace-supplied modules, which are gated behind an explicit
// trust flag (a workspace manifest must not be able to run npm code
// just by being opened). Grammar DATA is never trusted: every spec —
// file, inline, or compiled from BNF — passes the firewall below
// before any engine load.

const fs = require('fs')
const path = require('path')

// Caps bounding grammar LOAD cost (parse cost is bounded separately by
// parse budgets, design §10). MAX_GRAMMAR_RULES is ported from mcp;
// the others exist because a rule-count cap alone lets one rule carry
// an arbitrarily large alts array, an arbitrarily deep options tree
// (deep enough to overflow the recursive scans), or an arbitrarily
// large file — L2/L3 grammar data is untrusted, so total complexity is
// bounded, not just the rule-name count.
const MAX_GRAMMAR_RULES = 5000
const MAX_GRAMMAR_ALTS = 10000
const MAX_GRAMMAR_DEPTH = 100
const MAX_GRAMMAR_BYTES = 1000000

// The three BNF dialects, by grammar-file extension. Separate packages
// and separate dialects — ABNF's compiler rejects the other two, so
// dispatch is by extension, never "try them all".
const DIALECTS = {
  '.abnf': '@tabnas/abnf',
  '.ebnf': '@tabnas/ebnf',
  '.gbnf': '@tabnas/gbnf',
}

class LoadError extends Error {
  constructor(message, issues) {
    super(message + (issues && issues.length
      ? '\n  ' + issues.map((i) => i.path + ': ' + i.message).join('\n  ')
      : ''))
    this.name = 'LoadError'
    this.issues = issues || []
  }
}

// ---------------------------------------------------------------------
// The grammar firewall, ported from mcp/ts/src/core.ts (design §10).
// Layers: prototype-pollution keys, live-code refusals (`ref`,
// `plugins`), non-builtin @-refs, rule-count cap, schema-v gate.
// Structural junk beyond these is caught loudly by the trial load —
// tn.grammar() rejects malformed specs — so the Ajv structural pass mcp
// runs is deliberately not duplicated here.

const FORBIDDEN_KEYS = ['__proto__', 'constructor', 'prototype']

// Alt keys whose string values the engine resolves as function
// references (grammar.schema.json $defs.alt). `a` may be an array.
const ALT_FUNC_KEYS = ['b', 'p', 'r', 'a', 'e', 'h', 'c']

function scanForbiddenKeys(val, p, out, depth) {
  depth = depth || 0
  if (null === val || 'object' !== typeof val) return
  if (MAX_GRAMMAR_DEPTH < depth) {
    out.push({
      path: p,
      message: 'grammar nesting deeper than ' + MAX_GRAMMAR_DEPTH +
        ' levels: refused (no real GrammarSpec is this deep, and the ' +
        'scans must not be recursed off the stack)',
    })
    return
  }
  if (Array.isArray(val)) {
    val.forEach((v, i) => scanForbiddenKeys(v, p + '[' + i + ']', out, depth + 1))
    return
  }
  // getOwnPropertyNames, not Object.keys: JSON.parse creates __proto__
  // as a real own property that hides behind the inherited accessor.
  for (const key of Object.getOwnPropertyNames(val)) {
    const childPath = p + '.' + key
    if (FORBIDDEN_KEYS.includes(key)) {
      out.push({
        path: childPath,
        message: "forbidden key '" + key + "': refused to prevent " +
          'prototype pollution (a serialized grammar is data, never a ' +
          'route to Object.prototype)',
      })
      continue
    }
    const desc = Object.getOwnPropertyDescriptor(val, key)
    if (desc && 'value' in desc) {
      scanForbiddenKeys(desc.value, childPath, out, depth + 1)
    }
  }
}

const REF_SHAPED = /^@[A-Za-z_$][\w$.-]*$/

function makeRefScanner(builtinRefs) {
  const isBuiltin = (v) =>
    v.endsWith('$') && Object.prototype.hasOwnProperty.call(builtinRefs, v)
  const badRef = (v, p) => ({
    path: p,
    message: "unknown function reference '" + v + "': a serialized " +
      'grammar may only name $-suffixed engine builtins',
  })

  // Options strings: '@@…' (escaped literal), '@SKIP' (merge sentinel)
  // and '@/re/flags' / '@~/re/flags' (serialized RegExps) are data;
  // $-suffixed builtins pass; any other ref-shaped '@name' would be
  // resolved from a ref bag this lane refuses to accept.
  function scanOptionsRefs(val, p, out, depth) {
    depth = depth || 0
    if (MAX_GRAMMAR_DEPTH < depth || 100 < out.length) return
    if ('string' === typeof val) {
      if ('@' !== val[0] || val.startsWith('@@') || '@SKIP' === val ||
        /^@~?\/.*\/[\w]*$/.test(val) || isBuiltin(val)) return
      if (REF_SHAPED.test(val)) out.push(badRef(val, p))
      return
    }
    if (Array.isArray(val)) {
      val.forEach((v, i) => scanOptionsRefs(v, p + '[' + i + ']', out, depth + 1))
      return
    }
    if (null !== val && 'object' === typeof val) {
      for (const k of Object.keys(val)) {
        scanOptionsRefs(val[k], p + '.' + k, out, depth + 1)
      }
    }
  }

  // In alt function positions EVERY '@'-string is a reference, so the
  // rule is strict: builtin or refused.
  function scanAltRefs(alt, p, out) {
    if (null == alt || 'object' !== typeof alt || Array.isArray(alt)) return
    for (const k of ALT_FUNC_KEYS) {
      const v = alt[k]
      if ('string' === typeof v && v.startsWith('@')) {
        if (!isBuiltin(v)) out.push(badRef(v, p + '.' + k))
      } else if ('a' === k && Array.isArray(v)) {
        v.forEach((item, i) => {
          if ('string' === typeof item && item.startsWith('@') &&
            !isBuiltin(item)) out.push(badRef(item, p + '.a[' + i + ']'))
        })
      }
    }
  }

  return { scanOptionsRefs, scanAltRefs }
}

function altsOf(stateVal) {
  if (Array.isArray(stateVal)) return stateVal
  if (null != stateVal && 'object' === typeof stateVal &&
    Array.isArray(stateVal.alts)) return stateVal.alts
  return []
}

// Full firewall over a candidate spec. `parserMod` supplies the
// engine's BUILTIN_REFS and BUILTIN_SCHEMA_VERSION so the accepted
// builtin set and version ceiling are the engine's own, never a copy.
function firewallSpec(gs, parserMod) {
  if (null == gs || 'object' !== typeof gs || Array.isArray(gs)) {
    return [{ path: '$', message: 'grammar must be a JSON object (the serialized GrammarSpec form)' }]
  }
  const out = []
  scanForbiddenKeys(gs, '$', out)
  if (0 < out.length) return out // poisoned: nothing below touches it

  if ('ref' in gs) {
    out.push({
      path: '$.ref',
      message: "'ref' is not part of the serialized grammar form: live " +
        'functions are not JSON. Name $-suffixed engine builtins instead.',
    })
  }
  if (null != gs.options && 'object' === typeof gs.options &&
    !Array.isArray(gs.options) &&
    Object.prototype.hasOwnProperty.call(gs.options, 'plugins')) {
    out.push({
      path: '$.options.plugins',
      message: 'plugins cannot be supplied through a serialized grammar: ' +
        'a plugin is live code, and this lane accepts only data',
    })
  }

  const v = 'number' === typeof gs.v ? gs.v : 1
  if (v > parserMod.BUILTIN_SCHEMA_VERSION) {
    out.push({
      path: '$.v',
      message: 'grammar declares builtin schema version ' + v +
        '; this engine supports up to ' + parserMod.BUILTIN_SCHEMA_VERSION,
    })
  }

  const { scanOptionsRefs, scanAltRefs } = makeRefScanner(parserMod.BUILTIN_REFS)
  if (null != gs.options && 'object' === typeof gs.options) {
    scanOptionsRefs(gs.options, '$.options', out)
  }
  if (null != gs.rule && 'object' === typeof gs.rule) {
    const ruleNames = Object.keys(gs.rule)
    if (ruleNames.length > MAX_GRAMMAR_RULES) {
      out.push({
        path: '$.rule',
        message: 'grammar defines ' + ruleNames.length +
          ' rules, more than ' + MAX_GRAMMAR_RULES,
      })
    }
    // Total alternates are capped as well: one rule can carry an
    // arbitrarily large alts array, and the rule-count cap alone would
    // wave it through.
    let altCount = 0
    for (const rulename of ruleNames) {
      const rulespec = gs.rule[rulename]
      if (null == rulespec || 'object' !== typeof rulespec) continue
      for (const state of ['open', 'close']) {
        const alts = altsOf(rulespec[state])
        altCount += alts.length
        alts.forEach((alt, i) =>
          scanAltRefs(alt, '$.rule.' + rulename + '.' + state + '[' + i + ']', out))
      }
      if (altCount > MAX_GRAMMAR_ALTS) break
    }
    if (altCount > MAX_GRAMMAR_ALTS) {
      out.push({
        path: '$.rule',
        message: 'grammar defines more than ' + MAX_GRAMMAR_ALTS +
          ' alternates in total',
      })
    }
  }
  return out
}

// ---------------------------------------------------------------------
// Path resolution, workspace-sandboxed. A workspace manifest names
// files relative to its own folder and may not reach outside it —
// document-controlled paths are attack surface (design §10). The check
// runs on REAL paths: a lexical prefix test alone accepts `grammar
// .json` that is a symlink out of the workspace, and the read then
// follows the link (review catch on #1).

function resolveSandboxed(file, baseDir) {
  if (null == baseDir) {
    throw new LoadError('grammar file paths need a base directory: ' + file)
  }
  let base
  try {
    base = fs.realpathSync(path.resolve(baseDir))
  } catch (e) {
    throw new LoadError('workspace folder not readable: ' + baseDir + ': ' + e.message)
  }
  const abs = path.resolve(base, file)
  // Lexical gate first, so a plainly escaping RELATIVE path is refused
  // with the clear message even when its target does not exist.
  if (abs !== base && !abs.startsWith(base + path.sep)) {
    throw new LoadError(
      'grammar file escapes its workspace folder: ' + file + ' (from ' + base + ')')
  }
  let real
  try {
    real = fs.realpathSync(abs)
  } catch (e) {
    throw new LoadError('grammar file not readable: ' + file + ': ' + e.message)
  }
  if (real !== base && !real.startsWith(base + path.sep)) {
    throw new LoadError(
      'grammar file escapes its workspace folder (via symlink): ' +
        file + ' -> ' + real)
  }
  return real
}

// ---------------------------------------------------------------------
// L1: which export of a plugin module is the plugin. Export SHAPE does
// not answer that — @tabnas/jsonic exports its root instance wrapper as
// the module and the real plugin as the lowercase name, and applying
// the wrapper is silently harmless (a parse function returns non-string
// input unchanged), which serves an EMPTY grammar with no error.
// Candidates are probed on a throwaway child instance; only the export
// that actually installs rules is applied to the real one, and no
// candidate is ever applied twice.

function pluginCandidates(req, name) {
  let mod
  try {
    mod = req(name)
  } catch (e) {
    // The @tabnas/<name> fallback is for SHORT names only. A module
    // that exists but throws while initializing must surface its own
    // error — falling back would either hide it behind a not-found for
    // a name nobody asked for, or silently serve a different package.
    const notFound = 'MODULE_NOT_FOUND' === e.code &&
      String(e.message).includes("'" + name + "'")
    if (!notFound || name.startsWith('@')) throw e
    mod = req('@tabnas/' + name)
  }
  const short = String(name).replace(/^@tabnas\//, '')
  const camel = short.charAt(0).toUpperCase() + short.slice(1)
  const out = []
  const add = (fn) => {
    if ('function' === typeof fn && !out.includes(fn)) out.push(fn)
  }
  add(mod)
  if (mod) {
    add(mod.default)
    add(mod[camel])
    add(mod[short])
  }
  return out
}

function ruleNames(tn) {
  try {
    return Object.keys(tn.rule()).sort().join(',')
  } catch (e) {
    return ''
  }
}

function installsRules(tn, fn) {
  try {
    const probe = tn.make()
    const before = ruleNames(probe)
    const after = ruleNames(probe.use(fn) || probe)
    return after !== before
  } catch (e) {
    return false
  }
}

function applyPlugin(tn, req, name) {
  const candidates = pluginCandidates(req, name)
  if (0 === candidates.length) {
    throw new LoadError('tabnas-lsp: plugin is not a function: ' + name)
  }
  if (1 === candidates.length) return tn.use(candidates[0]) || tn
  for (const fn of candidates) {
    if (installsRules(tn, fn)) return tn.use(fn) || tn
  }
  // A modifier plugin legitimately installs no rules; fall back to the
  // documented preference order (bare, .default, CamelCase, lowercase).
  return tn.use(candidates[0]) || tn
}

// Grammar-layer base chains are applied host-side (toml-style: callers
// compose .use(jsonic).use(Toml)); compiler bases are library deps of
// the compiler and are NOT applied (abnf-style: .use(bnf).use(abnf)
// cannot parse .abnf documents at all).
function buildStack(entry) {
  const stack = []
  if (entry.base && 'grammar' === entry.pluginKind) stack.push(entry.base)
  stack.push(entry.name)
  return stack
}

// ---------------------------------------------------------------------
// L3: dialect compile. Each dialect package exports a converter
// (`abnfConvert` / `<dialect>Convert` / `convert`) and, in compile
// mode, `toPureSpec`. Compile with `builtins: true` explicitly — the
// default conversion is closure mode and is not data.

function dialectOf(file) {
  const ext = path.extname(file || '').toLowerCase()
  const pkg = DIALECTS[ext]
  if (!pkg) {
    throw new LoadError(
      'unknown grammar dialect ' + (ext || '(no extension)') + ' for ' + file +
        ' — expected one of: ' + Object.keys(DIALECTS).join(', '))
  }
  return { ext, pkg, dialect: ext.slice(1) }
}

function compileGrammarText(req, file, src) {
  const { pkg, dialect } = dialectOf(file)
  let mod
  try {
    mod = req(pkg)
  } catch (e) {
    throw new LoadError(
      'grammar dialect package not installed: ' + pkg +
        ' (needed to compile ' + file + '): ' + e.message)
  }
  const convert = mod[dialect + 'Convert'] || mod.convert
  if ('function' !== typeof convert) {
    throw new LoadError(pkg + ' exports no ' + dialect + 'Convert/convert function')
  }
  let spec = convert(src, { builtins: true })
  // Pure lowering when the package offers it: strips compiler marks,
  // stamps v, carries meta.provenance for rule-name canonicalization.
  if ('function' === typeof mod.toPureSpec) spec = mod.toPureSpec(spec)
  return spec
}

// ---------------------------------------------------------------------
// The loader. makeLoader(requireFn, opts) -> makeInstance(entry):
// dispatches on entry.load and returns a configured Tabnas instance
// with recovery enabled. opts.trust.workspaceModules gates L1 for
// workspace-sourced entries (default: refused).

function makeLoader(requireFn, opts) {
  const req = requireFn || require
  // opts is read at instance-make time, not captured here: the server
  // learns trust settings from initializationOptions AFTER the loader
  // is constructed, and mutates the same opts object.

  return function makeInstance(entry) {
    const trust = (opts && opts.trust) || {}
    const parserMod = req('@tabnas/parser')
    const { Tabnas } = parserMod
    // Nested merge, not a shallow spread: an entry that configures any
    // options.parse setting must not silently lose recover.enabled —
    // multi-error diagnostics are the pipeline's foundation. Explicit
    // entry recover settings still win over these defaults.
    const entryOpts = entry.options || {}
    const entryParse = entryOpts.parse || {}
    const entryRecover = entryParse.recover || {}
    const tnOpts = Object.assign({}, entryOpts, {
      parse: Object.assign({}, entryParse, {
        recover: Object.assign({ enabled: true }, entryRecover),
      }),
    })
    if (entry.syncGroups && undefined === entryRecover.syncGroups) {
      tnOpts.parse.recover.syncGroups = entry.syncGroups
    }

    const load = entry.load || { module: entry.name }

    // --- L2: serialized GrammarSpec ---
    if (null != load.spec) {
      let gs = load.spec
      if ('string' === typeof gs) {
        const file = resolveSandboxed(gs, entry._dir)
        gs = JSON.parse(readCapped(file, entry))
      }
      return installSpec(gs, entry, tnOpts)
    }

    // --- L3: BNF-dialect grammar text ---
    if (null != load.grammar) {
      const file = resolveSandboxed(load.grammar, entry._dir)
      const src = readCapped(file, entry)
      const gs = compileGrammarText(req, file, src)
      return installSpec(gs, entry, tnOpts)
    }

    // --- L1: plugin module ---
    const name = load.module || entry.name
    if ('workspace' === entry._source && true !== trust.workspaceModules) {
      throw new LoadError(
        'workspace entry ' + entry.languageId + ' loads module ' + name +
          ', which runs code from the workspace. Refused: set ' +
          'trustWorkspaceModules in initializationOptions to allow it.')
    }
    let tn = new Tabnas(tnOpts)
    for (const stackName of entry.stack || buildStack(entry)) {
      tn = applyPlugin(tn, req, stackName)
    }
    entry._inst = tn
    return tn

    function installSpec(gs, entry_, tnOpts_) {
      const issues = firewallSpec(gs, parserMod)
      if (0 < issues.length) {
        throw new LoadError(
          'grammar for ' + entry_.languageId + ' failed the firewall', issues)
      }
      let tn_ = new Tabnas(tnOpts_)
      // Composition-aware: a spec layering on a base grammar validates
      // and loads against that declared stack, not a bare engine.
      if (entry_.base && 'grammar' === entry_.pluginKind) {
        tn_ = applyPlugin(tn_, req, entry_.base)
      }
      if (entry_.stack) {
        for (const stackName of entry_.stack) {
          tn_ = applyPlugin(tn_, req, stackName)
        }
      }
      tn_.grammar(gs)
      entry_._inst = tn_
      return tn_
    }
  }
}

// Read a grammar file with the byte cap applied: the untrusted-data
// bound has to hold before JSON.parse or a dialect compile sees the
// content, not after.
function readCapped(file, entry) {
  const stat = fs.statSync(file)
  if (stat.size > MAX_GRAMMAR_BYTES) {
    throw new LoadError(
      'grammar file for ' + entry.languageId + ' is ' + stat.size +
        ' bytes, larger than ' + MAX_GRAMMAR_BYTES)
  }
  return fs.readFileSync(file, 'utf8')
}

module.exports = {
  makeLoader,
  firewallSpec,
  compileGrammarText,
  resolveSandboxed,
  LoadError,
  MAX_GRAMMAR_RULES,
  MAX_GRAMMAR_ALTS,
  MAX_GRAMMAR_DEPTH,
  MAX_GRAMMAR_BYTES,
  DIALECTS,
}
