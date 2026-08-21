#!/usr/bin/env node
/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// tabnas-lsp --stdio : the unified tabnas language server.
const { startServer } = require('../src/server')
startServer()
