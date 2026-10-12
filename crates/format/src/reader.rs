//! Reader: bytes → [`Document`] (spec §2–§5).
//!
//! Spine: size/BOM/UTF-8 gate → header → `lex_lines` (lexical layer, `E027`/`E028`) →
//! `check_records` (ranks, shapes, attributes) → `check_keys` (ids, names) →
//! `check_kind_rules` → `normalize` (shortest coordinates, wire endpoint order).
//! Every check is a pass over whole tables.
//!
//! `simd-loops` triage: lines average under 60 bytes and the lexer is a state chain,
//! so the only wide scan is the LF search, which `str::find` already does with memchr.

use crate::diag::{Code, Diagnostic, NO_RECORD, ParseError};
use crate::document::{Document, NONE, Str, VERSION, is_word_byte};
use crate::id::{id_sort_key, is_id};
use crate::scalar::{CoordError, Orient, format_coord, is_number, parse_coord};
use crate::schema::{FileKind, KeyRule, RANK_MAX, RecordKind, Rest, Shape, Ty};

/// Largest file the reader accepts (spec §2, `E021`).
pub const MAX_FILE_BYTES: usize = 1 << 30;
/// Errors collected before the reader stops looking for more.
const DIAGNOSTICS_MAX: usize = 64;
/// UTF-8 byte order mark, rejected (`E028`).
const BOM: &[u8] = b"\xEF\xBB\xBF";

/// Parses a whole file into a [`Document`] that borrows unescaped values from `bytes`.
///
/// CRLF and a missing final newline are accepted. The returned model is canonical:
/// coordinates are rewritten to their shortest decimal and wires so that
/// `(x1, y1) < (x2, y2)`. Duplicate ids are not errors: they are reported as `W001`
/// in [`Document::warnings`] and left for the `doc` layer to re-id.
///
/// # Errors
///
/// Every error in the file (up to 64), in line order: `E021` size, `E028` lexical
/// (BOM, invalid UTF-8, bad header, bad escape, ...), `E027` unknown record (unless the
/// header version is newer than [`VERSION`]), `E023` rank order, `E029` arity or value
/// form, `E022` coordinates, `E024` duplicate names, `E025` kind rules, `E026` zero-length wires.
/// A `journal` file has no known record words, so every record is kept as `Unknown`.
///
/// # Examples
///
/// ```
/// let doc = schemify_format::parse(b"schemify 1 shbench\nwire 0abc 0 0 0 0\n");
/// assert!(doc.unwrap_err().has(schemify_format::Code::E026));
/// ```
pub fn parse(bytes: &[u8]) -> Result<Document<'_>, ParseError> {
    parse_with(bytes, MAX_FILE_BYTES, RANK_MAX)
}

/// Parses the header and every record of rank `<= max_rank`, stopping at the first
/// record of a higher rank without reading further. `parse_prefix(b, 1)` is the symbol
/// interface (pins, params, impls) a library browser or netlister needs; it costs the
/// size of that prefix, not of the file (research §2.1: 5 µs).
///
/// Comments just before the cut become end-of-file comments. Duplicate checks and kind
/// rules see only the prefix (kind rules are skipped when `max_rank` is 0).
///
/// # Errors
///
/// As [`parse`], for the prefix.
pub fn parse_prefix(bytes: &[u8], max_rank: u8) -> Result<Document<'_>, ParseError> {
    let cut = prefix_len(bytes, max_rank);
    parse_with(&bytes[..cut], MAX_FILE_BYTES, max_rank)
}

/// Returns the byte offset of the first line starting a record of rank `> max_rank`.
fn prefix_len(bytes: &[u8], max_rank: u8) -> usize {
    let Some(header_end) = bytes.iter().position(|&b| b == b'\n') else {
        return bytes.len();
    };
    let kind = std::str::from_utf8(&bytes[..header_end])
        .ok()
        .and_then(|h| h.split_ascii_whitespace().nth(2))
        .and_then(FileKind::from_word);
    let Some(kind) = kind else { return bytes.len() };
    let mut pos = header_end + 1;
    while pos < bytes.len() {
        let end = bytes[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |i| pos + i);
        let line = &bytes[pos..end];
        if line.first().is_some_and(u8::is_ascii_alphabetic) {
            let w = line
                .iter()
                .position(|&c| !is_word_byte(c))
                .unwrap_or(line.len());
            let word = std::str::from_utf8(&line[..w]).expect("word bytes are ASCII");
            if kind.lookup(word).is_some_and(|k| k.rank() > max_rank) {
                return pos;
            }
        }
        assert!(end + 1 > pos, "line scan must advance");
        pos = end + 1;
    }
    assert!(pos >= bytes.len());
    bytes.len()
}

