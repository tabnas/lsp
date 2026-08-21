/* Copyright (c) 2026 Richard Rodger, MIT License */
'use strict'

// Document store: line index and position-encoding conversion. All
// encoding knowledge lives here (design §5): engine diagnostics carry
// col/pos in UTF-16 units (TS engine) but `len` in Unicode CODE
// POINTS — both must convert to the client's negotiated encoding
// before a Range is built.

class Doc {
  constructor(uri, languageId, version, text) {
    this.uri = uri
    this.languageId = languageId
    this.version = version
    this.text = text
    this._lines = null
  }

  update(text, version) {
    this.text = text
    this.version = version
    this._lines = null
  }

  // Offsets (UTF-16 units) of each line start.
  lineStarts() {
    if (!this._lines) {
      const starts = [0]
      const t = this.text
      for (let i = 0; i < t.length; i++) {
        if ('\n' === t[i]) starts.push(i + 1)
      }
      this._lines = starts
    }
    return this._lines
  }

  // Engine (row, col) [1-based, col in UTF-16 units] -> LSP Position.
  posFrom(row, col) {
    return { line: Math.max(0, row - 1), character: Math.max(0, col - 1) }
  }

  // Engine diagnostic -> LSP Range. `len` counts CODE POINTS of the
  // token source; convert to UTF-16 units by scanning the actual text
  // from the start offset (astral chars take two units each).
  rangeFrom(row, col, pos, lenCodePoints) {
    const start = this.posFrom(row, col)
    let units = 0
    let cp = 0
    const t = this.text
    let i = 0 <= pos ? pos : this.offsetAt(start)
    while (cp < lenCodePoints && i + units < t.length) {
      const code = t.codePointAt(i + units)
      units += 0xffff < code ? 2 : 1
      cp++
    }
    const endOffset = i + Math.max(units, lenCodePoints > 0 ? 1 : 0)
    return { start, end: this.positionAt(Math.min(endOffset, t.length)) }
  }

  offsetAt(position) {
    const starts = this.lineStarts()
    const line = Math.min(position.line, starts.length - 1)
    return Math.min(starts[line] + position.character, this.text.length)
  }

  positionAt(offset) {
    const starts = this.lineStarts()
    let lo = 0
    let hi = starts.length - 1
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1
      if (starts[mid] <= offset) lo = mid
      else hi = mid - 1
    }
    return { line: lo, character: offset - starts[lo] }
  }
}

class DocumentStore {
  constructor() {
    this.docs = new Map()
  }
  open(uri, languageId, version, text) {
    const d = new Doc(uri, languageId, version, text)
    this.docs.set(uri, d)
    return d
  }
  get(uri) {
    return this.docs.get(uri)
  }
  close(uri) {
    this.docs.delete(uri)
  }
}

module.exports = { Doc, DocumentStore }
