//! Lexical layer: source text → document rows (spec §2): the header record, then one
//! record, attribute, verbatim or comment row per line, with `E027`/`E028` findings.
//! Shapes, ranks and keys are checked afterwards by `reader.rs`.
//!
//! `simd-loops` triage: lines average under 60 bytes and the lexer is a state chain,
//! so the only wide scan is the LF search, which `str::find` already does with memchr.
//!
//! Contents:
//! - `pub(crate) const DIAGNOSTICS_MAX: usize`
//! - `pub(crate) fn lex<'src>(src: &'src str, diagnostics: &mut Vec<Diagnostic>)
//!   -> Result<Document<'src>, Diagnostic>`
//! - `pub(crate) fn is_bare_byte(c: u8) -> bool`

use crate::diag::{Code, Diagnostic, NO_RECORD};
use crate::document;
use crate::document::{Document, Str, UnknownWord, VERSION};
use crate::schema::{FileKind, RecordKind};

/// Errors collected before the reader stops looking for more.
pub(crate) const DIAGNOSTICS_MAX: usize = 64;

/// An error inside one line.
struct LexError {
    /// Byte index within the line.
    col: usize,
    detail: &'static str,
}

/// Lexing state of one file: the document being filled and the line errors.
struct Lexer<'src, 'out> {
    doc: Document<'src>,
    diagnostics: &'out mut Vec<Diagnostic>,
    /// Comments waiting for the next record.
    pending: Vec<Str>,
    /// Inside a rejected record: its indented lines are dropped, not misattributed.
    skipping: bool,
}

/// Lexes the whole file: the header (whose failure is the only diagnostic), then every
/// line. Returns the document and appends the line errors (at most [`DIAGNOSTICS_MAX`])
/// to `diagnostics`, which must be empty.
pub(crate) fn lex<'src>(
    src: &'src str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Document<'src>, Diagnostic> {
    assert!(diagnostics.is_empty(), "lexing starts the diagnostics");
    let (doc, body) = lex_header(src)?;
    let mut lexer = Lexer {
        doc,
        diagnostics,
        pending: Vec::new(),
        skipping: false,
    };
    lexer.lex_lines(body);
    assert!(lexer.diagnostics.len() <= DIAGNOSTICS_MAX);
    assert!(
        lexer.pending.is_empty(),
        "trailing comments became end-of-file comments"
    );
    Ok(lexer.doc)
}

/// Returns whether `c` may appear in a bare value (spec `bchar`, bytewise; bytes of
/// multi-byte UTF-8 characters are all ≥ 0x80 and allowed).
pub(crate) fn is_bare_byte(c: u8) -> bool {
    c > b' ' && c != b'"' && c != b'\\' && c != 0x7f
}

// ---- private ----

/// Lexes line 1, `schemify <version> <kind>`, and returns the document with its header
/// record plus the byte offset where line 2 starts.
fn lex_header(src: &str) -> Result<(Document<'_>, usize), Diagnostic> {
    let bad = |col: usize, detail| Diagnostic {
        code: Code::E028,
        line: 1,
        col: col as u32 + 1,
        record: NO_RECORD,
        detail,
    };
    let nl = src.find('\n');
    let body = nl.map_or(src.len(), |i| i + 1);
    let line = &src[..nl.unwrap_or(src.len())];
    let line = line.strip_suffix('\r').unwrap_or(line);
    if !line.starts_with("schemify") {
        return Err(bad(0, "line 1 must be `schemify <version> <kind>`"));
    }
    let col = |t: &str| t.as_ptr() as usize - line.as_ptr() as usize;
    let mut tokens = line.split([' ', '\t']).filter(|t| !t.is_empty());
    let (_, version, kind) = (tokens.next(), tokens.next(), tokens.next());
    let (Some(version), Some(kind)) = (version, kind) else {
        return Err(bad(0, "line 1 must be `schemify <version> <kind>`"));
    };
    if let Some(extra) = tokens.next() {
        return Err(bad(col(extra), "unexpected text after the header kind"));
    }
    if line.as_bytes()[8] != b' ' && line.as_bytes()[8] != b'\t' {
        return Err(bad(8, "line 1 must be `schemify <version> <kind>`"));
    }
    let number = version
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| version.parse::<u16>().ok())
        .flatten();
    let Some(number) = number.filter(|&v| v >= 1) else {
        return Err(bad(
            col(version),
            "version must be a positive integer below 65536",
        ));
    };
    let Some(file_kind) = FileKind::from_word(kind) else {
        return Err(bad(col(kind), "unknown file kind"));
    };
    let mut doc = Document::bare(src, number, file_kind);
    doc.reserve_for(src.len());
    doc.begin(RecordKind::Header, 1);
    // Stored canonical (`01` → `1`) so the writer needs no special case for the header.
    let v = doc.intern(&number.to_string());
    let k = doc.intern(file_kind.word());
    doc.push_field_str(v, 0);
    doc.push_field_str(k, 0);
    assert_eq!(doc.record_count(), 1);
    assert!(body <= src.len());
    assert!(body > 8, "line 2 starts after `schemify `");
    Ok((doc, body))
}