pub(crate) fn parse_with(
    bytes: &[u8],
    limit: usize,
    max_rank: u8,
) -> Result<Document<'_>, ParseError> {
    assert!(limit <= MAX_FILE_BYTES && max_rank <= RANK_MAX);
    let fail = |code, line, col, detail| ParseError {
        diagnostics: vec![Diagnostic {
            code,
            line,
            col,
            record: NO_RECORD,
            detail,
        }],
    };
    if bytes.len() > limit {
        return Err(fail(Code::E021, 1, 1, "file exceeds 2^30 bytes"));
    }
    if bytes.starts_with(BOM) {
        return Err(fail(Code::E028, 1, 1, "byte order mark is not allowed"));
    }
    let src = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => {
            let bad = e.valid_up_to();
            let line = 1 + bytes[..bad].iter().filter(|&&b| b == b'\n').count();
            let line_start = bytes[..bad]
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |i| i + 1);
            return Err(fail(
                Code::E028,
                line as u32,
                (bad - line_start + 1) as u32,
                "invalid UTF-8",
            ));
        }
    };
    let (mut doc, body) = lex_header(src).map_err(|d| ParseError {
        diagnostics: vec![d],
    })?;

    let mut lexer = Lexer {
        doc,
        diags: Vec::new(),
        pending: Vec::new(),
        vals: Vec::new(),
        skipping: false,
    };
    lexer.lex_lines(body);
    doc = lexer.doc;
    let mut diags = lexer.diags;
    if diags.len() < DIAGNOSTICS_MAX {
        check_records(&doc, &mut diags);
        check_keys(&mut doc, &mut diags);
        if max_rank >= 1 {
            check_kind_rules(&doc, &mut diags);
        }
    }
    if diags.is_empty() {
        normalize(&mut doc);
        assert!(doc.warnings.iter().all(|d| d.code.is_warning()));
        return Ok(doc);
    }
    diags.sort_by_key(|d| (d.line, d.col, d.code));
    diags.truncate(DIAGNOSTICS_MAX);
    Err(ParseError { diagnostics: diags })
}

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
    doc.begin(RecordKind::Header, 1);
    // Stored canonical (`01` → `1`) so the writer needs no special case for the header.
    let v = doc.intern(&number.to_string());
    let k = doc.intern(file_kind.word());
    doc.push_field_str(v);
    doc.push_field_str(k);
    assert_eq!(doc.record_count(), 1);
    assert!(body <= src.len() && body > 8);
    Ok((doc, body))
}

/// Error position inside one line: (byte index, detail).
type LexError = (usize, &'static str);

struct Lexer<'s> {
    doc: Document<'s>,
    diags: Vec<Diagnostic>,
    /// Comments waiting for the next record.
    pending: Vec<Str>,
    /// Scratch for one line's values, committed only when the whole line lexes.
    vals: Vec<Str>,
    /// Inside a rejected record: its indented lines are dropped, not misattributed.
    skipping: bool,
}

