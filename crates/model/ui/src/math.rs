//! The `math` widget (D21): LaTeX source in, RaTeX's layout out as positioned glyphs, rules and
//! paths ([`MathList`]), in pixels with the origin at the box's top-left and the baseline at
//! `height`.
//!
//! RaTeX positions glyphs with KaTeX font metrics, so the glyphs must come from the KaTeX fonts
//! (embedded, OFL). D21 names an OpenType MATH font; that would need a MATH-table layout engine,
//! which RaTeX is not.
//!
//! Contents:
//! - `pub struct MathFonts`: `new`, `face(u8) -> &FontData`
//! - `pub enum Verb`, `pub struct MathPaths`, `pub struct MathList`
//! - `pub(crate) fn face_bytes(name: &str) -> &'static [u8]`
//! - `pub(crate) fn layout_math(src: &str, px: f32, fonts: &MathFonts) -> Result<MathList, String>`

use std::fmt;
use std::sync::Arc;

use parley::FontData;
use parley::fontique::Blob;
use ratex_font::FontId;
use ratex_layout::LayoutOptions;
use ratex_types::display_item::DisplayItem;
use ratex_types::path_command::PathCommand;
use skrifa::FontRef;

/// The embedded KaTeX faces, indexed by the `u8` face index stored in [`MathList::glyph_face`].
pub struct MathFonts {
    data: Vec<FontData>,
}

/// Path verbs for [`MathPaths`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verb {
    /// Start a subpath at the next point.
    Move,
    /// Line to the next point.
    Line,
    /// Quadratic to the next two points.
    Quad,
    /// Cubic to the next three points.
    Cubic,
    /// Close the subpath (no points).
    Close,
}

/// Paths as SoA: path `i` uses verbs `verb_end[i-1]..verb_end[i]`; points are consumed in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MathPaths {
    /// Verbs of all paths.
    pub verb: Vec<Verb>,
    /// Points of all paths, in pixels.
    pub pt: Vec<[f32; 2]>,
    /// One past each path's last verb.
    pub verb_end: Vec<u32>,
}

/// Positioned math in pixels. Glyph columns are parallel; `y` is the glyph baseline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MathList {
    /// Face index into [`MathFonts`].
    pub glyph_face: Vec<u8>,
    /// Glyph id in that face.
    pub glyph_id: Vec<u32>,
    /// Pen x.
    pub glyph_x: Vec<f32>,
    /// Baseline y.
    pub glyph_y: Vec<f32>,
    /// Font size in pixels.
    pub glyph_px: Vec<f32>,
    /// Filled rectangles `[x, y, w, h]` (fraction bars, overlines, boxes).
    pub rules: Vec<[f32; 4]>,
    /// Filled paths (large delimiters).
    pub fills: MathPaths,
    /// Stroked paths (radicals, angles); stroke width is the caller's (≈ 0.04 em).
    pub strokes: MathPaths,
    /// Box width.
    pub width: f32,
    /// Ascent above the baseline (the baseline is at `y = height`).
    pub height: f32,
    /// Descent below the baseline.
    pub depth: f32,
    /// Glyphs dropped because no embedded face covers them.
    pub missing: u32,
}

/// One embedded KaTeX face: its [`FontId`] spelling and the bytes of `fonts/KaTeX_<name>.ttf`.
macro_rules! face {
    ($name:literal) => {
        (
            $name,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/fonts/KaTeX_",
                $name,
                ".ttf"
            )),
        )
    };
}

/// KaTeX faces in [`FontId`] spelling, with their bytes.
const FACES: [(&str, &[u8]); 19] = [
    face!("AMS-Regular"),
    face!("Caligraphic-Regular"),
    face!("Fraktur-Regular"),
    face!("Fraktur-Bold"),
    face!("Main-Bold"),
    face!("Main-BoldItalic"),
    face!("Main-Italic"),
    face!("Main-Regular"),
    face!("Math-BoldItalic"),
    face!("Math-Italic"),
    face!("SansSerif-Bold"),
    face!("SansSerif-Italic"),
    face!("SansSerif-Regular"),
    face!("Script-Regular"),
    face!("Size1-Regular"),
    face!("Size2-Regular"),
    face!("Size3-Regular"),
    face!("Size4-Regular"),
    face!("Typewriter-Regular"),
];

/// The bytes of embedded KaTeX face `name` ([`FontId`] spelling, `"Main-Regular"`).
///
/// # Panics
///
/// `name` is not one of the embedded faces.
pub(crate) fn face_bytes(name: &str) -> &'static [u8] {
    let face = FACES.iter().find(|f| f.0 == name);
    face.expect("an embedded KaTeX face").1
}

impl Default for MathFonts {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for MathFonts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MathFonts")
            .field("faces", &self.data.len())
            .finish()
    }
}

impl MathFonts {
    /// Wraps the embedded faces (no copy: the blobs borrow the static bytes).
    #[must_use]
    pub fn new() -> Self {
        // Arc: `Blob::new` takes one (the font library's API); it wraps a `&'static` slice.
        let data: Vec<FontData> = FACES
            .iter()
            .map(|(_, b)| FontData::new(Blob::new(Arc::new(*b)), 0))
            .collect();
        MathFonts { data }
    }

