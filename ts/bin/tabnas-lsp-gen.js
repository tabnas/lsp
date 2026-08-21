#!/usr/bin/env node
/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// tabnas-lsp-gen: generate a standalone single-language LSP server —
// Node package or Go module — plus editor plugins, from one grammar
// (design §7). A deliberately separate bin from tabnas-lsp: a
// generator argument bug must never break editor launches.

const { generate, ALL_EDITORS } = require('../src/generate')

const USAGE = `usage: tabnas-lsp-gen --out <dir> <input> [options]

input (exactly one):
  --entry <languageId>     a bundled fleet registry entry
  --module <npm-name>      a tabnas plugin module (Node runtime only)
  --spec <file.json>       a serialized GrammarSpec (pure data)
  --grammar <file>         BNF-dialect text (.abnf | .ebnf | .gbnf)

options:
  --out <dir>              target directory (required)
  --language-id <id>       served language id (derived when omitted)
  --extensions <.a,.b>     file extensions (default: .<language-id>)
  --runtime <node|go>      server runtime (default: node)
  --editors <list|none>    ${ALL_EDITORS.join(',')} (default: all)
  --unified                editor plugins for the whole bundled
                           registry over tabnas-lsp itself (no server)

go runtime:
  --go-module <path>       generated module path
  --go-plugin <import>     Go plugin package (closure/imperative grammars)
  --go-plugin-func <Name>  plugin function (default: CamelCased id)
  --go-replace <mod=dir>   replace directive for local dev (repeatable)
`

function parseArgs(argv) {
  const opts = { input: {}, goReplace: [] }
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    const next = () => {
      if (i + 1 >= argv.length) throw new Error(a + ' needs a value')
      return argv[++i]
    }
    switch (a) {
      case '--out': opts.out = next(); break
      case '--entry': opts.input.entry = next(); break
      case '--module': opts.input.module = next(); break
      case '--spec': opts.input.spec = next(); break
      case '--grammar': opts.input.grammar = next(); break
      case '--language-id': opts.languageId = next(); break
      case '--extensions':
        opts.extensions = next().split(',').map((x) =>
          x.startsWith('.') ? x : '.' + x)
        break
      case '--runtime': opts.runtime = next(); break
      case '--editors': {
        const v = next()
        opts.editors = 'none' === v ? [] : v.split(',')
        break
      }
      case '--unified': opts.unified = true; break
      case '--go-module': opts.goModule = next(); break
      case '--go-plugin': opts.goPlugin = next(); break
      case '--go-plugin-func': opts.goPluginFunc = next(); break
      case '--go-replace': opts.goReplace.push(next()); break
      case '--help': case '-h':
        console.log(USAGE)
        process.exit(0)
        break
      default:
        throw new Error('unknown option: ' + a + '\n\n' + USAGE)
    }
  }
  return opts
}

try {
  const opts = parseArgs(process.argv.slice(2))
  if (!opts.out) throw new Error('--out is required\n\n' + USAGE)
  const inputs = Object.keys(opts.input).length
  if (!opts.unified && 1 !== inputs) {
    throw new Error('exactly one input is required\n\n' + USAGE)
  }
  const { files, out } = generate(opts)
  console.log('generated ' + files.length + ' files under ' + out + ':')
  for (const f of files) console.log('  ' + f)
} catch (e) {
  console.error('tabnas-lsp-gen: ' + e.message)
  process.exitCode = 1
}
