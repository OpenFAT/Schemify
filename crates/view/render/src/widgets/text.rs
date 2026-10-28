//! Shaped content: text layouts, prose lists, math lists and icons in, painter calls out.
//! A glyph run becomes one [`Painter::glyphs`] call; math delimiters and icons share one path
//! encoding ([`MathPaths`]), filled then stroked.
//!
//! Contents:
//! - `pub fn draw_prose<P: Painter>(p, id, text, at, color, scratch)`
//! - `pub fn draw_text<P: Painter>(p, layout, x, y, color, scratch)`
//! - `pub fn draw_math<P: Painter>(p, list, fonts, at, color, scratch)`
//! - `pub(super) fn draw_icon<P: Painter>(p, icon, area, pad, color, scratch)`

use kurbo::{Affine, BezPath, Rect};
use peniko::Color;
use vello_cpu::Glyph;

use schemify_ui::TextSystem;
use schemify_ui::icon::{ICON_BOX, ICON_STROKE, Icon};
use schemify_ui::math::{MathFonts, MathList, MathPaths, Verb};
use schemify_ui::parley::{Layout, PositionedLayoutItem};

use super::UiScratch;
use crate::painter::Painter;

/// Draws prose layout `id` ([`TextSystem::prose`]) with its top-left at `at`: code shades,
/// rules, images, text runs, then formulas.
///
/// # Panics
///
/// Panics when `id` is not a prose layout of `text`, or its list's columns are out of step.
pub fn draw_prose<P: Painter>(
    p: &mut P,
    id: u32,
    text: &TextSystem,
    at: (f64, f64),
    color: Color,
    scratch: &mut UiScratch,
) {
    let list = text.prose_list(id);
    assert_eq!(
        list.text_id.len(),
        list.text_y.len(),
        "text columns in step"
    );
    assert_eq!(
        list.math_id.len(),
        list.math_y.len(),
        "math columns in step"
    );
    assert_eq!(
        list.text_id.len(),
        list.text_x.len(),
        "text columns in step"
    );
    assert_eq!(
        list.math_id.len(),
        list.math_x.len(),
        "math columns in step"
    );
    assert_eq!(
        list.image_id.len(),
        list.image_rect.len(),
        "image columns in step"
    );
    let r = |q: &[f32; 4]| {
        let (x, y) = (at.0 + f64::from(q[0]), at.1 + f64::from(q[1]));
        Rect::new(x, y, x + f64::from(q[2]), y + f64::from(q[3]))
    };
    for s in &list.shades {
        p.fill_rect(r(s), color.with_alpha(0.08));
    }
    for q in &list.rules {
        p.fill_rect(r(q), color.with_alpha(0.6));
    }
    for (&img, q) in list.image_id.iter().zip(&list.image_rect) {
        let data = crate::painter::image_data(&mut scratch.images, text.image(img));
        p.image(&data, r(q));
    }
    for k in 0..list.text_id.len() {
        let l = text.layout(list.text_id[k]);
        let (x, y) = (
            at.0 + f64::from(list.text_x[k]),
            at.1 + f64::from(list.text_y[k]),
        );
        draw_text(p, l, x, y, color, scratch);
    }
    for k in 0..list.math_id.len() {
        let m = text.math_list(list.math_id[k]);
        let a = Affine::translate((
            at.0 + f64::from(list.math_x[k]),
            at.1 + f64::from(list.math_y[k]),
        ));
        draw_math(p, m, text.math_fonts(), a, color, scratch);
    }
}

/// Draws every glyph run of a parley layout with its top-left at `(x, y)`.
pub fn draw_text<P: Painter>(
    p: &mut P,
    layout: &Layout<()>,
    x: f64,
    y: f64,
    color: Color,
    scratch: &mut UiScratch,
) {
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(run) = item else {
                continue;
            };
            scratch.glyphs.clear();
            scratch
                .glyphs
                .extend(run.positioned_glyphs().map(|g| Glyph {
                    id: g.id,
                    x: g.x,
                    y: g.y,
                }));
            let r = run.run();
            scratch.coords.clear();
            scratch
                .coords
                .extend(r.normalized_coords().iter().map(|c| c.to_bits()));
            // A synthesized oblique (italic of an upright-only face) slants about the baseline.
            let mut at = Affine::translate((x, y));
            if let Some(deg) = r.synthesis().skew() {
                let b = f64::from(run.baseline());
                let k = -f64::from(deg).to_radians().tan();
                at = at
                    * Affine::translate((0.0, b))
                    * Affine::skew(k, 0.0)
                    * Affine::translate((0.0, -b));
            }
            p.glyphs(
                r.font(),
                r.font_size(),
                &scratch.coords,
                color,
                at,
                &scratch.glyphs,
            );
        }
    }
}