impl<'s> Lexer<'s> {
    fn lex_lines(&mut self, body: usize) {
        let src = self.doc.src;
        assert!(body <= src.len() && self.doc.record_count() == 1);
        let mut pos = body;
        let mut line_no: u32 = 1;
        while pos < src.len() && self.diags.len() < DIAGNOSTICS_MAX {
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
        for c in std::mem::take(&mut self.pending) {
            self.doc.eof_comments.push(c);
        }
        assert!(self.pending.is_empty());
        assert!(
            self.diags.len() <= DIAGNOSTICS_MAX,
            "at most one error per line, loop stops at the cap"
        );
    }

    fn error(&mut self, code: Code, line: u32, col: usize, detail: &'static str) {
        assert!(
            line >= 2 && !code.is_warning(),
            "lexer errors follow the header"
        );
        self.diags.push(Diagnostic {
            code,
            line,
            col: col as u32 + 1,
            record: NO_RECORD,
            detail,
        });
    }

    fn lex_line(&mut self, start: usize, end: usize, line_no: u32) {
        let src = self.doc.src;
        assert!(start <= end && end <= src.len());
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
            let span = self.doc.src_span(off, off + text.len());
            self.pending.push(span);
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
        assert!(!line.is_empty() && !line.starts_with([' ', '\t']));
        self.skipping = true;
        let word_end = match lex_word(line.as_bytes(), 0) {
            Ok(e) => e,
            Err((col, detail)) => return self.error(Code::E028, line_no, col, detail),
        };
        if let Err((col, detail)) = self.lex_values(start, line, word_end) {
            return self.error(Code::E028, line_no, col, detail);
        }
        let word = &line[..word_end];
        let kind = match self.doc.kind.lookup(word) {
            Some(k) => k,
            // A newer version (spec §5) or a journal (no known words): kept as written.
            None if self.doc.version > VERSION || self.doc.kind == FileKind::Journal => {
                RecordKind::Unknown
            }
            None if word == "schemify" => {
                return self.error(Code::E028, line_no, 0, "second header line");
            }
            None => {
                return self.error(
                    Code::E027,
                    line_no,
                    0,
                    "record word not known for this file kind",
                );
            }
        };
        let r = self.doc.begin(kind, line_no);
        if kind == RecordKind::Unknown {
            let w = self.doc.src_span(start, start + word_end);
            self.doc.unknown_words.push((r, w));
        }
        for c in self.pending.drain(..) {
            self.doc.comments.push(c);
        }
        for i in 0..self.vals.len() {
            self.doc.push_field_str(self.vals[i]);
        }
        assert_eq!(self.doc.field_spans(r).len(), self.vals.len());
        self.skipping = false;
    }

    fn lex_attr(&mut self, start: usize, line: &str, indent: usize, line_no: u32) {
        assert!(indent > 0 && indent < line.len() && !self.skipping);
        let key_end = match lex_word(line.as_bytes(), indent) {
            Ok(e) => e,
            Err((col, detail)) => return self.error(Code::E028, line_no, col, detail),
        };
        if let Err((col, detail)) = self.lex_values(start, line, key_end) {
            return self.error(Code::E028, line_no, col, detail);
        }
        let key = self.doc.src_span(start + indent, start + key_end);
        let a = self.doc.push_attr_str(key, line_no);
        for i in 0..self.vals.len() {
            self.doc.push_value_str(self.vals[i]);
        }
        assert_eq!(self.doc.value_spans(a).len(), self.vals.len());
    }

    fn lex_verbatim(&mut self, start: usize, line: &str, indent: usize, line_no: u32) {
        assert_eq!(line.as_bytes()[indent], b'|');
        assert!(indent > 0 && !self.skipping);
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
        let span = self.doc.src_span(start + off, start + off + text.len());
        self.doc.push_verbatim_str(span);
        assert_eq!(span.len as usize, text.len());
    }

    /// Lexes `*(WS value) *WSP` from byte `i` of `line` into `self.vals`.
    fn lex_values(&mut self, start: usize, line: &str, mut i: usize) -> Result<(), LexError> {
        assert!(i <= line.len() && start + line.len() <= self.doc.src.len());
        self.vals.clear();
        let b = line.as_bytes();
        loop {
            let ws = i;
            while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                i += 1;
            }
            if i == b.len() {
                assert!(self.vals.len() <= b.len(), "every value consumes input");
                return Ok(());
            }
            if i == ws {
                return Err((i, "values must be separated by whitespace"));
            }
            if b[i] == b'"' {
                i = self.lex_quoted(start, line, i)?;
                continue;
            }
            if b[i] == b'#' || b[i] == b'|' {
                return Err((i, "a bare value cannot start with `#` or `|`; quote it"));
            }
            let value_start = i;
            while i < b.len() && b[i] != b' ' && b[i] != b'\t' {
                if !is_bare_byte(b[i]) {
                    return Err((i, "character not allowed in a bare value; quote it"));
                }
                i += 1;
            }
            let span = self.doc.src_span(start + value_start, start + i);
            self.vals.push(span);
        }
    }

