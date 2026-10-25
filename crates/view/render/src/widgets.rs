//! Paints `ui_layout` rows: backgrounds, borders, text, indicators, and the `math`
//! widget. Reads the UI tables; writes only painter calls.

use kurbo::{Affine, BezPath, Rect};
use peniko::Color;
use vello_cpu::Glyph;

use schemify_ui::icon::{ICON_BOX, ICON_STROKE, Icon};
use schemify_ui::math::{MathFonts, MathList, MathPaths, Verb};
use schemify_ui::parley::{Layout, PositionedLayoutItem};
use schemify_ui::{
    Kind, Layer, NO_TEXT, Prop, RowPaint, Shown, Strings, StyleProp, TextSystem, UiLayout, UiState,
    UiTables, row_paint, row_style,
};

use crate::painter::{Painter, outline, rgba};

/// Reused buffers for UI painting.
#[derive(Debug, Default)]
pub struct UiScratch {
    glyphs: Vec<Glyph>,
    coords: Vec<i16>,
    path: BezPath,
    images: Vec<peniko::ImageData>,
}

/// A `ui_layout` rect as a kurbo rect.
pub(crate) fn rect(r: schemify_ui::Rect) -> Rect {
    Rect::new(
        f64::from(r.x),
        f64::from(r.y),
        f64::from(r.x + r.w),
        f64::from(r.y + r.h),
    )
}

/// Paints the rows of one layer. Call with [`Layer::Main`], draw the canvas into the canvas
/// row's rect, then call with [`Layer::Overlay`] so popups cover the schematic.
#[allow(clippy::too_many_arguments)]
pub fn draw_ui<P: Painter>(
    p: &mut P,
    tables: &UiTables,
    layout: &UiLayout,
    strings: &Strings,
    text: &mut TextSystem,
    state: &UiState,
    layer: Layer,
    scratch: &mut UiScratch,
) {
    assert_eq!(layout.rect().len(), layout.len());
    assert_eq!(layout.clip().len(), layout.len());
    // Rows inside a scroll pane are clipped to it; consecutive rows share one clip layer.
    let mut clipped: Option<schemify_ui::Rect> = None;
    let rows = layout.rows(layer);
    let mut r = rows.start;
    while r < rows.end {
        let clip = layout.clip()[r];
        if clipped != Some(clip).filter(|c| *c != schemify_ui::NO_CLIP) {
            if clipped.take().is_some() {
                p.pop_clip();
            }
            if clip != schemify_ui::NO_CLIP {
                p.push_clip(rect(clip));
                clipped = Some(clip);
            }
        }
        let vis = layout.rect()[r].intersect(&clip);
        if vis.w <= 0.0 || vis.h <= 0.0 {
            r += 1;
            continue; // scrolled out of view
        }
        draw_row(p, tables, layout, strings, text, state, scratch, r);
        r += 1;
    }
    if clipped.is_some() {
        p.pop_clip();
    }
}

