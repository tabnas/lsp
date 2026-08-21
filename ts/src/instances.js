/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Engine-instance management (design §6). One long-lived Tabnas
// instance per cache key, with:
//  - exactly ONE permanent mux subscriber installed at creation
//    (ctx.sub aliases the instance's shared list and parent_ctx
//    deep-merge mutates it in place — per-parse subscription corrupts
//    the instance), forwarding to the single active collector, wrapped
//    in an exception guard so a consumer throw cannot abort a user
//    parse;
//  - rebuild-on-reload (tn.grammar() prepends; never re-apply);
//  - per-grammar failure quarantine (a throwing grammar is disabled,
//    the server survives).
//
// Parses are SERIALIZED: the protocol layer is single-threaded and
// runs one parse at a time, so a single active-collector slot is the
// whole demux. A speculative WeakMap-keyed variant for concurrent
// parses existed here once and was dead code with a broken mux (its
// permanent subscriber never consulted the trampoline its parse()
// installed — review catch on #1); concurrency support starts from
// the design, not by resurrecting it.

const QUARANTINE_LIMIT = 3

class Instances {
  // makeInstance(entry) -> Tabnas (loader supplied by the host: module
  // require, GrammarSpec load, or BNF dialect compile).
  constructor(makeInstance) {
    this.makeInstance = makeInstance
    this.cache = new Map()
    this.failures = new Map()
    this._active = null
  }

  key(entry, folder) {
    return entry.languageId + ' ' + JSON.stringify(entry.options || {}) +
      ' ' + (folder || entry._dir || '')
  }

  // Quarantine is keyed the same way the instance cache is. Keying it
  // by languageId alone, while the cache keys on (languageId, options,
  // folder), meant one workspace folder's broken `mydsl` grammar
  // disabled every OTHER folder's working `mydsl` too — and a
  // languageId is not unique across folders by design.
  quarantined(entry, folder) {
    return QUARANTINE_LIMIT <= (this.failures.get(this.key(entry, folder)) || 0)
  }

  recordFailure(entry, folder) {
    const k = this.key(entry, folder)
    this.failures.set(k, 1 + (this.failures.get(k) || 0))
  }

  get(entry, folder) {
    if (this.quarantined(entry, folder)) return null
    const k = this.key(entry, folder)
    let inst = this.cache.get(k)
    if (!inst) {
      try {
        inst = this.makeInstance(entry)
      } catch (e) {
        this.recordFailure(entry, folder)
        throw e
      }
      this.installMux(inst)
      this.cache.set(k, inst)
    }
    return inst
  }

  // Grammar hot-reload: rebuild, never re-apply (tn.grammar prepends).
  // Clears the failure count for the same keys it drops from the cache,
  // so a reloaded grammar leaves quarantine.
  invalidate(entry) {
    const prefix = entry.languageId + ' '
    for (const k of [...this.cache.keys()]) {
      if (k.startsWith(prefix)) this.cache.delete(k)
    }
    for (const k of [...this.failures.keys()]) {
      if (k.startsWith(prefix)) this.failures.delete(k)
    }
  }

  installMux(inst) {
    const self = this
    inst.sub({
      lex: (tkn, rule, ctx) => {
        const c = self._active
        if (c && c.lex) {
          try { c.lex(tkn, rule, ctx) } catch (e) { c.err = e }
        }
      },
      ruleDone: (rule, ctx, done) => {
        const c = self._active
        if (c && c.ruleDone) {
          try { c.ruleDone(rule, ctx, done) } catch (e) { c.err = e }
        }
      },
    })
  }

  parse(inst, src, collector) {
    const prev = this._active
    this._active = collector || null
    try {
      return inst.parse(src)
    } finally {
      this._active = prev
    }
  }
}

// Historical name for the same class, kept so existing callers and
// tests keep working: serialization is now the only implementation.
const SerialInstances = Instances

module.exports = { Instances, SerialInstances, QUARANTINE_LIMIT }