    /// Lexes the quoted value starting at the `"` at byte `q`; returns the index after
    /// the closing quote. Values without escapes borrow the source.
    fn lex_quoted(&mut self, start: usize, line: &str, q: usize) -> Result<usize, LexError> {
        let b = line.as_bytes();
        assert_eq!(b[q], b'"');
        let mut i = q + 1;
        while i < b.len() && b[i] != b'"' && b[i] != b'\\' && b[i] != b'\r' {
            i += 1;
        }
        if i < b.len() && b[i] == b'"' {
            let span = self.doc.src_span(start + q + 1, start + i);
            self.vals.push(span);
            return Ok(i + 1);
        }
        let arena_start = self.doc.arena_len();
        self.doc.arena.push_str(&line[q + 1..i]);
        let result = self.unescape(line, i, q);
        match result {
            Ok(close) => {
                assert!(close > q && b[close] == b'"');
                let span = self.doc.arena_span(arena_start);
                self.vals.push(span);
                Ok(close + 1)
            }
            Err(e) => {
                self.doc.arena.truncate(arena_start);
                Err(e)
            }
        }
    }

    /// Appends the rest of a quoted value from byte `i` to the arena, resolving escapes;
    /// returns the index of the closing quote.
    fn unescape(&mut self, line: &str, mut i: usize, q: usize) -> Result<usize, LexError> {
        let b = line.as_bytes();
        assert!(i > q && i <= b.len());
        while i < b.len() {
            match b[i] {
                b'"' => return Ok(i),
                b'\r' => return Err((i, "bare CR in a quoted value")),
                b'\\' => {
                    let (c, len) = match b.get(i + 1) {
                        Some(b'"') => ('"', 2),
                        Some(b'\\') => ('\\', 2),
                        Some(b'n') => ('\n', 2),
                        Some(b't') => ('\t', 2),
                        Some(b'u') => unicode_escape(&b[i..]).ok_or((
                            i,
                            "`\\u{...}` needs 1-6 hex digits naming a Unicode scalar",
                        ))?,
                        _ => return Err((i, "unknown escape; use \\\" \\\\ \\n \\t or \\u{...}")),
                    };
                    self.doc.arena.push(c);
                    i += len;
                }
                _ => {
                    let run = i;
                    while i < b.len() && !matches!(b[i], b'"' | b'\\' | b'\r') {
                        i += 1;
                    }
                    self.doc.arena.push_str(&line[run..i]);
                }
            }
        }
        Err((q, "unterminated quoted value"))
    }
}

/// Decodes `\u{h...}` at the start of `b`; returns the char and the escape's byte length.
fn unicode_escape(b: &[u8]) -> Option<(char, usize)> {
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
        return Err((
            i,
            "expected a word (a letter, then letters, digits, `_ - . :`)",
        ));
    }
    let mut j = i + 1;
    while j < b.len() && is_word_byte(b[j]) {
        j += 1;
    }
    if j < b.len() && b[j] != b' ' && b[j] != b'\t' {
        return Err((j, "character not allowed in a word"));
    }
    assert!(j > i && j <= b.len());
    Ok(j)
}

/// Returns whether `c` may appear in a bare value (spec `bchar`, bytewise; bytes of
/// multi-byte UTF-8 characters are all ≥ 0x80 and allowed).
pub(crate) fn is_bare_byte(c: u8) -> bool {
    c > b' ' && c != b'"' && c != b'\\' && c != 0x7f
}

// ---------------------------------------------------------------- schema checks

/// Returns the 1-based column of a source span, or 0 for arena (unescaped) text.
fn column(doc: &Document<'_>, s: Str) -> u32 {
    let Some(off) = s.src_offset() else { return 0 };
    assert!(off <= doc.src.len());
    let line_start = doc.src[..off].rfind('\n').map_or(0, |i| i + 1);
    assert!(line_start <= off);
    (off - line_start + 1) as u32
}

