// Copyright (c) 2026 Richard Rodger, MIT License

//! Semantic tokens from the reconciled lex trace, mirroring
//! `ts/src/core.js` (`tokenType`, `semanticTokens`) and
//! `go/semantic.go`: the CANON default map plus the prefix conventions
//! plus per-entry overrides, onto the fixed superset legend, so a
//! hot-added grammar never forces the server to re-register its legend.
//!
//! Positions in a [`SemanticToken`] are LSP units, 0-based rows and
//! UTF-16 columns (the wire form the server negotiates), with the same
//! column and length in Unicode scalar values beside them for hosts that
//! index text by character. A token spanning lines is split into
//! line-local tokens, as the TypeScript core does: multiline semantic
//! tokens are an optional client capability, and an unsplit one
//! mis-highlights or is rejected by clients without it.

use std::collections::HashMap;
use std::fmt;

use crate::trace::{reconcile, TokenPoint};

/// An LSP semantic token type, one of the nine in [`LEGEND`].
///
/// The declaration order IS the legend order: [`TokenType::index`] is
/// the discriminant, and it is what the delta-encoded `data` carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TokenType {
    String,
    Number,
    Comment,
    Keyword,
    Operator,
    Variable,
    Macro,
    Type,
    Property,
}

/// The fixed superset legend (design §11), in wire order.
pub const LEGEND: [TokenType; 9] = [
    TokenType::String,
    TokenType::Number,
    TokenType::Comment,
    TokenType::Keyword,
    TokenType::Operator,
    TokenType::Variable,
    TokenType::Macro,
    TokenType::Type,
    TokenType::Property,
];

impl TokenType {
    /// The LSP `SemanticTokenTypes` name.
    pub const fn name(self) -> &'static str {
        match self {
            TokenType::String => "string",
            TokenType::Number => "number",
            TokenType::Comment => "comment",
            TokenType::Keyword => "keyword",
            TokenType::Operator => "operator",
            TokenType::Variable => "variable",
            TokenType::Macro => "macro",
            TokenType::Type => "type",
            TokenType::Property => "property",
        }
    }

    /// The position in [`LEGEND`], which is what `data` encodes.
    pub const fn index(self) -> u32 {
        self as u32
    }

    /// The legend entry with this name, if any.
    pub fn from_name(name: &str) -> Option<TokenType> {
        LEGEND.iter().copied().find(|kind| kind.name() == name)
    }

    /// The legend entry at this index, if any.
    pub fn from_index(index: u32) -> Option<TokenType> {
        LEGEND.get(index as usize).copied()
    }
}