/// Paints row `r` of the layout: background, border, text, indicators and its widget.
#[allow(clippy::too_many_arguments)]
fn draw_row<P: Painter>(
    p: &mut P,
    tables: &UiTables,
    layout: &UiLayout,
    strings: &Strings,
    text: &mut TextSystem,
    state: &UiState,
    scratch: &mut UiScratch,
    r: usize,
) {
    let n = &tables.nodes;
    let node = layout.node()[r];
    let i = node as usize;
    let shown = layout.shown()[r];
    let kind = n.kind()[i];
    let id = n.id()[i];
    let style = |prop| row_style(tables, layout.title_style(), node, shown, prop);
    let area = rect(layout.rect()[r]);
    let RowPaint { bg, fg } = row_paint(tables, layout, strings, state, r);
    let fg = rgba(fg);
    if bg & 0xff != 0 && kind != Kind::Canvas {
        p.fill_rect(area, rgba(bg));
    }
    let border = style(StyleProp::Border);
    if border > 0 {
        let w = f64::from(border);
        let color = rgba(style(StyleProp::BorderColor));
        outline(p, &mut scratch.path, area.inset(-w / 2.0), w, color);
    }
    let pad = f64::from(style(StyleProp::Pad));
    let indent = f64::from(layout.indent()[r]);
    if kind == Kind::Separator {
        let y = (area.y0 + area.y1) / 2.0;
        p.fill_rect(
            Rect::new(area.x0 + 2.0, y.floor(), area.x1 - 2.0, y.floor() + 1.0),
            rgba(style(StyleProp::Fg)),
        );
        return;
    }
    let area_kind = kind == Kind::TextArea;
    let editing = matches!(kind, Kind::TextInput | Kind::TextArea) && state.focus() == id;
    let label = layout.label()[r];
    let pad_y = f64::from(style(StyleProp::PadY));
    // A text area taller than its box scrolls inside itself, clipped to the box.
    let scrolled = if area_kind {
        let max = schemify_ui::area_scroll_max(tables, layout, text, state, r);
        p.push_clip(area);
        f64::from(state.scroll_of(id).min(max))
    } else {
        0.0
    };
    if editing && let Some(ed) = state.editor().and_then(|e| e.try_layout()) {
        let y = if area_kind {
            area.y0 + pad_y - scrolled
        } else {
            area.y0 + (area.height() - f64::from(ed.height())) / 2.0
        };
        let (x, w) = (area.x0 + pad, f64::from(ed.width()));
        draw_text(p, ed, x, y, fg, scratch);
        if let Some(c) = state.editor().and_then(|e| e.cursor_geometry(1.0)) {
            p.fill_rect(Rect::new(x + c.x0, y + c.y0, x + c.x0 + 1.0, y + c.y1), fg);
        }
        debug_assert!(w >= 0.0);
    } else if label != NO_TEXT {
        let l = text.layout(label);
        let y = if area_kind {
            area.y0 + pad_y - scrolled
        } else {
            area.y0 + (area.height() - f64::from(l.height())) / 2.0
        };
        let x = match kind {
            Kind::Checkbox => {
                area.x0 + pad + f64::from(l.height()) + f64::from(style(StyleProp::Gap))
            }
            _ => area.x0 + pad + indent,
        };
        draw_text(p, l, x, y, fg, scratch);
    }
    if area_kind {
        p.pop_clip();
    }
    let accel = layout.accel()[r];
    if accel != NO_TEXT {
        let l = text.layout(accel);
        let y = area.y0 + (area.height() - f64::from(l.height())) / 2.0;
        let color = rgba(style(StyleProp::AccelFg));
        draw_text(
            p,
            l,
            area.x1 - pad - f64::from(l.width()),
            y,
            color,
            scratch,
        );
    }
    let icon = layout.icon()[r];
    if icon != u32::MAX {
        draw_icon(p, text.icon_paths(icon), area, (pad, pad_y), fg, scratch);
    }
    if shown == Shown::Cascade || (kind == Kind::Dropdown && shown == Shown::Normal) {
        let (cx, cy) = (area.x1 - pad - 4.0, (area.y0 + area.y1) / 2.0);
        let path = &mut scratch.path;
        path.truncate(0);
        if kind == Kind::Dropdown {
            path.move_to((cx - 4.0, cy - 2.0));
            path.line_to((cx + 4.0, cy - 2.0));
            path.line_to((cx, cy + 3.0));
        } else {
            path.move_to((cx - 3.0, cy - 4.0));
            path.line_to((cx + 2.0, cy));
            path.line_to((cx - 3.0, cy + 4.0));
        }
        path.close_path();
        p.fill_path(path, fg);
    }
    let checkable =
        !n.prop(Prop::Checked)[i].is_none() && matches!(kind, Kind::Item | Kind::Checkbox);
    if checkable {
        let size = 9.0;
        let x = area.x0 + pad;
        let y = ((area.y0 + area.y1) / 2.0 - size / 2.0).floor();
        let b = Rect::new(x, y, x + size, y + size);
        let path = &mut scratch.path;
        path.truncate(0);
        if style(StyleProp::Radius) > 0 {
            path.extend(kurbo::Shape::path_elements(
                &kurbo::Circle::new(b.center(), size / 2.0),
                0.1,
            ));
        } else {
            path.extend(kurbo::Shape::path_elements(&b.inset(-0.5), 0.1));
        }
        p.stroke_path(path, 1.0, fg);
        if n.is_checked(strings, node) {
            p.fill_rect(b.inset(-2.0), fg);
        }
    }
    if kind == Kind::Math && layout.math()[r] != u32::MAX {
        let list = text.math_list(layout.math()[r]);
        let at = Affine::translate((area.x0 + pad, area.y0 + f64::from(style(StyleProp::PadY))));
        draw_math(p, list, text.math_fonts(), at, fg, scratch);
    }
    if kind == Kind::Prose && layout.prose()[r] != u32::MAX {
        draw_prose(
            p,
            layout.prose()[r],
            text,
            (area.x0 + pad, area.y0 + pad_y),
            fg,
            scratch,
        );
    }
}

/// Draws an [`Icon`] scaled from its 16-unit box to the largest square inside `area`'s
/// padding `(x, y)`, centered; strokes are [`ICON_STROKE`] units wide.
fn draw_icon<P: Painter>(
    p: &mut P,
    icon: &Icon,
    area: Rect,
    pad: (f64, f64),
    color: Color,
    scratch: &mut UiScratch,
) {
    let side = (area.width() - 2.0 * pad.0).min(area.height() - 2.0 * pad.1);
    if side <= 0.0 {
        return;
    }
    let s = side / f64::from(ICON_BOX);
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

/// Draws prose layout `id` ([`TextSystem::prose`]) with its top-left at `at`: code shades,
/// rules, images, text runs, then formulas.
pub fn draw_prose<P: Painter>(
    p: &mut P,
    id: u32,
    text: &TextSystem,
    at: (f64, f64),
    color: Color,
    scratch: &mut UiScratch,
) {
    let list = text.prose_list(id);
    assert_eq!(list.text_id.len(), list.text_y.len());
    assert_eq!(list.math_id.len(), list.math_y.len());
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
pub fn draw_math<P: Painter>(
    p: &mut P,
    list: &MathList,
    fonts: &MathFonts,
    at: Affine,
    color: Color,
    scratch: &mut UiScratch,
) {
    let g = list.glyph_id.len();
    assert!(
        list.glyph_face.len() == g
            && list.glyph_x.len() == g
            && list.glyph_y.len() == g
            && list.glyph_px.len() == g
    );
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