/// Checks one value against a slot type.
fn check_ty(ty: Ty, v: &str) -> Result<(), (Code, &'static str)> {
    let ok = match ty {
        Ty::Any => true,
        Ty::Id | Ty::IdRef => is_id(v),
        Ty::Coord => {
            return parse_coord(v).map(|_| ()).map_err(|e| match e {
                CoordError::NotNumber => (Code::E029, "expected a coordinate number"),
                CoordError::NotRepresentable => (
                    Code::E022,
                    "needs more than 3 fractional digits or overflows i32 milli-units",
                ),
            });
        }
        Ty::Number => is_number(v),
        Ty::Orient => Orient::parse(v).is_some(),
        Ty::Bool => v == "true" || v == "false",
        Ty::Choice(words) => words.contains(&v),
    };
    if ok {
        return Ok(());
    }
    Err((
        Code::E029,
        match ty {
            Ty::Id | Ty::IdRef => "expected an id (1-13 lowercase Crockford base32 chars)",
            Ty::Number => "expected a number `-?digits(.digits)?`",
            Ty::Orient => "expected R0 R90 R180 R270 MR0 MR90 MR180 MR270",
            Ty::Bool => "expected true or false",
            _ => "value not in the allowed set",
        },
    ))
}

/// Checks a value list against a shape; returns the offending value index
/// ([`NONE`] for an arity error) with its code.
fn check_shape(
    shape: &Shape,
    doc: &Document<'_>,
    values: &[Str],
) -> Result<(), (u32, Code, &'static str)> {
    let (n, fixed) = (values.len(), shape.fixed.len());
    assert!(fixed <= 8, "schema shapes are short");
    if n < fixed {
        return Err((NONE, Code::E029, "too few values"));
    }
    let extra = n - fixed;
    let arity_ok = match shape.rest {
        Rest::None => extra == 0,
        Rest::Optional(_) => extra <= 1,
        Rest::Many(_) => true,
        Rest::Pairs(_) => extra % 2 == 0,
    };
    if !arity_ok {
        return Err((NONE, Code::E029, "wrong number of values"));
    }
    for (i, s) in values.iter().enumerate() {
        let ty = shape.ty(i).expect("arity checked above");
        check_ty(ty, doc.text(*s)).map_err(|(code, detail)| (i as u32, code, detail))?;
    }
    Ok(())
}

fn check_records(doc: &Document<'_>, diags: &mut Vec<Diagnostic>) {
    assert_eq!(doc.record_kind(0), RecordKind::Header);
    let diags_before = diags.len();
    let mut rank_max = 0u8;
    for r in 0..doc.record_count() {
        let kind = doc.record_kind(r);
        if kind == RecordKind::Unknown {
            continue;
        }
        let schema = kind.schema();
        let line = doc.record_line(r);
        let mut report = |code, col, detail| {
            diags.push(Diagnostic {
                code,
                line,
                col,
                record: r,
                detail,
            })
        };
        if kind != RecordKind::Header {
            if schema.rank < rank_max {
                report(
                    Code::E023,
                    1,
                    "record of a lower rank after a higher-rank record",
                );
            }
            rank_max = rank_max.max(schema.rank);
            let fields = doc.field_spans(r);
            if let Err((i, code, detail)) = check_shape(&schema.fields, doc, fields) {
                report(
                    code,
                    if i == NONE {
                        1
                    } else {
                        column(doc, fields[i as usize])
                    },
                    detail,
                );
            }
        }
        if doc.verbatim(r).len() > 0 && !schema.verbatim {
            report(
                Code::E029,
                1,
                "this record kind takes no `|` verbatim lines",
            );
        }
        let mut seen: u32 = 0; // bit i: schema.attrs[i] already present
        for a in doc.attrs(r) {
            let Some((i, attr)) = schema.attr(doc.attr_key(a)) else {
                continue;
            };
            let (aline, key) = (doc.attr_line(a), doc.attr_key[a as usize]);
            if seen & (1 << i) != 0 && !attr.repeat {
                diags.push(Diagnostic {
                    code: Code::E024,
                    line: aline,
                    col: column(doc, key),
                    record: r,
                    detail: "attribute given twice",
                });
            }
            seen |= 1 << i;
            let values = doc.value_spans(a);
            if let Err((vi, code, detail)) = check_shape(&attr.shape, doc, values) {
                let col = column(doc, if vi == NONE { key } else { values[vi as usize] });
                diags.push(Diagnostic {
                    code,
                    line: aline,
                    col,
                    record: r,
                    detail,
                });
            }
        }
        for (i, attr) in schema.attrs.iter().enumerate() {
            if attr.required && seen & (1 << i) == 0 {
                diags.push(Diagnostic {
                    code: Code::E029,
                    line,
                    col: 1,
                    record: r,
                    detail: "missing required attribute",
                });
            }
        }
        if kind == RecordKind::Shape
            && let Some(form) = doc.field(r, 1)
            && crate::schema::shape_arity(form, doc.fields(r).len().saturating_sub(2))
                == Some(false)
        {
            diags.push(Diagnostic {
                code: Code::E029,
                line,
                col: 1,
                record: r,
                detail: "wrong number of values for this shape form",
            });
        }
        if kind == RecordKind::Wire && doc.fields(r).len() == 5 {
            let c = |i| doc.field(r, i).map(parse_coord);
            if c(1).is_some_and(|x| x.is_ok())
                && c(2).is_some_and(|y| y.is_ok())
                && c(1) == c(3)
                && c(2) == c(4)
            {
                diags.push(Diagnostic {
                    code: Code::E026,
                    line,
                    col: 1,
                    record: r,
                    detail: "wire endpoints are equal",
                });
            }
        }
    }
    assert!(
        diags[diags_before..]
            .iter()
            .all(|d| d.record != NO_RECORD && !d.code.is_warning())
    );
}

