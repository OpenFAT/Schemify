//! Text shaping and measurement (parley).
//!
//! The embedded UI font (a Noto Sans subset, `fonts/README.md`) is registered and named by
//! every style, so every string the UI ships shapes the same on every platform (D32) and the
//! golden images stay deterministic. System fonts (fontique) are consulted only for
//! characters outside the subset; the web build has none.

use std::collections::HashMap;
use std::sync::Arc;

use parley::fontique::Blob;
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, Layout, LayoutContext, StyleProperty,
};

use crate::math::{MathFonts, MathList};
use crate::prose::ProseList;
use crate::strings::{Strings, Sym};

/// Index of a shaped layout in [`TextSystem`]; valid until the next [`TextSystem::begin_frame`]
/// that evicts.
pub type TextId = u32;

/// "No text" sentinel for [`TextId`] columns.
pub const NO_TEXT: TextId = u32::MAX;

/// Noto Sans subset to the UI's scripts and symbols (fonts/README.md has the command).
static NOTO_SANS: &[u8] = include_bytes!("../fonts/NotoSans-UI.ttf");
/// Monospace face for code (prose code spans and blocks); KaTeX's, already embedded for math.
static MONO: &[u8] = include_bytes!("../fonts/KaTeX_Typewriter-Regular.ttf");

/// Font database, shaping scratch, and the cache of shaped UI strings and math.
///
/// Capacity: [`TextSystem::CACHE_CAPACITY`] cached layouts; beyond that the whole cache is
/// dropped at the next [`TextSystem::begin_frame`] (a phase boundary), so a session that keeps
/// producing new strings stays bounded.
pub struct TextSystem {
    pub(crate) font_cx: FontContext,
    pub(crate) layout_cx: LayoutContext<()>,
    family: String,
    mono: String,
    layouts: Vec<Layout<()>>,
    cache: HashMap<(Sym, u16), TextId>,
    math_fonts: MathFonts,
    maths: Vec<MathList>,
    math_cache: HashMap<(Sym, u16), u32>,
    proses: Vec<ProseList>,
    prose_cache: HashMap<(Sym, u16, u16), u32>,
}

impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TextSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextSystem")
            .field("layouts", &self.layouts.len())
            .finish()
    }
}

impl TextSystem {
    /// Cached layouts kept across frames before the cache is reset.
    pub const CACHE_CAPACITY: usize = 1 << 14;