impl fmt::Display for TokenType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Engine-standard token names (railroad's CANON key set) to token
/// types. `#ID` is per-plugin and comes from an entry's overrides.
pub const DEFAULT_TOKEN_TYPES: [(&str, TokenType); 11] = [
    ("#ST", TokenType::String),
    ("#NR", TokenType::Number),
    ("#CM", TokenType::Comment),
    ("#VL", TokenType::Keyword),
    ("#TX", TokenType::String),
    ("#OB", TokenType::Operator),
    ("#CB", TokenType::Operator),
    ("#OS", TokenType::Operator),
    ("#CS", TokenType::Operator),
    ("#CL", TokenType::Operator),
    ("#CA", TokenType::Operator),
];

/// The prefix conventions for grammars with their own token schemes
/// (design §5), tried in order after the defaults. `ID` and `#ID`, the
/// identifier convention, map to `variable` and are matched whole rather
/// than by prefix; see [`prefix_token_type`].
pub const PREFIX_TYPES: [(&str, TokenType); 5] = [
    ("KW_", TokenType::Keyword),
    ("LIT_", TokenType::String),
    ("TRIVIA_", TokenType::Comment),
    ("PP_", TokenType::Macro),
    ("PUNC_", TokenType::Operator),
];

/// A registry entry's `semanticTokens` map: engine token name to LSP
/// token type name. A type outside the legend drops the token.
pub type Overrides = HashMap<String, String>;

/// The CANON default for an engine-standard token name.
pub fn default_token_type(name: &str) -> Option<TokenType> {
    DEFAULT_TOKEN_TYPES
        .iter()
        .find(|(canon, _)| *canon == name)
        .map(|&(_, kind)| kind)
}

/// The prefix convention a token name follows, if any.
pub fn prefix_token_type(name: &str) -> Option<TokenType> {
    if let Some(&(_, kind)) = PREFIX_TYPES
        .iter()
        .find(|(prefix, _)| name.starts_with(prefix))
    {
        return Some(kind);
    }
    if name == "ID" || name == "#ID" {
        return Some(TokenType::Variable);
    }
    None
}

/// Resolve an engine token name to a token TYPE NAME: the entry's
/// override when it names one, else the CANON default, else the prefix
/// convention; `None` for a token the pipeline does not colour. This is
/// the TypeScript `tokenType`, and like it the override is returned as
/// written, legend member or not: [`token_type`] is where a type outside
/// the legend becomes "no token", which is how both other runtimes
/// treat it.
pub fn token_type_name<'a>(name: &'a str, overrides: Option<&'a Overrides>) -> Option<&'a str> {
    if let Some(kind) = overrides.and_then(|map| map.get(name)) {
        // An empty override is no override (the TypeScript check is on
        // truthiness), so it falls through to the defaults.
        if !kind.is_empty() {
            return Some(kind.as_str());
        }
    }
    default_token_type(name)
        .or_else(|| prefix_token_type(name))
        .map(TokenType::name)
}

/// Resolve an engine token name to a legend entry, `None` for a token
/// the pipeline does not colour (unmapped, or overridden to a type the
/// legend does not carry).
pub fn token_type(name: &str, overrides: Option<&Overrides>) -> Option<TokenType> {
    token_type_name(name, overrides).and_then(TokenType::from_name)
}

/// One line-local semantic token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticToken {
    /// 0-based line.
    pub row: usize,
    /// 0-based column, in UTF-16 code units (the LSP wire unit).
    pub col: usize,
    /// Length in UTF-16 code units.
    pub len: usize,
    /// The same column in Unicode scalar values.
    pub col_chars: usize,
    /// The same length in Unicode scalar values.
    pub len_chars: usize,
    /// The legend entry.
    pub kind: TokenType,
}

/// One line-local piece of a mapped token, in every unit a host needs:
/// the LSP row and UTF-16 column and length, the same in Unicode scalar
/// values, and the byte range in the source. [`semantic_tokens`] and
/// `highlight` are both views of this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Segment {
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) len: usize,
    pub(crate) col_chars: usize,
    pub(crate) len_chars: usize,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) kind: TokenType,
}

impl Segment {
    pub(crate) fn token(&self) -> SemanticToken {
        SemanticToken {
            row: self.row,
            col: self.col,
            len: self.len,
            col_chars: self.col_chars,
            len_chars: self.len_chars,
            kind: self.kind,
        }
    }
}

/// Byte offsets of each line start, for converting engine rows and
/// columns. Every conversion walks the actual text, because an astral
/// code point is one scalar value and TWO UTF-16 units. Mirrors
/// `go/documents.go`.
struct LineIndex<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|&(_, byte)| byte == b'\n')
                .map(|(index, _)| index + 1),
        );
        LineIndex { text, starts }
    }

    /// The byte range `[start, end)` of a 0-based line, excluding its
    /// terminator (`\n`, or `\r\n`). A line past the end clamps to the
    /// last line.
    fn span(&self, line: usize) -> (usize, usize) {
        let line = line.min(self.starts.len() - 1);
        let start = self.starts[line];
        let mut end = match self.starts.get(line + 1) {
            Some(&next) => next - 1,
            None => self.text.len(),
        };
        if end > start && self.text.as_bytes()[end - 1] == b'\r' {
            end -= 1;
        }
        (start, end)
    }

    /// An engine `(row, col)`, 1-based with the column in scalar values,
    /// as `(line, utf16 column, scalar column)`.
    fn position(&self, ri: usize, ci: usize) -> (usize, usize, usize) {
        let line = ri.saturating_sub(1);
        let (start, end) = self.span(line);
        let want = ci.saturating_sub(1);
        let mut units = 0;
        let mut chars = 0;
        for c in self.text[start..end].chars() {
            if chars >= want {
                break;
            }
            units += c.len_utf16();
            chars += 1;
        }
        (line, units, chars)
    }
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Emits segments in document order, dropping any that would step
/// backwards (the out-of-order guard the delta encoding needs), and
/// never an empty one.
struct Emitter {
    out: Vec<Segment>,
    prev_row: usize,
    prev_col: usize,
}