impl Lexer<'_, '_> {
    fn lex_lines(&mut self, body: usize) {
        let src = self.doc.src;
        assert!(body <= src.len());
        assert_eq!(self.doc.record_count(), 1, "only the header is lexed yet");
        let mut pos = body;
        let mut line_no: u32 = 1;
        while pos < src.len() && self.diagnostics.len() < DIAGNOSTICS_MAX {
            line_no += 1;
            let end = src[pos..].find('\n').map_or(src.len(), |i| pos + i);
            let text_end = if end > pos && src.as_bytes()[end - 1] == b'\r' {
                end - 1
            } else {
                end
            };
            self.lex_line(pos, text_end, line_no);
            assert!(end + 1 > pos, "line loop must advance");
            pos = end + 1;
        }
        self.doc.eof_comments.append(&mut self.pending);
        assert!(self.pending.is_empty());
        assert!(
            self.diagnostics.len() <= DIAGNOSTICS_MAX,
            "at most one error per line, loop stops at the cap"
        );
    }

    fn error(&mut self, code: Code, line: u32, col: usize, detail: &'static str) {
        assert!(line >= 2, "lexer errors follow the header");
        assert!(!code.is_warning(), "the lexer reports errors only");
        self.diagnostics.push(Diagnostic {
            code,
            line,
            col: col as u32 + 1,
            record: NO_RECORD,
            detail,
        });
    }

    fn lex_line(&mut self, start: usize, end: usize, line_no: u32) {
        let src = self.doc.src;
        assert!(start <= end);
        assert!(end <= src.len());
        let line = &src[start..end];
        assert!(
            end == src.len() || matches!(src.as_bytes()[end], b'\n' | b'\r'),
            "one line at a time"
        );
        let b = line.as_bytes();
        let indent = b.iter().take_while(|&&c| c == b' ' || c == b'\t').count();
        if indent == b.len() {
            return; // blank
        }
        if b[indent] == b'#' {
            let text = line[indent + 1..].trim_end_matches([' ', '\t']);
            if let Some(i) = text.find('\r') {
                return self.error(Code::E028, line_no, indent + 1 + i, "bare CR in a comment");
            }
            let off = start + indent + 1;
            self.pending.push(Str::src(off, off + text.len()));
        } else if indent == 0 {
            self.lex_record(start, line, line_no);
        } else if self.skipping {
            // Indented line of a rejected record: already reported once.
        } else if b[indent] == b'|' {
            self.lex_verbatim(start, line, indent, line_no);
        } else {
            self.lex_attr(start, line, indent, line_no);
        }
    }

    fn lex_record(&mut self, start: usize, line: &str, line_no: u32) {
        assert!(!line.is_empty());
        assert!(
            !line.starts_with([' ', '\t']),
            "a record line starts in column 0"
        );
        self.skipping = true;
        let word_end = match lex_word(line.as_bytes(), 0) {
            Ok(e) => e,
            Err(e) => return self.error(Code::E028, line_no, e.col, e.detail),
        };
        // Values lex straight into the field pool; a rejected line truncates them away.
        let first = self.doc.fields.len();
        let doc = &mut self.doc;
        let lexed = lex_values(start, line, word_end, &mut doc.arena, &mut doc.fields);
        let word = &line[..word_end];
        let kind = match (lexed, doc.kind.lookup(word)) {
            (Err(e), _) => Err((Code::E028, e.col, e.detail)),
            (Ok(()), Some(k)) => Ok(k),
            // A newer version (spec §5) or a journal (no known words): kept as written.
            (Ok(()), None) if doc.version > VERSION || doc.kind == FileKind::Journal => {
                Ok(RecordKind::Unknown)
            }
            (Ok(()), None) if word == "schemify" => Err((Code::E028, 0, "second header line")),
            (Ok(()), None) => Err((Code::E027, 0, "record word not known for this file kind")),
        };
        let kind = match kind {
            Ok(k) => k,
            Err((code, col, detail)) => {
                self.doc.fields.truncate(first);
                return self.error(code, line_no, col, detail);
            }
        };
        let r = self.doc.begin(kind, line_no);
        self.doc.record_field_start[r as usize] = first as u32;
        if kind == RecordKind::Unknown {
            self.doc.unknown_words.push(UnknownWord {
                record: r,
                word: Str::src(start, start + word_end),
            });
        }
        self.doc.comments.append(&mut self.pending);
        assert!(self.doc.field_spans(r).len() <= line.len());
        self.skipping = false;
    }