    /// Creates the system and registers the embedded fonts. UI text asks for the Noto Sans
    /// subset, then KaTeX Main (already embedded for math) for the arrows and relations Noto
    /// Sans lacks (→ ≤ ≥), and only then the system fallback.
    pub fn new() -> Self {
        let mut font_cx = FontContext::default();
        let mut register = |bytes: &'static [u8]| {
            let families = font_cx
                .collection
                .register_fonts(Blob::new(Arc::new(bytes)), None);
            families
                .first()
                .and_then(|(id, _)| font_cx.collection.family_name(*id))
                .expect("an embedded font registers a family")
                .to_owned()
        };
        let noto = register(NOTO_SANS);
        let symbols = register(crate::math::face_bytes("Main-Regular"));
        let mono = register(MONO);
        // A CSS family list: every builder parses it (`FontFamily::from(&str)`).
        let family = format!("\"{noto}\", \"{symbols}\"");
        assert!(noto != mono && symbols != mono);
        TextSystem {
            font_cx,
            layout_cx: LayoutContext::new(),
            family,
            mono,
            layouts: Vec::new(),
            cache: HashMap::new(),
            math_fonts: MathFonts::new(),
            maths: Vec::new(),
            math_cache: HashMap::new(),
            proses: Vec::new(),
            prose_cache: HashMap::new(),
        }
    }

    /// Marks a layout pass boundary: if the cache is over capacity it is emptied, which
    /// invalidates every [`TextId`] handed out before this call.
    pub fn begin_frame(&mut self) {
        if self.layouts.len() + self.maths.len() + self.proses.len() > Self::CACHE_CAPACITY {
            self.layouts.clear();
            self.cache.clear();
            self.maths.clear();
            self.math_cache.clear();
            self.proses.clear();
            self.prose_cache.clear();
        }
    }

    /// Shapes `text` at `px` pixels (single line, no wrapping), cached by (string, size).
    pub fn shape(&mut self, strings: &Strings, text: Sym, px: u16) -> TextId {
        assert!(px > 0, "font size must be positive");
        if let Some(&id) = self.cache.get(&(text, px)) {
            return id;
        }
        let mut layout = Layout::new();
        self.shape_into(strings.get(text), f32::from(px), &mut layout);
        let id = self.layouts.len() as TextId;
        self.layouts.push(layout);
        self.cache.insert((text, px), id);
        id
    }

    /// Shapes uncached text into `out` (reusing its allocation): canvas labels, editors.
    pub fn shape_into(&mut self, text: &str, px: f32, out: &mut Layout<()>) {
        assert!(px > 0.0 && px.is_finite(), "font size must be positive");
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, true);
        builder.push_default(StyleProperty::FontFamily(FontFamily::from(
            self.family.as_str(),
        )));
        builder.push_default(StyleProperty::FontSize(px));
        builder.build_into(out, text);
        out.break_all_lines(None);
        out.align(Alignment::Start, AlignmentOptions::default());
    }

    pub(crate) fn family(&self) -> &str {
        &self.family
    }

    pub(crate) fn mono(&self) -> &str {
        &self.mono
    }

    pub(crate) fn contexts(&mut self) -> (&mut FontContext, &mut LayoutContext<()>) {
        (&mut self.font_cx, &mut self.layout_cx)
    }

    /// Stores a layout built outside [`TextSystem::shape`] (prose runs) and returns its id.
    pub(crate) fn push_layout(&mut self, layout: Layout<()>) -> TextId {
        self.layouts.push(layout);
        (self.layouts.len() - 1) as TextId
    }

    /// Lays out math that has no interned source (prose formulas); `None` outside the subset.
    pub(crate) fn math_uncached(&mut self, src: &str, px: f32) -> Option<u32> {
        let list = crate::math::layout_math(src, px, &self.math_fonts).ok()?;
        self.maths.push(list);
        Some((self.maths.len() - 1) as u32)
    }

    /// Lays out markdown `src` (the `prose` widget, [`crate::prose`]) at `px` wrapped at
    /// `width` logical pixels, cached by (source, size, width). The id is valid until the next
    /// [`TextSystem::begin_frame`] that evicts.
    ///
    /// # Panics
    ///
    /// Panics if `px` or `width` is zero.
    pub fn prose(&mut self, strings: &Strings, src: Sym, px: u16, width: u16) -> u32 {
        assert!(px > 0 && width > 0, "prose needs a size and a width");
        if let Some(&id) = self.prose_cache.get(&(src, px, width)) {
            return id;
        }
        let list =
            crate::prose::layout_prose(self, strings.get(src), f32::from(px), f32::from(width));
        self.proses.push(list);
        let id = (self.proses.len() - 1) as u32;
        self.prose_cache.insert((src, px, width), id);
        id
    }

    /// A prose layout from [`TextSystem::prose`].
    pub fn prose_list(&self, id: u32) -> &ProseList {
        &self.proses[id as usize]
    }

    /// A shaped layout.
    ///
    /// # Panics
    ///
    /// Panics on [`NO_TEXT`] or a stale id.
    pub fn layout(&self, id: TextId) -> &Layout<()> {
        &self.layouts[id as usize]
    }

    /// Lays out LaTeX `src` at `px` pixels per em, cached by (source, size).
    ///
    /// # Errors
    ///
    /// Returns the parser's message when `src` is not in the supported LaTeX subset.
    pub fn math(&mut self, strings: &Strings, src: Sym, px: u16) -> Result<u32, String> {
        if let Some(&id) = self.math_cache.get(&(src, px)) {
            return Ok(id);
        }
        let list = crate::math::layout_math(strings.get(src), f32::from(px), &self.math_fonts)?;
        let id = self.maths.len() as u32;
        self.maths.push(list);
        self.math_cache.insert((src, px), id);
        Ok(id)
    }

    /// A math layout from [`TextSystem::math`].
    pub fn math_list(&self, id: u32) -> &MathList {
        &self.maths[id as usize]
    }

    /// The embedded math fonts (render needs them to draw [`MathList`] glyphs).
    pub fn math_fonts(&self) -> &MathFonts {
        &self.math_fonts
    }
}