    /// Font for a face index from [`MathList::glyph_face`].
    #[must_use]
    pub fn face(&self, i: u8) -> &FontData {
        &self.data[i as usize]
    }
}

/// Parses and lays out `src` at `px` pixels per em.
///
/// # Errors
///
/// Returns RaTeX's parse message for input outside its LaTeX subset.
pub(crate) fn layout_math(src: &str, px: f32, fonts: &MathFonts) -> Result<MathList, String> {
    use skrifa::MetadataProvider;

    assert!(px > 0.0 && px.is_finite(), "math size must be positive");
    assert_eq!(fonts.data.len(), FACES.len());
    let ast = ratex_parser::parse(src).map_err(|e| e.to_string())?;
    let display =
        ratex_layout::to_display_list(&ratex_layout::layout(&ast, &LayoutOptions::default()));
    let mut out = MathList {
        width: (display.width * f64::from(px)) as f32,
        height: (display.height * f64::from(px)) as f32,
        depth: (display.depth * f64::from(px)) as f32,
        ..MathList::default()
    };
    let s = f64::from(px);
    let p = |v: f64| (v * s) as f32;
    // Parsed faces cached by index: a formula touches few faces, and parsing one is a table scan.
    let mut faces: [Option<FontRef<'static>>; FACES.len()] = [const { None }; FACES.len()];
    for item in &display.items {
        match item {
            DisplayItem::GlyphPath {
                x,
                y,
                scale,
                font,
                char_code,
                ..
            } => {
                let Some(fi) = FACES.iter().position(|f| f.0 == font) else {
                    out.missing += 1;
                    continue;
                };
                let ch = FontId::parse(font)
                    .map_or(char::from_u32(*char_code).unwrap_or('\u{fffd}'), |id| {
                        ratex_font::katex_ttf_glyph_char(id, *char_code)
                    });
                let face = faces[fi].get_or_insert_with(|| {
                    FontRef::new(FACES[fi].1).expect("embedded KaTeX face parses")
                });
                let Some(gid) = face.charmap().map(ch) else {
                    out.missing += 1;
                    continue;
                };
                out.glyph_face.push(fi as u8);
                out.glyph_id.push(gid.to_u32());
                out.glyph_x.push(p(*x));
                out.glyph_y.push(p(*y));
                out.glyph_px.push(p(*scale));
            }
            DisplayItem::Line {
                x,
                y,
                width,
                thickness,
                ..
            } => {
                // `y` is the rule's center line.
                out.rules.push([
                    p(*x),
                    p(y - thickness / 2.0),
                    p(*width),
                    p(*thickness).max(1.0),
                ]);
            }
            DisplayItem::Rect {
                x,
                y,
                width,
                height,
                ..
            } => out.rules.push([p(*x), p(*y), p(*width), p(*height)]),
            DisplayItem::Path {
                x,
                y,
                commands,
                fill,
                ..
            } => {
                let paths = if *fill {
                    &mut out.fills
                } else {
                    &mut out.strokes
                };
                for c in commands {
                    let (verb, pts): (Verb, &[(f64, f64)]) = match *c {
                        PathCommand::MoveTo { x: a, y: b } => (Verb::Move, &[(a, b)]),
                        PathCommand::LineTo { x: a, y: b } => (Verb::Line, &[(a, b)]),
                        PathCommand::QuadTo { x1, y1, x: a, y: b } => {
                            (Verb::Quad, &[(x1, y1), (a, b)])
                        }
                        PathCommand::CubicTo {
                            x1,
                            y1,
                            x2,
                            y2,
                            x: a,
                            y: b,
                        } => (Verb::Cubic, &[(x1, y1), (x2, y2), (a, b)]),
                        PathCommand::Close => (Verb::Close, &[]),
                    };
                    paths.verb.push(verb);
                    paths
                        .pt
                        .extend(pts.iter().map(|&(a, b)| [p(x + a), p(y + b)]));
                }
                paths.verb_end.push(paths.verb.len() as u32);
            }
        }
    }
    debug_assert_eq!(out.glyph_id.len(), out.glyph_px.len());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_has_glyphs_and_a_bar() {
        let fonts = MathFonts::new();
        let m = layout_math(r"g_m = \frac{2 I_D}{V_{ov}}", 20.0, &fonts).unwrap();
        assert_eq!(m.missing, 0);
        assert!(m.glyph_id.len() >= 9, "glyphs {}", m.glyph_id.len());
        assert_eq!(m.rules.len(), 1, "one fraction bar");
        assert!(m.width > 60.0 && m.height > 10.0 && m.depth > 5.0);
        assert!(
            m.glyph_x
                .iter()
                .all(|&x| (0.0..=m.width + 1.0).contains(&x))
        );
    }

    #[test]
    fn sqrt_draws_a_surd_and_bad_input_errors() {
        let fonts = MathFonts::new();
        let m = layout_math(r"\sqrt{x^2+1}", 20.0, &fonts).unwrap();
        assert!(!m.glyph_id.is_empty());
        assert!(layout_math(r"\frac{1}{", 20.0, &fonts).is_err());
    }
}