impl Emitter {
    fn emit(&mut self, segment: Segment) {
        if segment.len < 1 {
            return;
        }
        if segment.row < self.prev_row
            || (segment.row == self.prev_row && segment.col < self.prev_col)
        {
            return;
        }
        self.prev_row = segment.row;
        self.prev_col = segment.col;
        self.out.push(segment);
    }
}

/// Map already-reconciled tokens to line-local segments.
pub(crate) fn segments(
    reconciled: &[TokenPoint],
    overrides: Option<&Overrides>,
    text: &str,
) -> Vec<Segment> {
    let lines = LineIndex::new(text);
    let mut emitter = Emitter {
        out: Vec::new(),
        prev_row: 0,
        prev_col: 0,
    };
    for token in reconciled {
        let Some(kind) = token_type(&token.name, overrides) else {
            continue;
        };
        let (row, col, col_chars) = lines.position(token.ri, token.ci);
        if token.src.contains('\n') {
            // A token spanning lines (multiline string, block comment)
            // becomes one segment per line. A `\r` before the `\n` is
            // the line terminator's, not the token's.
            let mut offset = token.si;
            for (i, part) in token.src.split('\n').enumerate() {
                let seg = part.strip_suffix('\r').unwrap_or(part);
                let start = offset.min(text.len());
                emitter.emit(Segment {
                    row: row + i,
                    col: if i == 0 { col } else { 0 },
                    len: utf16_len(seg),
                    col_chars: if i == 0 { col_chars } else { 0 },
                    len_chars: seg.chars().count(),
                    start,
                    end: (start + seg.len()).min(text.len()),
                    kind,
                });
                offset += part.len() + 1;
            }
        } else {
            let end = (token.si + token.span()).min(text.len());
            emitter.emit(Segment {
                row,
                col,
                len: utf16_len(&token.src).max(1),
                col_chars,
                len_chars: token.src.chars().count().max(1),
                start: token.si.min(end),
                end,
                kind,
            });
        }
    }
    emitter.out
}

/// The semantic tokens of a document from its raw lex trace: the events
/// are [`reconcile`]d, mapped through [`token_type`] with the entry's
/// `overrides`, and positioned against `text`, the source that was
/// parsed. Tokens the pipeline does not colour are absent; a token
/// spanning lines is split per line.
pub fn semantic_tokens(
    events: &[TokenPoint],
    overrides: Option<&Overrides>,
    text: &str,
) -> Vec<SemanticToken> {
    semantic_tokens_reconciled(&reconcile(events), overrides, text)
}

/// [`semantic_tokens`] for a trace that is already reconciled.
pub fn semantic_tokens_reconciled(
    reconciled: &[TokenPoint],
    overrides: Option<&Overrides>,
    text: &str,
) -> Vec<SemanticToken> {
    segments(reconciled, overrides, text)
        .iter()
        .map(Segment::token)
        .collect()
}

/// The LSP `SemanticTokens.data` form: five integers per token, the row
/// and column as deltas from the previous token (the column relative to
/// the line start when the row changes), the UTF-16 length, the legend
/// index, and no modifiers. A token that would step backwards is
/// dropped, as the canonical pipeline drops it.
pub fn encode(tokens: &[SemanticToken]) -> Vec<u32> {
    let mut data = Vec::with_capacity(tokens.len() * 5);
    let mut prev_row = 0;
    let mut prev_col = 0;
    for token in tokens {
        if token.len < 1 || token.row < prev_row || (token.row == prev_row && token.col < prev_col)
        {
            continue;
        }
        let d_row = token.row - prev_row;
        let d_col = if d_row == 0 {
            token.col - prev_col
        } else {
            token.col
        };
        data.extend([
            d_row as u32,
            d_col as u32,
            token.len as u32,
            token.kind.index(),
            0,
        ]);
        prev_row = token.row;
        prev_col = token.col;
    }
    data
}