/// Font-wide metrics of one embedded UI face ([`ui_fonts`]) in its font units at its default
/// instance (regular weight): what a PDF font descriptor or a PostScript Type 42 header
/// states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontInfo {
    /// Font units per em.
    pub upem: u16,
    /// Glyph count: glyph ids are `0..glyphs`.
    pub glyphs: u16,
    /// Ascent above the baseline (positive).
    pub ascent: i16,
    /// Descent below the baseline (negative).
    pub descent: i16,
    /// Height of a capital letter.
    pub cap_height: i16,
    /// Union of every glyph's box, `[x_min, y_min, x_max, y_max]`.
    pub bbox: [i16; 4],
}

/// One character of exported text: the [`ui_fonts`] face that prints it, its glyph id there
/// and its advance width in that face's font units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontGlyph {
    /// Index into [`ui_fonts`].
    pub face: u8,
    /// Glyph id in that face (0 = `.notdef`).
    pub id: u16,
    /// Advance width, font units of that face.
    pub advance: u16,
}

/// The embedded UI faces in the fallback order UI text shapes with: the Noto Sans subset,
/// then KaTeX Main for the arrows and relations it lacks (→ ≤ ≥). Both are TrueType files
/// under the SIL OFL 1.1, which allows embedding: exporters embed them so printed text uses
/// the glyphs the canvas draws (PDF `FontFile2`, PostScript Type 42) instead of a base-14
/// font that only covers ASCII.
pub fn ui_fonts() -> [&'static [u8]; 2] {
    [NOTO_SANS, crate::math::face_bytes("Main-Regular")]
}

fn ui_face(face: u8) -> skrifa::FontRef<'static> {
    let bytes = ui_fonts()[usize::from(face)];
    skrifa::FontRef::new(bytes).expect("an embedded UI face parses")
}

/// The metrics of face `face` of [`ui_fonts`].
///
/// # Panics
///
/// Panics if `face` is not an index into [`ui_fonts`].
pub fn ui_font_info(face: u8) -> FontInfo {
    use skrifa::MetadataProvider;
    use skrifa::instance::{LocationRef, Size};
    let m = ui_face(face).metrics(Size::unscaled(), LocationRef::default());
    let r = |v: f32| v.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
    let b = m.bounds.unwrap_or_default();
    let info = FontInfo {
        upem: m.units_per_em,
        glyphs: m.glyph_count,
        ascent: r(m.ascent),
        descent: r(m.descent),
        cap_height: r(m.cap_height.unwrap_or(m.ascent * 0.7)),
        bbox: [r(b.x_min), r(b.y_min), r(b.x_max), r(b.y_max)],
    };
    assert!(info.upem >= 16 && info.glyphs > 1, "a usable TrueType face");
    info
}