/// Draws a [`MathList`] (glyphs, rules, delimiter paths) in the space of `at`.
///
/// # Panics
///
/// Panics when the list's glyph columns are out of step.
pub fn draw_math<P: Painter>(
    p: &mut P,
    list: &MathList,
    fonts: &MathFonts,
    at: Affine,
    color: Color,
    scratch: &mut UiScratch,
) {
    let g = list.glyph_id.len();
    assert_eq!(list.glyph_face.len(), g, "glyph columns in step");
    assert_eq!(list.glyph_x.len(), g, "glyph columns in step");
    assert_eq!(list.glyph_y.len(), g, "glyph columns in step");
    assert_eq!(list.glyph_px.len(), g, "glyph columns in step");
    // One run per maximal stretch of glyphs sharing face and size.
    let mut k = 0;
    while k < g {
        let (face, px) = (list.glyph_face[k], list.glyph_px[k]);
        scratch.glyphs.clear();
        while k < g && list.glyph_face[k] == face && list.glyph_px[k] == px {
            scratch.glyphs.push(Glyph {
                id: list.glyph_id[k],
                x: list.glyph_x[k],
                y: list.glyph_y[k],
            });
            k += 1;
        }
        p.glyphs(fonts.face(face), px, &[], color, at, &scratch.glyphs);
    }
    for r in &list.rules {
        let a = at * kurbo::Point::new(f64::from(r[0]), f64::from(r[1]));
        let b = at * kurbo::Point::new(f64::from(r[0] + r[2]), f64::from(r[1] + r[3]));
        p.fill_rect(Rect::from_points(a, b), color);
    }
    let em = list.glyph_px.first().copied().unwrap_or(16.0);
    let width = f64::from(em) * 0.04;
    draw_paths(
        p,
        [&list.fills, &list.strokes],
        at,
        width,
        color,
        &mut scratch.path,
    );
}

/// Draws an [`Icon`] scaled from its 16-unit box to the largest square inside `area`'s
/// padding `(x, y)`, centered; strokes are [`ICON_STROKE`] units wide.
pub(super) fn draw_icon<P: Painter>(
    p: &mut P,
    icon: &Icon,
    area: Rect,
    pad: (f64, f64),
    color: Color,
    scratch: &mut UiScratch,
) {
    assert!(pad.0 >= 0.0, "an icon's horizontal padding");
    assert!(pad.1 >= 0.0, "an icon's vertical padding");
    let side = (area.width() - 2.0 * pad.0).min(area.height() - 2.0 * pad.1);
    if side <= 0.0 {
        return;
    }
    let s = side / f64::from(ICON_BOX);
    debug_assert!(s > 0.0, "an icon scales up from its box");
    let c = area.center();
    let at = Affine::translate((c.x - side / 2.0, c.y - side / 2.0)) * Affine::scale(s);
    let width = f64::from(ICON_STROKE) * s;
    draw_paths(
        p,
        [&icon.fill, &icon.stroke],
        at,
        width,
        color,
        &mut scratch.path,
    );
}

// ---- private ----

/// Fills every path of `fill`, then strokes every path of `stroke` `width` px wide, each
/// mapped through `at` and built in `out`: icons and math delimiters share the encoding.
fn draw_paths<P: Painter>(
    p: &mut P,
    [fill, stroke]: [&MathPaths; 2],
    at: Affine,
    width: f64,
    color: Color,
    out: &mut BezPath,
) {
    for (paths, filled) in [(fill, true), (stroke, false)] {
        let (mut v0, mut pt) = (0usize, 0usize);
        for &v1 in &paths.verb_end {
            out.truncate(0);
            let mut take = || {
                let q = paths.pt[pt];
                pt += 1;
                at * kurbo::Point::new(f64::from(q[0]), f64::from(q[1]))
            };
            for v in &paths.verb[v0..v1 as usize] {
                match v {
                    Verb::Move => out.move_to(take()),
                    Verb::Line => out.line_to(take()),
                    Verb::Quad => {
                        let (a, b) = (take(), take());
                        out.quad_to(a, b);
                    }
                    Verb::Cubic => {
                        let (a, b, c) = (take(), take(), take());
                        out.curve_to(a, b, c);
                    }
                    Verb::Close => out.close_path(),
                }
            }
            if filled {
                p.fill_path(out, color);
            } else {
                p.stroke_path(out, width, color);
            }
            v0 = v1 as usize;
        }
        debug_assert_eq!(pt, paths.pt.len(), "every point drawn");
    }
}