/// Reports duplicate ids (`W001`, into the document's warnings) and duplicate names
/// within a kind (`E024`). Cost: two sorts, O(n log n) in records.
fn check_keys(doc: &mut Document<'_>, diags: &mut Vec<Diagnostic>) {
    assert!(doc.warnings.is_empty(), "keys are checked once per parse");
    let mut ids: Vec<(u128, u32)> = Vec::new();
    let mut names: Vec<(RecordKind, &str, &str, u32)> = Vec::new();
    for r in 0..doc.record_count() {
        let kind = doc.record_kind(r);
        match kind.key() {
            KeyRule::Id if kind != RecordKind::Unknown => {
                if let Some(key) = doc.field(r, 0).and_then(id_sort_key) {
                    ids.push((key, r));
                }
            }
            KeyRule::Name | KeyRule::TypeName if kind != RecordKind::Unknown => {
                let (k0, k1) = doc.record_key(r);
                names.push((kind, k0, k1, r));
            }
            _ => {}
        }
    }
    ids.sort_unstable();
    names.sort_unstable();
    let mut warnings = Vec::new();
    for w in ids.windows(2) {
        if w[0].0 == w[1].0 {
            let line = doc.record_line(w[1].1);
            warnings.push(Diagnostic {
                code: Code::W001,
                line,
                col: 1,
                record: w[1].1,
                detail: "id already used earlier in the file",
            });
        }
    }
    for w in names.windows(2) {
        if (w[0].0, w[0].1, w[0].2) == (w[1].0, w[1].1, w[1].2) {
            let line = doc.record_line(w[1].3);
            diags.push(Diagnostic {
                code: Code::E024,
                line,
                col: 1,
                record: w[1].3,
                detail: "name already used by this record kind",
            });
        }
    }
    warnings.sort_by_key(|d| d.line);
    assert!(
        warnings
            .iter()
            .all(|d| d.code == Code::W001 && d.record != NO_RECORD)
    );
    doc.warnings = warnings;
}

/// Spec §4 kind rules (`E025`): `.shprim` has ≥ 1 pin and ≥ 1 impl, none schematic;
/// `.shcomp` has exactly one schematic impl; more than one impl needs a `default`.
pub(crate) fn check_kind_rules(doc: &Document<'_>, diags: &mut Vec<Diagnostic>) {
    assert_eq!(doc.record_kind(0), RecordKind::Header);
    let (mut pins, mut impls, mut schematic) = (0u32, 0u32, 0u32);
    for r in 0..doc.record_count() {
        match doc.record_kind(r) {
            RecordKind::Pin => pins += 1,
            RecordKind::Impl => {
                impls += 1;
                schematic += u32::from(doc.field(r, 1) == Some("schematic"));
            }
            _ => {}
        }
    }
    let mut report = |detail| {
        diags.push(Diagnostic {
            code: Code::E025,
            line: 1,
            col: 1,
            record: 0,
            detail,
        })
    };
    match doc.kind {
        FileKind::Shprim if pins == 0 => report(".shprim needs at least one pin"),
        FileKind::Shprim if impls == 0 => report(".shprim needs at least one impl"),
        FileKind::Shprim if schematic > 0 => report(".shprim cannot have a schematic impl"),
        FileKind::Shcomp if schematic != 1 => report(".shcomp needs exactly one schematic impl"),
        _ => {}
    }
    let cell = matches!(doc.kind, FileKind::Shprim | FileKind::Shcomp);
    if cell && impls > 1 && doc.find_attr(0, "default").is_none() {
        report("more than one impl needs a `default` header attribute");
    }
}

