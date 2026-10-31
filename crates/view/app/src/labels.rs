//! Canvas labels as egui text: a [`Texts`] table and a camera in, text shapes out, culled to
//! the view and skipped under [`TEXT_MIN_PX`]. Each label is laid out at the octave size above
//! its screen size (8, 16, 32 px …) and scaled down to it, so a zoom reuses egui's cached
//! layouts and glyphs until it crosses an octave (measured 2026-10-30, 200 labels: 0.24 ms a
//! zoom frame against 1.6 ms laying each out at its exact size).
//!
//! Contents: `pub(crate) fn draw(ctx, view, cam, texts, rows, palette, out)`

use schemify_canvas::kurbo::Rect;
use schemify_canvas::{Camera, LAYERS, Palette, Texts};
use schemify_render::egui::{self, FontId, Shape, emath::TSTransform, epaint::TextShape};
use schemify_render::gpu::TEXT_MIN_PX;

/// Smallest and largest layout size, px.
const OCTAVES: (f32, f32) = (8.0, 256.0);

/// Appends a text shape to `out` for each live label of `t` (all, or the listed `rows`) that
/// is big enough to read and may touch `view`; returns how many.
pub(crate) fn draw(
    ctx: &egui::Context,
    view: Rect,
    cam: &Camera,
    t: &Texts,
    rows: Option<&[u32]>,
    palette: &Palette,
    out: &mut Vec<Shape>,
) -> usize {
    assert_eq!(t.x.len(), t.end.len(), "text columns in step");
    assert!(cam.scale > 0.0, "a drawable camera, scale {}", cam.scale);
    let (ox, oy) = cam.origin(view);
    let before = out.len();
    let all = rows.is_none().then_some(0..t.x.len()).into_iter().flatten();
    let listed = all.chain(rows.into_iter().flatten().map(|&i| i as usize));
    for i in listed {
        let layer = usize::from(t.layer[i]);
        if layer >= LAYERS {
            continue; // dead
        }
        let px = f64::from(t.size[i]) * cam.scale;
        let (x, y) = (
            f64::from(t.x[i]) * cam.scale + ox,
            f64::from(t.y[i]) * cam.scale + oy,
        );
        // A turned label extends left/up of its anchor too: only the size culls it.
        let rot = t.rot.get(i).copied().unwrap_or(0);
        let visible = px >= TEXT_MIN_PX
            && (rot != 0 || (x < view.x1 && y < view.y1 && y + px * 2.0 > view.y0));
        if !visible {
            continue;
        }
        let text = &t.text[schemify_canvas::chain(&t.end, i)];
        let base = (px as f32).log2().ceil().exp2().clamp(OCTAVES.0, OCTAVES.1);
        let [r, g, b, a] = palette.color[layer].to_be_bytes();
        let color = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
        let galley =
            ctx.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), FontId::proportional(base), color));
        let angle = f32::from(rot) * std::f32::consts::FRAC_PI_2;
        let mut shape =
            Shape::Text(TextShape::new(egui::Pos2::ZERO, galley, color).with_angle(angle));
        shape.transform(TSTransform::new(
            egui::vec2(x as f32, y as f32),
            px as f32 / base,
        ));
        out.push(shape);
    }
    out.len() - before
}
