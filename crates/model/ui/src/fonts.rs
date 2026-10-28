//! The embedded UI faces and their export metrics: text in, one face, glyph id and advance per
//! character out, for exporters that embed the faces the canvas draws with (PDF `FontFile2`,
//! PostScript Type 42).
//!
//! Contents:
//! - `pub(crate) static NOTO_SANS: &[u8]`
//! - `pub struct FontInfo`, `pub struct FontGlyph`
//! - `pub fn ui_fonts() -> [&'static [u8]; 2]`
//! - `pub fn ui_font_info(face: u8) -> FontInfo`
//! - `pub fn ui_font_glyphs(text: &str, out: &mut Vec<FontGlyph>)`

/// Noto Sans subset to the UI's scripts and symbols (fonts/README.md has the command).
pub(crate) static NOTO_SANS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fonts/NotoSans-UI.ttf"
));

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
#[must_use]
pub fn ui_fonts() -> [&'static [u8]; 2] {
    [NOTO_SANS, crate::math::face_bytes("Main-Regular")]
}

/// The metrics of face `face` of [`ui_fonts`].
///
/// # Panics
///
/// Panics if `face` is not an index into [`ui_fonts`].
#[must_use]
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

// ---- private ----

/// Face `face` of [`ui_fonts`], parsed.
fn ui_face(face: u8) -> skrifa::FontRef<'static> {
    let bytes = ui_fonts()[usize::from(face)];
    skrifa::FontRef::new(bytes).expect("an embedded UI face parses")
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
}
