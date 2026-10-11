//! The `math` widget (D21): a LaTeX subset laid out by RaTeX into positioned glyphs, rules and
//! paths, in pixels with the origin at the box's top-left and the baseline at `height`.
//!
//! RaTeX positions glyphs with KaTeX font metrics, so the glyphs must come from the KaTeX fonts
//! (embedded, OFL). D21 names an OpenType MATH font; that would need a MATH-table layout engine,
//! which RaTeX is not.

use std::sync::Arc;

use parley::FontData;
use parley::fontique::Blob;
use ratex_font::FontId;
use ratex_types::display_item::DisplayItem;
use ratex_types::path_command::PathCommand;
use skrifa::MetadataProvider;

/// KaTeX faces in [`FontId`] spelling, with their bytes.
const FACES: [(&str, &[u8]); 19] = [
    (
        "AMS-Regular",
        include_bytes!("../fonts/KaTeX_AMS-Regular.ttf"),
    ),
    (
        "Caligraphic-Regular",
        include_bytes!("../fonts/KaTeX_Caligraphic-Regular.ttf"),
    ),
    (
        "Fraktur-Regular",
        include_bytes!("../fonts/KaTeX_Fraktur-Regular.ttf"),
    ),
    (
        "Fraktur-Bold",
        include_bytes!("../fonts/KaTeX_Fraktur-Bold.ttf"),
    ),
    ("Main-Bold", include_bytes!("../fonts/KaTeX_Main-Bold.ttf")),
    (
        "Main-BoldItalic",
        include_bytes!("../fonts/KaTeX_Main-BoldItalic.ttf"),
    ),
    (
        "Main-Italic",
        include_bytes!("../fonts/KaTeX_Main-Italic.ttf"),
    ),
    (
        "Main-Regular",
        include_bytes!("../fonts/KaTeX_Main-Regular.ttf"),
    ),
    (
        "Math-BoldItalic",
        include_bytes!("../fonts/KaTeX_Math-BoldItalic.ttf"),
    ),
    (
        "Math-Italic",
        include_bytes!("../fonts/KaTeX_Math-Italic.ttf"),
    ),
    (
        "SansSerif-Bold",
        include_bytes!("../fonts/KaTeX_SansSerif-Bold.ttf"),
    ),
    (
        "SansSerif-Italic",
        include_bytes!("../fonts/KaTeX_SansSerif-Italic.ttf"),
    ),
    (
        "SansSerif-Regular",
        include_bytes!("../fonts/KaTeX_SansSerif-Regular.ttf"),
    ),
    (
        "Script-Regular",
        include_bytes!("../fonts/KaTeX_Script-Regular.ttf"),
    ),
    (
        "Size1-Regular",
        include_bytes!("../fonts/KaTeX_Size1-Regular.ttf"),
    ),
    (
        "Size2-Regular",
        include_bytes!("../fonts/KaTeX_Size2-Regular.ttf"),
    ),
    (
        "Size3-Regular",
        include_bytes!("../fonts/KaTeX_Size3-Regular.ttf"),
    ),
    (
        "Size4-Regular",
        include_bytes!("../fonts/KaTeX_Size4-Regular.ttf"),
    ),
    (
        "Typewriter-Regular",
        include_bytes!("../fonts/KaTeX_Typewriter-Regular.ttf"),
    ),
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

/// The embedded KaTeX faces, indexed by the `u8` face index stored in [`MathList::glyph_face`].
pub struct MathFonts {
    data: Vec<FontData>,
}

impl Default for MathFonts {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MathFonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MathFonts")
            .field("faces", &self.data.len())
            .finish()
    }
}

impl MathFonts {
    /// Wraps the embedded faces (no copy: the blobs borrow the static bytes).
    pub fn new() -> Self {
        let data = FACES
            .iter()
            .map(|(_, b)| FontData::new(Blob::new(Arc::new(*b)), 0))
            .collect();
        MathFonts { data }
    }

    /// Font for a face index from [`MathList::glyph_face`].
    pub fn face(&self, i: u8) -> &FontData {
        &self.data[i as usize]
    }
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

/// Parses and lays out `src` at `px` pixels per em.
///
/// # Errors
///
/// Returns RaTeX's parse message for input outside its LaTeX subset.
pub fn layout_math(src: &str, px: f32, fonts: &MathFonts) -> Result<MathList, String> {
    assert!(px > 0.0 && px.is_finite(), "math size must be positive");
    assert_eq!(fonts.data.len(), FACES.len());
    let ast = ratex_parser::parse(src).map_err(|e| e.to_string())?;
    let display = ratex_layout::to_display_list(&ratex_layout::layout(
        &ast,
        &ratex_layout::LayoutOptions::default(),
    ));
    let mut out = MathList {
        width: (display.width * f64::from(px)) as f32,
        height: (display.height * f64::from(px)) as f32,
        depth: (display.depth * f64::from(px)) as f32,
        ..MathList::default()
    };
    let s = f64::from(px);
    let p = |v: f64| (v * s) as f32;
    // Parsed faces cached by index: a formula touches few faces, and parsing one is a table scan.
    let mut faces: [Option<skrifa::FontRef<'static>>; FACES.len()] = [const { None }; FACES.len()];
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
                    skrifa::FontRef::new(FACES[fi].1).expect("embedded KaTeX face parses")
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