/// Appends one [`FontGlyph`] per character of `text`: the first [`ui_fonts`] face that maps
/// the character (the canvas's fallback order), at its default instance. No shaping (no
/// kerning or ligatures): exactly what a PDF `Tj` with a `W` width array or a PostScript
/// `glyphshow` places. A character no face has gets glyph 0 of face 0 (`.notdef`, a box).
///
/// Cost: at most one charmap lookup per face per character.
pub fn ui_font_glyphs(text: &str, out: &mut Vec<FontGlyph>) {
    use skrifa::MetadataProvider;
    use skrifa::instance::{LocationRef, Size};
    let faces = [ui_face(0), ui_face(1)];
    let cmaps = [faces[0].charmap(), faces[1].charmap()];
    let metrics = [
        faces[0].glyph_metrics(Size::unscaled(), LocationRef::default()),
        faces[1].glyph_metrics(Size::unscaled(), LocationRef::default()),
    ];
    let before = out.len();
    for c in text.chars() {
        let hit = (0..faces.len()).find_map(|f| {
            let id = cmaps[f].map(c)?;
            u16::try_from(id.to_u32()).ok().map(|id| (f, id))
        });
        let (face, id) = hit.unwrap_or((0, 0));
        let adv = metrics[face]
            .advance_width(skrifa::GlyphId::new(u32::from(id)))
            .unwrap_or(0.0);
        out.push(FontGlyph {
            face: face as u8,
            id,
            advance: adv.round().clamp(0.0, f32::from(u16::MAX)) as u16,
        });
    }
    assert_eq!(out.len() - before, text.chars().count());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The export helpers pick faces in the canvas's fallback order: Noto Sans for what it
    /// has, KaTeX Main for the arrows and relations it lacks, `.notdef` for the rest.
    #[test]
    fn export_glyphs_follow_the_ui_fallback_order() {
        for face in 0..2 {
            let info = ui_font_info(face);
            assert!(info.ascent > 0 && info.descent < 0 && info.bbox[0] < info.bbox[2]);
            assert_eq!(
                &ui_fonts()[usize::from(face)][..4],
                &[0, 1, 0, 0],
                "TrueType"
            );
        }
        let mut g = Vec::new();
        ui_font_glyphs("AΩµ→≤漢", &mut g);
        assert_eq!(g.len(), 6);
        let faces: Vec<u8> = g.iter().map(|x| x.face).collect();
        assert_eq!(faces, [0, 0, 0, 1, 1, 0]);
        assert!(g[..5].iter().all(|x| x.id > 0 && x.advance > 0));
        assert!(g[..5].iter().all(|x| x.id < ui_font_info(x.face).glyphs));
        assert_eq!(g[5].id, 0, "outside both faces: .notdef");
    }

    #[test]
    fn shapes_with_embedded_font_and_caches() {
        let mut strings = Strings::new();
        let mut text = TextSystem::new();
        let a = strings.intern("File");
        let id = text.shape(&strings, a, 13);
        assert_eq!(text.shape(&strings, a, 13), id);
        assert_ne!(text.shape(&strings, a, 20), id);
        let w13 = text.layout(id).width();
        let l = text.layout(id);
        assert!(l.width() > 15.0 && l.width() < 40.0, "width {}", l.width());
        assert!(l.height() > 10.0 && l.height() < 25.0);
        let wide = text.shape(&strings, strings.find("File").unwrap(), 20);
        assert!(text.layout(wide).width() > w13);
    }

    /// The embedded fonts cover every character the UI ships (D39 cut, `fonts/README.md`),
    /// so system fonts never shape UI text: each cluster is a real glyph of the Noto subset,
    /// or of KaTeX Main for the arrows and relations Noto Sans lacks.
    #[test]
    fn ui_characters_come_from_the_embedded_fonts() {
        // ASCII, the non-ASCII characters in the app/ui/render/default-UI sources, Greek.
        let probe = "AZaz09!~ µΩ±×°→≤≥−…§Δ—· αβγδΣπ € ½";
        let mut text = TextSystem::new();
        let mut layout = Layout::new();
        text.shape_into(probe, 14.0, &mut layout);
        let mut glyphs = 0;
        for line in layout.lines() {
            for item in line.items() {
                let parley::PositionedLayoutItem::GlyphRun(run) = item else {
                    continue;
                };
                let font = run.run().font().data.data().as_ptr();
                let symbols = crate::math::face_bytes("Main-Regular").as_ptr();
                assert!(
                    font == NOTO_SANS.as_ptr() || font == symbols,
                    "a glyph from another font"
                );
                for g in run.glyphs() {
                    assert_ne!(g.id, 0, "notdef in the embedded font");
                    glyphs += 1;
                }
            }
        }
        assert!(glyphs >= probe.chars().filter(|c| !c.is_whitespace()).count());
        assert!(NOTO_SANS.len() < 200 * 1024, "the UI font is the subset");
    }
}