/// [`semantic_tokens`], delta-encoded.
pub fn semantic_tokens_data(
    events: &[TokenPoint],
    overrides: Option<&Overrides>,
    text: &str,
) -> Vec<u32> {
    encode(&semantic_tokens(events, overrides, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(name: &str, si: usize, ri: usize, ci: usize, src: &str) -> TokenPoint {
        TokenPoint {
            name: name.into(),
            si,
            ri,
            ci,
            len: src.len(),
            src: src.into(),
        }
    }

    fn rows(tokens: &[SemanticToken]) -> Vec<(usize, usize, usize, &'static str)> {
        tokens
            .iter()
            .map(|t| (t.row, t.col, t.len, t.kind.name()))
            .collect()
    }

    #[test]
    fn the_legend_is_in_wire_order() {
        let names: Vec<&str> = LEGEND.iter().map(|kind| kind.name()).collect();
        assert_eq!(
            names,
            [
                "string", "number", "comment", "keyword", "operator", "variable", "macro", "type",
                "property"
            ]
        );
        for (i, kind) in LEGEND.iter().enumerate() {
            assert_eq!(kind.index() as usize, i);
            assert_eq!(TokenType::from_index(i as u32), Some(*kind));
            assert_eq!(TokenType::from_name(kind.name()), Some(*kind));
            assert_eq!(kind.to_string(), kind.name());
        }
        assert_eq!(TokenType::from_index(9), None);
        assert_eq!(TokenType::from_name("nope"), None);
    }

    #[test]
    fn canon_defaults_map_the_engine_standard_tokens() {
        assert_eq!(token_type("#ST", None), Some(TokenType::String));
        assert_eq!(token_type("#TX", None), Some(TokenType::String));
        assert_eq!(token_type("#NR", None), Some(TokenType::Number));
        assert_eq!(token_type("#CM", None), Some(TokenType::Comment));
        assert_eq!(token_type("#VL", None), Some(TokenType::Keyword));
        for punct in ["#OB", "#CB", "#OS", "#CS", "#CL", "#CA"] {
            assert_eq!(
                token_type(punct, None),
                Some(TokenType::Operator),
                "{punct}"
            );
        }
        for uncoloured in ["#SP", "#LN", "#ZZ", "#BD", "#AA", "#UK"] {
            assert_eq!(token_type(uncoloured, None), None, "{uncoloured}");
        }
    }

    #[test]
    fn prefix_conventions_apply_after_the_defaults() {
        assert_eq!(token_type("KW_if", None), Some(TokenType::Keyword));
        assert_eq!(token_type("LIT_str", None), Some(TokenType::String));
        assert_eq!(token_type("TRIVIA_ws", None), Some(TokenType::Comment));
        assert_eq!(token_type("PP_define", None), Some(TokenType::Macro));
        assert_eq!(token_type("PUNC_semi", None), Some(TokenType::Operator));
        assert_eq!(token_type("ID", None), Some(TokenType::Variable));
        assert_eq!(token_type("#ID", None), Some(TokenType::Variable));
        // Whole-name matches, not prefixes.
        assert_eq!(token_type("IDENT", None), None);
        assert_eq!(token_type("#IDX", None), None);
        assert_eq!(token_type("#PL", None), None);
    }

    #[test]
    fn overrides_win_and_may_drop_a_token() {
        let overrides: Overrides = [
            ("#ST".to_string(), "property".to_string()),
            ("#PL".to_string(), "type".to_string()),
            ("#NR".to_string(), "nope".to_string()),
            ("#VL".to_string(), String::new()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            token_type("#ST", Some(&overrides)),
            Some(TokenType::Property)
        );
        assert_eq!(token_type("#PL", Some(&overrides)), Some(TokenType::Type));
        // Named but outside the legend: the name is reported, the token
        // is not coloured.
        assert_eq!(token_type_name("#NR", Some(&overrides)), Some("nope"));
        assert_eq!(token_type("#NR", Some(&overrides)), None);
        // An empty override is no override.
        assert_eq!(
            token_type("#VL", Some(&overrides)),
            Some(TokenType::Keyword)
        );
        // Untouched names keep their defaults.
        assert_eq!(
            token_type("#OB", Some(&overrides)),
            Some(TokenType::Operator)
        );
    }

    #[test]
    fn columns_are_utf16_with_scalar_values_beside_them() {
        // {"é":"😀","b":1}   é is one unit, the emoji two.
        let text = "{\"\u{e9}\":\"\u{1F600}\",\"b\":1}";
        let events = vec![
            point("#OB", 0, 1, 1, "{"),
            point("#ST", 1, 1, 2, "\"\u{e9}\""),
            point("#CL", 4, 1, 5, ":"),
            point("#ST", 5, 1, 6, "\"\u{1F600}\""),
            point("#CA", 11, 1, 9, ","),
            point("#ST", 12, 1, 10, "\"b\""),
            point("#CL", 15, 1, 13, ":"),
            point("#NR", 16, 1, 14, "1"),
            point("#CB", 17, 1, 15, "}"),
        ];
        let tokens = semantic_tokens(&events, None, text);
        assert_eq!(
            rows(&tokens),
            [
                (0, 0, 1, "operator"),
                (0, 1, 3, "string"),
                (0, 4, 1, "operator"),
                (0, 5, 4, "string"),
                (0, 9, 1, "operator"),
                (0, 10, 3, "string"),
                (0, 13, 1, "operator"),
                (0, 14, 1, "number"),
                (0, 15, 1, "operator"),
            ]
        );
        let emoji = &tokens[3];
        assert_eq!((emoji.col_chars, emoji.len_chars), (5, 3));
        let comma = &tokens[4];
        assert_eq!((comma.col_chars, comma.len_chars), (8, 1));
    }

    #[test]
    fn a_token_spanning_lines_is_split_per_line() {
        let text = "{\"a\":`one\r\n\r\ntwo`}";
        let events = vec![
            point("#OB", 0, 1, 1, "{"),
            point("#ST", 1, 1, 2, "\"a\""),
            point("#CL", 4, 1, 5, ":"),
            point("#ST", 5, 1, 6, "`one\r\n\r\ntwo`"),
            point("#CB", 17, 3, 5, "}"),
        ];
        let tokens = semantic_tokens(&events, None, text);
        // The empty middle line yields nothing; the `\r`s are not counted.
        assert_eq!(
            rows(&tokens),
            [
                (0, 0, 1, "operator"),
                (0, 1, 3, "string"),
                (0, 4, 1, "operator"),
                (0, 5, 4, "string"),
                (2, 0, 4, "string"),
                (2, 4, 1, "operator"),
            ]
        );
        let segs = segments(&reconcile(&events), None, text);
        assert_eq!(&text[segs[3].start..segs[3].end], "`one");
        assert_eq!(&text[segs[4].start..segs[4].end], "two`");
    }

    #[test]
    fn encode_is_the_lsp_delta_form() {
        let text = "1\n 22";
        let events = vec![point("#NR", 0, 1, 1, "1"), point("#NR", 3, 2, 2, "22")];
        let tokens = semantic_tokens(&events, None, text);
        assert_eq!(encode(&tokens), [0, 0, 1, 1, 0, 1, 1, 2, 1, 0]);
        assert_eq!(semantic_tokens_data(&events, None, text), encode(&tokens));
        assert_eq!(encode(&[]), Vec::<u32>::new());
    }

    #[test]
    fn a_token_stepping_backwards_is_dropped() {
        // Engine positions that disagree with the byte order cannot be
        // delta-encoded; the guard drops the offender, as the canonical
        // pipeline does, rather than emitting a negative delta.
        let text = "1 2";
        let events = vec![point("#NR", 0, 1, 3, "1"), point("#NR", 2, 1, 1, "2")];
        let tokens = semantic_tokens(&events, None, text);
        assert_eq!(rows(&tokens), [(0, 2, 1, "number")]);
        let backwards = vec![
            SemanticToken {
                row: 1,
                col: 0,
                len: 1,
                col_chars: 0,
                len_chars: 1,
                kind: TokenType::Number,
            },
            SemanticToken {
                row: 0,
                col: 0,
                len: 1,
                col_chars: 0,
                len_chars: 1,
                kind: TokenType::Number,
            },
        ];
        assert_eq!(encode(&backwards), [1, 0, 1, 1, 0]);
    }

    #[test]
    fn positions_past_the_text_clamp_instead_of_panicking() {
        // A row past the last line keeps its number but is measured
        // against the last line, and a column past its end stops at the
        // end (the Go conversion's clamping, mirrored).
        let text = "1";
        let events = vec![point("#NR", 0, 5, 9, "1"), point("#ZZ", 1, 1, 2, "")];
        let tokens = semantic_tokens(&events, None, text);
        assert_eq!(rows(&tokens), [(4, 1, 1, "number")]);
        assert!(semantic_tokens(&[], None, "").is_empty());
    }
}
