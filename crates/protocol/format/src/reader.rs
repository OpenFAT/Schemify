//! Reader: bytes → [`Document`] or every error in them (spec §2–§5, §7).
//!
//! Spine: size/BOM/UTF-8 gate → `lex` (lexical layer, `E027`/`E028`) → `check_records`
//! (ranks, shapes, attributes; rewrites coordinates to their shortest decimal and wire
//! endpoints into order, parsing each coordinate once) → `check_keys` (ids, names) →
//! `check_kind_rules`. Every check is a pass over whole tables.
//!
//! Contents:
//! - `pub fn parse(bytes: &[u8]) -> Result<Document<'_>, ParseError>`
//! - `pub fn parse_prefix(bytes: &[u8], max_rank: u8) -> Result<Document<'_>, ParseError>`
//! - `pub(crate) fn check_kind_rules(doc: &Document<'_>, diags: &mut Vec<Diagnostic>)`

use crate::builder::push_coord;
use crate::diag::{Code, Diagnostic, NO_RECORD, ParseError};
use crate::document::{Document, NONE, Str, is_word_byte, resolve};
use crate::id::id_sort_key;
use crate::lex::{DIAGNOSTICS_MAX, lex};
use crate::schema::{FileKind, KeyRule, RANK_MAX, RecordKind, Shape, shape_arity};

/// Largest file the reader accepts (spec §2, `E021`).
const MAX_FILE_BYTES: usize = 1 << 30;
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
/// header version is newer than [`VERSION`](crate::VERSION)), `E023` rank order, `E029` arity or value
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

// ---- private ----

/// Parses `bytes` (at most `limit` of them) keeping records of rank `<= max_rank`.
fn parse_with(bytes: &[u8], limit: usize, max_rank: u8) -> Result<Document<'_>, ParseError> {
    assert!(limit <= MAX_FILE_BYTES && max_rank <= RANK_MAX);
    let fail = |d: Diagnostic| ParseError {
        diagnostics: vec![d],
    };
    let src = utf8(bytes, limit).map_err(fail)?;
    let (mut doc, mut diags) = lex(src).map_err(fail)?;
    if diags.len() < DIAGNOSTICS_MAX {
        check_records(&mut doc, &mut diags);
        check_keys(&mut doc, &mut diags);
        if max_rank >= 1 {
            check_kind_rules(&doc, &mut diags);
        }
    }
    if diags.is_empty() {
        assert!(doc.warnings.iter().all(|d| d.code.is_warning()));
        return Ok(doc);
    }
    sort_diags(&mut diags);
    diags.truncate(DIAGNOSTICS_MAX);
    Err(ParseError { diagnostics: diags })
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

/// The file gate: size (`E021`), BOM and UTF-8 (`E028`).
fn utf8(bytes: &[u8], limit: usize) -> Result<&str, Diagnostic> {
    let at = |code, line, col, detail| Diagnostic {
        code,
        line,
        col,
        record: NO_RECORD,
        detail,
    };
    if bytes.len() > limit {
        return Err(at(Code::E021, 1, 1, "file exceeds 2^30 bytes"));
    }
    if bytes.starts_with(BOM) {
        return Err(at(Code::E028, 1, 1, "byte order mark is not allowed"));
    }
    std::str::from_utf8(bytes).map_err(|e| {
        let bad = e.valid_up_to();
        let line = 1 + bytes[..bad].iter().filter(|&&b| b == b'\n').count();
        let line_start = bytes[..bad]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        let col = (bad - line_start + 1) as u32;
        at(Code::E028, line as u32, col, "invalid UTF-8")
    })
}

