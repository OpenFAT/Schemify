//! Labels on the CPU: a [`Texts`] table and a camera in, glyph runs out. Each distinct string
//! is shaped once at a reference size into the scratch's cache and drawn scaled by the
//! camera; labels too small to read are skipped. Texts are not instance rows: the painter
//! draws them after each table's rows.
//!
//! Contents:
//! - `pub fn draw_texts<P: Painter>(p: &mut P, view: Rect, cam: &Camera, t: &Texts, rows: Option<&[u32]>, palette: &Palette, text: &mut TextSystem, scratch: &mut CanvasScratch) -> usize`

use kurbo::{Affine, Rect};
use vello_cpu::Glyph;

use schemify_canvas::{Camera, LAYERS, Palette, Texts, chain};
use schemify_ui::TextSystem;
use schemify_ui::parley::PositionedLayoutItem;

use super::{CanvasScratch, TEXT_MIN_PX};
use crate::painter::{Painter, rgba};

/// Labels are shaped once at this size and scaled by the camera.
const TEXT_REF_PX: f32 = 64.0;
/// Distinct strings the label cache keeps before it starts over (a phase boundary, like the UI
/// text cache).
const TEXT_RUNS_MAX: usize = 1 << 16;


/// Draws the labels of `t` (the rows `rows` lists, or all), culled to `view` and skipping any
/// under [`TEXT_MIN_PX`] on screen; returns the labels drawn. Text is not an instance row: the
/// painter draws it after each table's rows until the glyph atlas lands.
///
/// Cost: O(listed rows + glyphs of visible labels).
#[allow(clippy::too_many_arguments)]
pub fn draw_texts<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    t: &Texts,
    rows: Option<&[u32]>,
    palette: &Palette,
    text: &mut TextSystem,
    scratch: &mut CanvasScratch,
) -> usize {
    assert_eq!(t.x.len(), t.end.len());
    let (ox, oy) = cam.origin(view);
    let mut drawn = 0;
    for i in each(t.x.len(), rows) {
        if usize::from(t.layer[i]) >= LAYERS {
            continue; // dead
        }
        let bytes = chain(&t.end, i);
        let px = f64::from(t.size[i]) * cam.scale;
        let (x, y) = (
            f64::from(t.x[i]) * cam.scale + ox,
            f64::from(t.y[i]) * cam.scale + oy,
        );
        // A turned label extends left/up of its anchor too: only the size culls it.
        let rot = t.rot.get(i).copied().unwrap_or(0);
        let visible = px >= TEXT_MIN_PX
            && (rot != 0 || (x < view.x1 && y < view.y1 && y + px * 2.0 > view.y0));
        if visible {
            let (g0, g1) = shaped(&t.text[bytes], text, scratch);
            if let (Some(face), true) = (scratch.text_face.clone(), g1 > g0) {
                let at = Affine::translate((x, y))
                    * Affine::rotate(f64::from(rot) * std::f64::consts::FRAC_PI_2)
                    * Affine::scale(px / f64::from(TEXT_REF_PX));
                let color = rgba(palette.color[usize::from(t.layer[i])]);
                let glyphs = &scratch.text_glyphs[g0 as usize..g1 as usize];
                p.glyphs(&face, TEXT_REF_PX, &[], color, at, glyphs);
                drawn += 1;
            }
        }
    }
    drawn
}

// ---- private ----


/// Every row `0..n`, or the listed ones.
fn each(n: usize, rows: Option<&[u32]>) -> impl Iterator<Item = usize> + '_ {
    let all = rows.is_none().then_some(0..n).into_iter().flatten();
    all.chain(rows.into_iter().flatten().map(|&i| i as usize))
}


/// The glyph range of `s` shaped at [`TEXT_REF_PX`] (positions relative to its top-left),
/// shaping it on first use: a string is shaped once per cache lifetime, however many labels or
/// frames show it.
fn shaped(s: &str, text: &mut TextSystem, scratch: &mut CanvasScratch) -> (u32, u32) {
    if let Some(&r) = scratch.text_runs.get(s) {
        return r;
    }
    if scratch.text_runs.len() >= TEXT_RUNS_MAX {
        scratch.text_runs.clear();
        scratch.text_glyphs.clear();
    }
    let g0 = scratch.text_glyphs.len() as u32;
    text.shape_into(s, TEXT_REF_PX, &mut scratch.text_layout);
    for line in scratch.text_layout.lines() {
        for item in line.items() {
            if let PositionedLayoutItem::GlyphRun(run) = item {
                // ponytail: one face for all labels (the only registered UI font). Font
                // fallback would need a face per glyph range.
                scratch
                    .text_face
                    .get_or_insert_with(|| run.run().font().clone());
                scratch
                    .text_glyphs
                    .extend(run.positioned_glyphs().map(|g| Glyph {
                        id: g.id,
                        x: g.x,
                        y: g.y,
                    }));
            }
        }
    }
    let r = (g0, scratch.text_glyphs.len() as u32);
    scratch.text_runs.insert(s.into(), r);
    assert!(r.0 <= r.1);
    r
}