    fn lex_attr(&mut self, start: usize, line: &str, indent: usize, line_no: u32) {
        assert!(indent > 0, "an attribute line is indented");
        assert!(indent < line.len(), "a blank line is not an attribute");
        assert!(!self.skipping);
        let key_end = match lex_word(line.as_bytes(), indent) {
            Ok(e) => e,
            Err(e) => return self.error(Code::E028, line_no, e.col, e.detail),
        };
        let first = self.doc.attr_values.len();
        let doc = &mut self.doc;
        if let Err(e) = lex_values(start, line, key_end, &mut doc.arena, &mut doc.attr_values) {
            self.doc.attr_values.truncate(first);
            return self.error(Code::E028, line_no, e.col, e.detail);
        }
        let key = Str::src(start + indent, start + key_end);
        let a = self.doc.push_attr_str(key, line_no);
        self.doc.attr_value_start[a as usize] = first as u32;
        assert!(self.doc.value_spans(a).len() <= line.len());
    }

    fn lex_verbatim(&mut self, start: usize, line: &str, indent: usize, line_no: u32) {
        assert_eq!(line.as_bytes()[indent], b'|');
        assert!(indent > 0, "a verbatim line is indented");
        assert!(!self.skipping);
        let rest = &line[indent + 1..];
        let (off, text) = match rest.strip_prefix(' ') {
            Some(t) => (indent + 2, t),
            None if rest.is_empty() => (indent + 1, rest),
            None => {
                return self.error(
                    Code::E028,
                    line_no,
                    indent + 1,
                    "verbatim text must follow `| `",
                );
            }
        };
        if let Some(i) = text.find('\r') {
            return self.error(Code::E028, line_no, off + i, "bare CR in a verbatim line");
        }
        let span = Str::src(start + off, start + off + text.len());
        self.doc.push_verbatim_str(span);
        assert_eq!(span.len as usize, text.len());
    }
}

/// Lexes `*(WS value) *WSP` from byte `i` of `line` (source byte `start`) onto `out`;
/// unescaped values go to `arena`. On error `out` may hold part of the line, which the
/// caller truncates.
fn lex_values(
    start: usize,
    line: &str,
    mut i: usize,
    arena: &mut String,
    out: &mut Vec<Str>,
) -> Result<(), LexError> {
    assert!(i <= line.len());
    assert!(
        start + line.len() < 1 << 31,
        "source spans address 2^31 bytes"
    );
    let before = out.len();
    let b = line.as_bytes();
    loop {
        let ws = i;
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        if i == b.len() {
            assert!(out.len() - before <= b.len(), "every value consumes input");
            return Ok(());
        }
        if i == ws {
            return Err(LexError {
                col: i,
                detail: "values must be separated by whitespace",
            });
        }
        if b[i] == b'"' {
            i = lex_quoted(start, line, i, arena, out)?;
            continue;
        }
        if b[i] == b'#' || b[i] == b'|' {
            return Err(LexError {
                col: i,
                detail: "a bare value cannot start with `#` or `|`; quote it",
            });
        }
        let value_start = i;
        while i < b.len() && b[i] != b' ' && b[i] != b'\t' {
            if !is_bare_byte(b[i]) {
                return Err(LexError {
                    col: i,
                    detail: "character not allowed in a bare value; quote it",
                });
            }
            i += 1;
        }
        out.push(Str::src(start + value_start, start + i));
    }
}

