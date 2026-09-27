// Copyright (c) 2026 Richard Rodger, MIT License

//! The host-facing convenience: parse a text and get back the byte spans
//! to colour, with the LSP tokens beside them.
//!
//! Errors do not stop highlighting. The parse runs with whatever
//! recovery the parser's options enable, and the trace up to wherever
//! the lexer stopped is still reconciled and mapped; [`Highlight::partial`]
//! says when that was short of the end of the text.
//!
//! The engine has no unsubscribe API, so a recorder installed on a
//! parser stays installed for that parser's lifetime, and every parse
//! fills every recorder ever installed. [`highlight`] therefore takes
//! the parser by value: it installs one recorder, parses once and
//! consumes the instance, so a second recorder on the same parser cannot
//! be written. A host that keeps an instance and parses repeatedly uses
//! a [`Highlighter`], which owns the parser and installs the recorder
//! once.

use tabnas::{ParseRecovery, Tabnas, TabnasError};

use crate::semantic::{segments, Overrides, Segment, SemanticToken, TokenType};
use crate::trace::{reconcile, LexTrace, TokenPoint};

/// A byte range of the highlighted text and its token type. Spans never
/// cross a line: a token spanning lines yields one span per line, the
/// line terminator excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the first byte.
    pub start: usize,
    /// Byte offset one past the last byte.
    pub end: usize,
    /// The legend entry.
    pub kind: TokenType,
}

/// Everything one highlighting parse yields.
#[derive(Debug, Clone)]
pub struct Highlight {
    /// The spans to colour, in document order, non-overlapping. Only
    /// tokens the pipeline maps appear here.
    pub spans: Vec<Span>,
    /// The same tokens in LSP form, one per span.
    pub tokens: Vec<SemanticToken>,
    /// The reconciled trace: every token the parse used, mapped or not,
    /// for a host that colours a grammar's own token names itself.
    pub reconciled: Vec<TokenPoint>,
    /// The parse errors, in order. With recovery enabled these are the
    /// recovered ones plus the one the parse ended on, if it did; with
    /// recovery off, the single fail-fast error.
    pub errors: Vec<TabnasError>,
    /// The lexer did not reach the end of the text: the spans cover a
    /// prefix of it. False whenever the whole text was lexed, however
    /// many errors recovery reported along the way.
    pub partial: bool,
}

/// Parse `text` with `parser`, recording the lex trace, and return the
/// spans to colour.
///
/// The parser is consumed. The recorder this installs stays on it (the
/// engine has no unsubscribe), and an instance highlighted twice this
/// way would carry two recorders, both filled by every later parse: the
/// results would stay right while memory and time grew with every call.
/// Taking the instance by value makes that impossible to write; to parse
/// repeatedly with one instance, use a [`Highlighter`]. `overrides` is
/// the registry entry's `semanticTokens` map, `None` for the defaults
/// alone.
pub fn highlight(mut parser: Tabnas, text: &str, overrides: Option<&Overrides>) -> Highlight {
    let trace = LexTrace::install(&mut parser);
    run(&mut parser, &trace, text, overrides)
}

/// A parser with the lex-trace recorder installed once, for repeated
/// highlighting.
pub struct Highlighter {
    parser: Tabnas,
    trace: LexTrace,
    overrides: Option<Overrides>,
}

impl Highlighter {
    /// Take ownership of `parser` and install the recorder.
    pub fn new(parser: Tabnas) -> Self {
        Self::with_overrides(parser, None)
    }

    /// [`Highlighter::new`] with a registry entry's `semanticTokens`
    /// overrides.
    pub fn with_overrides(mut parser: Tabnas, overrides: Option<Overrides>) -> Self {
        let trace = LexTrace::install(&mut parser);
        Highlighter {
            parser,
            trace,
            overrides,
        }
    }

    /// Parse `text` and return the spans to colour. One parse at a time:
    /// the recorder is cleared before the parse and drained after it.
    pub fn highlight(&mut self, text: &str) -> Highlight {
        run(&mut self.parser, &self.trace, text, self.overrides.as_ref())
    }

    /// The parser this highlights with.
    pub fn parser(&self) -> &Tabnas {
        &self.parser
    }

    /// The overrides in force.
    pub fn overrides(&self) -> Option<&Overrides> {
        self.overrides.as_ref()
    }

    /// Replace the overrides.
    pub fn set_overrides(&mut self, overrides: Option<Overrides>) {
        self.overrides = overrides;
    }

    /// Give the parser back. The recorder stays installed on it, and
    /// keeps recording into a buffer this handle no longer reads.
    pub fn into_parser(self) -> Tabnas {
        self.parser
    }
}

/// One parse through an installed recorder: cleared before, drained
/// after, so no parse sees another's events.
fn run(
    parser: &mut Tabnas,
    trace: &LexTrace,
    text: &str,
    overrides: Option<&Overrides>,
) -> Highlight {
    trace.clear();
    let recovery = parser.parse_recover(text);
    build(trace.take(), recovery, overrides, text)
}

fn build(
    events: Vec<TokenPoint>,
    recovery: ParseRecovery,
    overrides: Option<&Overrides>,
    text: &str,
) -> Highlight {
    // The end-of-source token is announced at `text.len()`, so the
    // lexer reached the end exactly when some event ends there. An
    // empty text announces nothing and is trivially complete.
    let reached_end = text.is_empty()
        || events
            .iter()
            .any(|event| event.si.saturating_add(event.len) >= text.len());
    let partial = recovery.fatal.is_some() || !reached_end;
    let reconciled = reconcile(&events);
    let segments = segments(&reconciled, overrides, text);
    let spans = segments
        .iter()
        .map(|segment| Span {
            start: segment.start,
            end: segment.end,
            kind: segment.kind,
        })
        .collect();
    let tokens = segments.iter().map(Segment::token).collect();
    Highlight {
        spans,
        tokens,
        reconciled,
        errors: recovery.errors,
        partial,
    }
}