fn check_records(doc: &mut Document<'_>, diags: &mut Vec<Diagnostic>) {
    assert_eq!(doc.record_kind(0), RecordKind::Header);
    let diags_before = diags.len();
    let mut rank_max = 0u8;
    let mut coords: Vec<Option<i32>> = Vec::new();
    doc.field_milli = vec![0; doc.fields.len()];
    doc.value_milli = vec![0; doc.attr_values.len()];
    for r in 0..doc.record_count() {
        let kind = doc.record_kind(r);
        if kind == RecordKind::Unknown {
            continue;
        }
        let schema = kind.schema();
        let line = doc.record_line(r);
        let diag = |code, col, detail| Diagnostic {
            code,
            line,
            col,
            record: r,
            detail,
        };
        if kind != RecordKind::Header {
            if schema.rank < rank_max {
                diags.push(diag(
                    Code::E023,
                    1,
                    "record of a lower rank after a higher-rank record",
                ));
            }
            rank_max = rank_max.max(schema.rank);
            let f = doc.field_range(r);
            let fields = &mut doc.fields[f.clone()];
            let milli = &mut doc.field_milli[f.clone()];
            if let Err((i, code, detail)) = check_shape(
                &schema.fields,
                doc.src,
                &mut doc.arena,
                fields,
                milli,
                &mut coords,
            ) {
                let col = if i == NONE {
                    1
                } else {
                    column(doc, doc.fields[f.start + i as usize])
                };
                diags.push(diag(code, col, detail));
            }
            // Spec §4: a wire has length, and is written `(x1, y1) < (x2, y2)`.
            if kind == RecordKind::Wire
                && let [_, Some(x1), Some(y1), Some(x2), Some(y2)] = coords[..]
            {
                if (x1, y1) == (x2, y2) {
                    diags.push(diag(Code::E026, 1, "wire endpoints are equal"));
                } else if (x2, y2) < (x1, y1) {
                    for (a, b) in [(1, 3), (2, 4)] {
                        doc.fields.swap(f.start + a, f.start + b);
                        doc.field_milli.swap(f.start + a, f.start + b);
                    }
                    assert!(!doc.wire_reversed(r));
                }
            }
        }
        if doc.verbatim(r).len() > 0 && !schema.verbatim {
            diags.push(diag(
                Code::E029,
                1,
                "this record kind takes no `|` verbatim lines",
            ));
        }
        let mut seen: u32 = 0; // bit i: schema.attrs[i] already present
        for a in doc.attrs(r) {
            let Some((i, attr)) = schema.attr(doc.attr_key(a)) else {
                continue;
            };
            let (aline, key) = (doc.attr_line(a), doc.attr_key[a as usize]);
            let at = |code, col, detail| Diagnostic {
                line: aline,
                ..diag(code, col, detail)
            };
            if seen & (1 << i) != 0 && !attr.repeat {
                diags.push(at(Code::E024, column(doc, key), "attribute given twice"));
            }
            seen |= 1 << i;
            let v = doc.value_range(a);
            let values = &mut doc.attr_values[v.clone()];
            let milli = &mut doc.value_milli[v.clone()];
            if let Err((vi, code, detail)) = check_shape(
                &attr.shape,
                doc.src,
                &mut doc.arena,
                values,
                milli,
                &mut coords,
            ) {
                let span = if vi == NONE {
                    key
                } else {
                    doc.attr_values[v.start + vi as usize]
                };
                diags.push(at(code, column(doc, span), detail));
            }
        }
        for (i, attr) in schema.attrs.iter().enumerate() {
            if attr.required && seen & (1 << i) == 0 {
                diags.push(diag(Code::E029, 1, "missing required attribute"));
            }
        }
        if kind == RecordKind::Shape
            && let Some(form) = doc.field(r, 1)
            && shape_arity(form, doc.fields(r).len().saturating_sub(2)) == Some(false)
        {
            diags.push(diag(
                Code::E029,
                1,
                "wrong number of values for this shape form",
            ));
        }
    }
    assert!(
        diags[diags_before..]
            .iter()
            .all(|d| d.record != NO_RECORD && !d.code.is_warning())
    );
    assert_eq!(doc.field_milli.len(), doc.fields.len());
    assert_eq!(doc.value_milli.len(), doc.attr_values.len());
}

/// Checks a value list against a shape and makes its coordinates canonical: a value
/// that is not its shortest decimal is re-formatted into `arena` and its span replaced.
/// Fills `coords` with each slot's milli-units (`None` for other and bad values) and
/// `milli`, the values' slice of the document's coordinate column, with the same (0 for
/// `None`); every value is visited even after an error, so a wire's endpoints are known
/// whenever its arity is right. Returns the first offending value index ([`NONE`] for an
/// arity error) with its code.
fn check_shape(
    shape: &Shape,
    src: &str,
    arena: &mut String,
    values: &mut [Str],
    milli: &mut [i32],
    coords: &mut Vec<Option<i32>>,
) -> Result<(), (u32, Code, &'static str)> {
    assert_eq!(milli.len(), values.len());
    coords.clear();
    shape
        .check_arity(values.len())
        .map_err(|detail| (NONE, Code::E029, detail))?;
    let mut first = Ok(());
    for (i, s) in values.iter_mut().enumerate() {
        let ty = shape.ty(i).expect("arity checked above");
        let text = resolve(src, arena, *s);
        let coord = match ty.check(text) {
            Ok(Some(m)) if !is_shortest_coord(text) => {
                *s = push_coord(arena, m);
                Some(m)
            }
            Ok(coord) => coord,
            Err((code, detail)) => {
                first = first.and(Err((i as u32, code, detail)));
                None
            }
        };
        milli[i] = coord.unwrap_or(0);
        coords.push(coord);
    }
    assert_eq!(coords.len(), values.len());
    first
}

/// Returns the 1-based column of a source span, or 0 for arena (unescaped) text.
fn column(doc: &Document<'_>, s: Str) -> u32 {
    let Some(off) = s.src_offset() else { return 0 };
    assert!(off <= doc.src.len());
    let line_start = doc.src[..off].rfind('\n').map_or(0, |i| i + 1);
    assert!(line_start <= off);
    (off - line_start + 1) as u32
}

/// Sorts diagnostics by (line, column, code), stably: the crate's one diagnostic sort.
fn sort_diags(diags: &mut [Diagnostic]) {
    diags.sort_by_key(|d| (d.line, d.col, d.code));
    assert!(
        diags
            .windows(2)
            .all(|w| (w[0].line, w[0].col) <= (w[1].line, w[1].col))
    );
}

/// Reports duplicate ids (`W001`, into the document's warnings) and duplicate names
/// within a kind (`E024`). Cost: two sorts, O(n log n) in records, near O(n) on canonical files.
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
    // Stable sorts merge the sorted runs a canonical file has (one per kind) in O(n log k);
    // keys are distinct (record index last), so the order is the same as unstable.
    ids.sort();
    names.sort();
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
    sort_diags(&mut warnings); // all W001 at column 1: by line
    assert!(
        warnings
            .iter()
            .all(|d| d.code == Code::W001 && d.record != NO_RECORD)
    );
    doc.warnings = warnings;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scalar::{format_coord, parse_coord};

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