/// Returns whether a valid coordinate literal is already its shortest decimal: no `-0`,
/// no leading zeros, and 1–3 fractional digits not ending in `0`.
fn is_shortest_coord(t: &str) -> bool {
    let body = t.strip_prefix('-').unwrap_or(t).as_bytes();
    let dot = body.iter().position(|&c| c == b'.');
    let whole = &body[..dot.unwrap_or(body.len())];
    assert!(!whole.is_empty(), "only called on validated coordinates");
    let whole_ok = whole == b"0" || whole[0] != b'0';
    let frac_ok = dot.is_none_or(|d| body.len() - d - 1 <= 3 && !body.ends_with(b"0"));
    whole_ok && frac_ok && t != "-0"
}

/// Makes the parsed model canonical: coordinates become their shortest decimal
/// (`-80.000` → `-80`, so a merge sees no change) and wire endpoints are swapped so
/// `(x1, y1) < (x2, y2)`. Runs only on error-free documents, so every coordinate parses.
fn normalize(doc: &mut Document<'_>) {
    assert_eq!(doc.record_kind(0), RecordKind::Header);
    let mut buf = String::new();
    for r in 0..doc.record_count() {
        let schema = doc.record_kind(r).schema();
        let fields = doc.field_range(r);
        for f in fields.clone() {
            if schema.fields.ty(f - fields.start) == Some(Ty::Coord) {
                doc.fields[f] = canonical_coord(doc, doc.fields[f], &mut buf);
            }
        }
        for a in doc.attrs(r) {
            let Some((_, attr)) = schema.attr(doc.attr_key(a)) else {
                continue;
            };
            let values = doc.value_range(a);
            for v in values.clone() {
                if attr.shape.ty(v - values.start) == Some(Ty::Coord) {
                    doc.attr_values[v] = canonical_coord(doc, doc.attr_values[v], &mut buf);
                }
            }
        }
        let start = fields.start;
        if doc.wire_reversed(r) {
            doc.fields.swap(start + 1, start + 3);
            doc.fields.swap(start + 2, start + 4);
            assert!(!doc.wire_reversed(r));
        }
    }
}

fn canonical_coord(doc: &mut Document<'_>, s: Str, buf: &mut String) -> Str {
    if is_shortest_coord(doc.text(s)) {
        return s; // the common case: no formatting, no allocation
    }
    buf.clear();
    format_coord(
        parse_coord(doc.text(s)).expect("coordinates are checked before normalizing"),
        buf,
    );
    assert!(is_shortest_coord(buf) && buf != doc.text(s));
    doc.intern(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortest_coord_predicate_matches_the_formatter() {
        for t in [
            "0",
            "-0",
            "00",
            "1",
            "-1",
            "10",
            "010",
            "1.5",
            "1.50",
            "1.500",
            "0.125",
            "-0.125",
            "2147483.647",
            "1.0",
            "0.0",
        ] {
            let mut canonical = String::new();
            format_coord(parse_coord(t).unwrap(), &mut canonical);
            assert_eq!(is_shortest_coord(t), canonical == t, "{t}");
        }
    }

    #[test]
    fn size_cap_is_e021() {
        let text = b"schemify 1 config\nset grid 10\n";
        assert!(parse_with(text, text.len(), RANK_MAX).is_ok());
        let err = parse_with(text, text.len() - 1, RANK_MAX).unwrap_err();
        assert_eq!(err.diagnostics[0].code, Code::E021);
    }

    #[test]
    fn prefix_stops_at_the_first_higher_rank_record() {
        let text = b"schemify 1 shcomp\npin A in 0 0\nimpl sch schematic\n# graphics\nline 0 0 1 1\ninst 0abc ./x\n  at 0 0 R0\n  name X1\n";
        assert_eq!(&text[prefix_len(text, 1)..][..4], b"line");
        let doc = parse_prefix(text, 1).unwrap();
        assert_eq!(doc.record_count(), 3);
        assert_eq!(doc.eof_comments().collect::<Vec<_>>(), [" graphics"]);
        assert_eq!(parse_prefix(text, 6).unwrap(), parse(text).unwrap());
    }
}
