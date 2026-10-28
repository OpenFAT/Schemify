//! PDF: one page per drawing, the embedded faces as CID fonts with their `ToUnicode` maps,
//! streams ASCII85-encoded.

use super::ops::{Glyphs, Lang, Used, page_ops};
use super::{FONT_NAMES, Look, Page};
use schemify_canvas::CanvasPrims;

/// ASCII85 (PDF `/ASCII85Decode`) of `bytes` with its `~>` end marker and a line break
/// every 80 characters, so a PDF carrying a font stays 7-bit text.
pub(super) fn ascii85(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 5 / 4 + bytes.len() / 64 + 4);
    let mut col = 0usize;
    let mut push = |out: &mut String, c: u8| {
        out.push(char::from(c));
        col += 1;
        if col == 80 {
            out.push('\n');
            col = 0;
        }
    };
    for chunk in bytes.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        let mut v = u32::from_be_bytes(word);
        if chunk.len() == 4 && v == 0 {
            push(&mut out, b'z');
            continue;
        }
        let mut digits = [0u8; 5];
        for d in digits.iter_mut().rev() {
            *d = (v % 85) as u8 + b'!';
            v /= 85;
        }
        for &d in &digits[..chunk.len() + 1] {
            push(&mut out, d);
        }
    }
    out.push_str("~>");
    out
}

/// The `ToUnicode` CMap of the glyphs one face prints, so viewers extract and search the text.
pub(super) fn to_unicode(glyphs: &Glyphs) -> String {
    use std::fmt::Write;
    let rows: Vec<(u16, char)> = glyphs
        .iter()
        .filter(|(g, _)| **g != 0)
        .map(|(g, (_, c))| (*g, *c))
        .collect();
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    // At most 100 entries per block (PDF 32000-1 §9.10.3).
    for block in rows.chunks(100) {
        let _ = writeln!(s, "{} beginbfchar", block.len());
        for &(g, c) in block {
            let _ = write!(s, "<{g:04X}> <");
            let mut units = [0u16; 2];
            for u in c.encode_utf16(&mut units) {
                let _ = write!(s, "{u:04X}");
            }
            s.push_str(">\n");
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}

/// The PDF objects of UI face `face`, numbered from `first`: the `Type0` font (the one a
/// page names `/F<face + 1>`), its `CIDFontType2` (widths `W` of the glyphs it prints), the
/// font descriptor, the `FontFile2` stream (the whole face, ASCII85) and the `ToUnicode` CMap.
pub(super) fn font_objects(first: usize, face: u8, glyphs: &Glyphs) -> [String; 5] {
    use std::fmt::Write;
    assert!(!glyphs.is_empty(), "only a face that prints is embedded");
    let name = FONT_NAMES[usize::from(face)];
    let info = schemify_ui::ui_font_info(face);
    let upem = u32::from(info.upem);
    let em = |v: i16| i64::from(v) * 1000 / i64::from(upem);
    let mut w = String::new();
    for (g, (adv, _)) in glyphs {
        let _ = write!(w, "{g} [{}] ", (u32::from(*adv) * 1000 + upem / 2) / upem);
    }
    let font = schemify_ui::ui_fonts()[usize::from(face)];
    let data = ascii85(font);
    let cmap = to_unicode(glyphs);
    let b = info.bbox;
    [
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /{name} /Encoding /Identity-H /DescendantFonts [{} 0 R] /ToUnicode {} 0 R >>",
            first + 1,
            first + 4
        ),
        format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{name} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {} 0 R /CIDToGIDMap /Identity /W [{}] >>",
            first + 2,
            w.trim_end()
        ),
        format!(
            "<< /Type /FontDescriptor /FontName /{name} /Flags 32 /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {} /Descent {} /CapHeight {} /StemV 80 /FontFile2 {} 0 R >>",
            em(b[0]),
            em(b[1]),
            em(b[2]),
            em(b[3]),
            em(info.ascent),
            em(info.descent),
            em(info.cap_height),
            first + 3
        ),
        format!(
            "<< /Length {} /Length1 {} /Filter /ASCII85Decode >>\nstream\n{data}\nendstream",
            data.len() + 1,
            font.len()
        ),
        format!("<< /Length {} >>\nstream\n{cmap}endstream", cmap.len()),
    ]
}

/// One PDF page per `(prims, world bounds)` drawing, each fitted to `page`. Texts print in
/// the embedded UI faces (each a `Type0`/`CIDFontType2` font carrying the whole face as
/// `FontFile2`, glyph ids as 2-byte codes, a `ToUnicode` map), picked per character in the
/// canvas's fallback order, so non-ASCII text prints as on the canvas; a character neither
/// face has prints `.notdef`. Only faces that print are embedded (Noto Sans ~190 KB, KaTeX
/// Main ~65 KB as ASCII85); the file is 7-bit text.
///
/// # Panics
///
/// Panics if `pages` is empty or a bound is inverted.
pub fn pdf(pages: &[(&CanvasPrims, [i32; 4])], look: &Look, page: Page) -> Vec<u8> {
    use std::fmt::Write;
    assert!(!pages.is_empty(), "a PDF has at least one page");
    let size = page.points();
    let mut used = Used::default();
    let contents: Vec<String> = pages
        .iter()
        .map(|(prims, b)| {
            assert!(b[0] <= b[2] && b[1] <= b[3], "inverted bounds");
            page_ops(prims, *b, look, size, Lang::Pdf, &mut used)
        })
        .collect();
    // 1 catalog, 2 pages, (page, content) pairs from 3, then five objects per printing face.
    let mut next = 3 + 2 * pages.len();
    let mut fonts: [Option<usize>; 2] = [None; 2];
    let mut resources = String::new();
    for (f, glyphs) in used.iter().enumerate() {
        if !glyphs.is_empty() {
            fonts[f] = Some(next);
            let _ = write!(resources, "/F{} {next} 0 R ", f + 1);
            next += 5;
        }
    }
    let resources = if resources.is_empty() {
        "<< >>".to_owned()
    } else {
        format!("<< /Font << {resources}>> >>")
    };
    let mut objs: Vec<String> = Vec::with_capacity(next);
    let kids: Vec<String> = (0..pages.len())
        .map(|k| format!("{} 0 R", 3 + 2 * k))
        .collect();
    objs.push("<< /Type /Catalog /Pages 2 0 R >>".to_owned());
    objs.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages.len()
    ));
    for (k, content) in contents.iter().enumerate() {
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources {resources} /Contents {} 0 R >>",
            size.0,
            size.1,
            4 + 2 * k
        ));
        objs.push(format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ));
    }
    for (f, first) in fonts.iter().enumerate() {
        if let Some(first) = *first {
            assert_eq!(objs.len() + 1, first, "font objects are numbered in order");
            objs.extend(font_objects(first, f as u8, &used[f]));
        }
    }
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objs.len());
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        let _ = write!(out, "{} 0 obj\n{body}\nendobj\n", i + 1);
    }
    let xref = out.len();
    let _ = write!(out, "xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1);
    for off in &offsets {
        let _ = writeln!(out, "{off:010} 00000 n ");
    }
    let _ = write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objs.len() + 1
    );
    assert_eq!(offsets.len(), next - 1);
    assert!(out.is_ascii(), "the PDF is 7-bit text");
    out.into_bytes()
}