/// Lexes the quoted value starting at the `"` at byte `q`; returns the index after
/// the closing quote. Values without escapes borrow the source.
fn lex_quoted(
    start: usize,
    line: &str,
    q: usize,
    arena: &mut String,
    out: &mut Vec<Str>,
) -> Result<usize, LexError> {
    let b = line.as_bytes();
    assert_eq!(b[q], b'"');
    let mut i = q + 1;
    while i < b.len() && b[i] != b'"' && b[i] != b'\\' && b[i] != b'\r' {
        i += 1;
    }
    if i < b.len() && b[i] == b'"' {
        out.push(Str::src(start + q + 1, start + i));
        return Ok(i + 1);
    }
    let arena_start = arena.len();
    arena.push_str(&line[q + 1..i]);
    match unescape(line, i, q, arena) {
        Ok(close) => {
            assert!(close > q);
            assert_eq!(b[close], b'"', "unescape stops at the closing quote");
            out.push(Str::arena_tail(arena, arena_start));
            Ok(close + 1)
        }
        Err(e) => {
            arena.truncate(arena_start);
            Err(e)
        }
    }
}

/// Appends the rest of a quoted value from byte `i` to `arena`, resolving escapes;
/// returns the index of the closing quote.
fn unescape(line: &str, mut i: usize, q: usize, arena: &mut String) -> Result<usize, LexError> {
    let b = line.as_bytes();
    assert!(i > q, "unescaping starts after the opening quote");
    assert!(i <= b.len());
    while i < b.len() {
        match b[i] {
            b'"' => return Ok(i),
            b'\r' => {
                return Err(LexError {
                    col: i,
                    detail: "bare CR in a quoted value",
                });
            }
            b'\\' => {
                let (c, len) = match b.get(i + 1) {
                    Some(b'"') => ('"', 2),
                    Some(b'\\') => ('\\', 2),
                    Some(b'n') => ('\n', 2),
                    Some(b't') => ('\t', 2),
                    Some(b'u') => unicode_escape(&b[i..]).ok_or(LexError {
                        col: i,
                        detail: "`\\u{...}` needs 1-6 hex digits naming a Unicode scalar",
                    })?,
                    _ => {
                        return Err(LexError {
                            col: i,
                            detail: "unknown escape; use \\\" \\\\ \\n \\t or \\u{...}",
                        });
                    }
                };
                arena.push(c);
                i += len;
            }
            _ => {
                let run = i;
                while i < b.len() && !matches!(b[i], b'"' | b'\\' | b'\r') {
                    i += 1;
                }
                arena.push_str(&line[run..i]);
            }
        }
    }
    Err(LexError {
        col: q,
        detail: "unterminated quoted value",
    })
}

/// Decodes `\u{h...}` at the start of `b`; returns the char and the escape's byte length.
fn unicode_escape(b: &[u8]) -> Option<(char, usize)> {
    assert!(b.starts_with(b"\\u"), "called at a `\\u` escape");
    if b.get(2) != Some(&b'{') {
        return None;
    }
    let close = b.iter().take(10).position(|&c| c == b'}')?;
    let hex = &b[3..close];
    // `1*6HEXDIG`: checked here because `from_str_radix` also takes a sign (`\u{+41}`).
    if hex.is_empty() || hex.len() > 6 || !hex.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let text = std::str::from_utf8(hex).expect("hex digits are ASCII");
    let value = u32::from_str_radix(text, 16).expect("1-6 hex digits fit u32");
    assert!(
        (5..=10).contains(&(close + 1)),
        "escape is 5 to 10 bytes long"
    );
    Some((char::from_u32(value)?, close + 1))
}

/// Lexes a word starting at byte `i`; returns its end, which is whitespace or the line end.
fn lex_word(b: &[u8], i: usize) -> Result<usize, LexError> {
    if !b.get(i).is_some_and(u8::is_ascii_alphabetic) {
        return Err(LexError {
            col: i,
            detail: "expected a word (a letter, then letters, digits, `_ - . :`)",
        });
    }
    let mut j = i + 1;
    while j < b.len() && document::is_word_byte(b[j]) {
        j += 1;
    }
    if j < b.len() && b[j] != b' ' && b[j] != b'\t' {
        return Err(LexError {
            col: j,
            detail: "character not allowed in a word",
        });
    }
    assert!(j > i, "a word has at least one letter");
    assert!(j <= b.len());
    Ok(j)
}
