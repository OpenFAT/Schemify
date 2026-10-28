//! EPS: one drawing, the embedded faces as Type 42 fonts (`sfnts` strings of at most
//! `SFNTS_MAX` bytes).
//!
//! Contents:
//! - `pub(super) const SFNTS_MAX`
//! - `pub fn eps`

use schemify_canvas::CanvasPrims;

use super::ops::{Glyphs, Lang, Used, page_ops};
use super::{FONT_NAMES, Look, Page};

/// Longest `sfnts` string of a Type 42 font: PostScript strings hold at most 65535 bytes,
/// and each must have an even length.
pub(super) const SFNTS_MAX: usize = 65_534;

/// The drawing as Encapsulated PostScript (Level 2), fitted into a `page`-sized bounding
/// box. Texts print in the embedded UI faces as Type 42 resources (only faces that print),
/// so non-ASCII text prints as on the canvas.
///
/// # Panics
///
/// Panics if `b` is inverted.
pub fn eps(prims: &CanvasPrims, b: [i32; 4], look: &Look, page: Page) -> String {
    assert!(b[0] <= b[2] && b[1] <= b[3], "inverted bounds");
    let size = page.points();
    let mut used = Used::default();
    let body = page_ops(prims, b, look, size, Lang::Ps, &mut used);
    let fonts: String = used
        .iter()
        .enumerate()
        .filter(|(_, glyphs)| !glyphs.is_empty())
        .map(|(f, glyphs)| type42(f as u8, glyphs))
        .collect();
    let out = format!(
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 {} {}\n%%Creator: Schemify\n%%LanguageLevel: 2\n%%EndComments\n{fonts}gsave\n{body}grestore\nshowpage\n%%EOF\n",
        size.0 as i32, size.1 as i32
    );
    assert!(out.is_ascii(), "the EPS is 7-bit text");
    out
}

// ---- private ----

/// UI face `face` as a PostScript Type 42 font resource named `FONT_NAMES[face]`: the
/// TrueType file as `sfnts` hex strings broken at table boundaries, `CharStrings` naming every
/// glyph it prints (`/g<id>`) for `glyphshow`.
///
/// # Panics
///
/// Panics if a table of the face is longer than [`SFNTS_MAX`] (not the case for the shipped
/// faces: the largest table, Noto Sans's `glyf`, is 63.6 KB).
fn type42(face: u8, glyphs: &Glyphs) -> String {
    use std::fmt::Write;
    // ponytail: breaks at table boundaries only; split `glyf` at `loca` glyph boundaries if
    // a UI face ever grows a table over 64 KB.
    let name = FONT_NAMES[usize::from(face)];
    let font = schemify_ui::ui_fonts()[usize::from(face)];
    let info = schemify_ui::ui_font_info(face);
    let be16 = |o: usize| usize::from(u16::from_be_bytes([font[o], font[o + 1]]));
    let be32 = |o: usize| u32::from_be_bytes([font[o], font[o + 1], font[o + 2], font[o + 3]]);
    let tables = be16(4);
    assert!(
        tables > 0 && 12 + 16 * tables <= font.len(),
        "a table directory"
    );
    let mut cuts: Vec<usize> = (0..tables)
        .map(|i| be32(12 + 16 * i + 8) as usize)
        .collect();
    cuts.push(font.len());
    cuts.sort_unstable();
    cuts.dedup();
    let mut strings: Vec<&[u8]> = Vec::new();
    let mut from = 0usize;
    for (k, &cut) in cuts.iter().enumerate() {
        // Extend the current string while the next cut still fits.
        if cuts.get(k + 1).is_some_and(|&n| n - from <= SFNTS_MAX) {
            continue;
        }
        assert!(
            cut > from && cut - from <= SFNTS_MAX,
            "a font table over 64 KB"
        );
        strings.push(&font[from..cut]);
        from = cut;
    }
    assert_eq!(from, font.len(), "every byte of the face is in a string");
    let u = f64::from(info.upem);
    let b = info.bbox;
    let mut s = format!(
        "%%BeginResource: font {name}\n11 dict begin\n/FontName /{name} def\n/FontType 42 def\n\
         /FontMatrix [1 0 0 1 0 0] def\n/FontBBox [{:.4} {:.4} {:.4} {:.4}] def\n/PaintType 0 def\n\
         /Encoding 256 array 0 1 255 {{1 index exch /.notdef put}} for def\n\
         /CharStrings {} dict dup begin\n/.notdef 0 def\n",
        f64::from(b[0]) / u,
        f64::from(b[1]) / u,
        f64::from(b[2]) / u,
        f64::from(b[3]) / u,
        glyphs.len() + 1
    );
    for g in glyphs.keys().filter(|g| **g != 0) {
        let _ = writeln!(s, "/g{g} {g} def");
    }
    s.push_str("end def\n/sfnts [\n");
    for part in &strings {
        s.push('<');
        for (i, byte) in part.iter().enumerate() {
            if i > 0 && i.is_multiple_of(64) {
                s.push('\n');
            }
            let _ = write!(s, "{byte:02X}");
        }
        // An odd-length string ends in a pad byte the interpreter drops (Type 42).
        if !part.len().is_multiple_of(2) {
            s.push_str("00");
        }
        s.push_str(">\n");
    }
    s.push_str("] def\nFontName currentdict end definefont pop\n%%EndResource\n");
    s
}
