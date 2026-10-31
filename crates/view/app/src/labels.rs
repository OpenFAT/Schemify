//! Canvas labels as egui text, laid out once per zoom octave and reused by every frame inside
//! it. A [`Group`] holds one table's labels in octave space (world units times the octave
//! `2^ceil(log2 scale)`, about an anchor near the view) for a cover box around the view; the
//! frame hands the GPU the group's mesh (tessellated once per set of labels shown) and the
//! camera as its transform ([`LabelMesh`]), so a pan, a hover or a keystroke lays out,
//! tessellates and uploads no text (measured 2026-10-31, 1k labels: 5.7 ms of a hover frame
//! laying out and scaling them per frame, 1.6 ms caching the layouts but tessellating them
//! per frame). A label's layout size is its octave size, clamped to [`OCTAVES`] (scaled once
//! at build past them), so it is drawn at half to whole the size it was rasterised at.
//!
//! Contents: `pub(crate) type Key`, `pub(crate) struct Group` (`stale`, `build`, `draw`),
//! `pub(crate) fn octave`

use std::sync::Arc;

use schemify_canvas::kurbo::Rect;
use schemify_canvas::{Camera, LAYERS, Palette, Texts};
use schemify_render::egui::epaint::{Mesh, Tessellator, TextShape};
use schemify_render::egui::{self, FontId, Shape, emath::TSTransform};
use schemify_render::gpu::{LabelMesh, TEXT_MIN_PX};

/// What a group's layouts depend on: the table's generation plus 1, the octave's bits, the
/// style key and the pixels per point's bits.
pub(crate) type Key = (u64, u64, u64, u32);

/// One table's labels laid out for one zoom octave over a cover box.
#[derive(Default)]
pub(crate) struct Group {
    /// What the layouts depend on ([`Key`]); 0 is never built.
    key: Key,
    /// The world box the group holds every label near.
    cover: [i32; 4],
    /// The octave scale: world units to octave-space points.
    octave: f64,
    /// The world point octave space is measured from.
    anchor: (f64, f64),
    /// Each label's world size, largest first; `shapes` in step.
    sizes: Vec<i32>,
    /// The labels as text shapes in octave space.
    shapes: Vec<Shape>,
    /// `shapes[..mesh_n]` tessellated (`usize::MAX`: not since the last build) against a
    /// glyph atlas of size `atlas`.
    mesh: Arc<Mesh>,
    mesh_n: usize,
    atlas: [usize; 2],
}

/// Smallest and largest layout size, px.
const OCTAVES: (f64, f64) = (8.0, 256.0);

/// The zoom octave of camera scale `scale`: the power of two at or above it.
pub(crate) fn octave(scale: f64) -> f64 {
    assert!(
        scale > 0.0 && scale.is_finite(),
        "a drawable scale, {scale}"
    );
    scale.log2().ceil().exp2()
}

impl Group {
    /// Whether the group must be rebuilt to draw world box `near` under `key`: built for other
    /// inputs, or the view left its cover.
    pub(crate) fn stale(&self, key: Key, near: [i32; 4]) -> bool {
        let c = &self.cover;
        self.key != key || near[0] < c[0] || near[1] < c[1] || near[2] > c[2] || near[3] > c[3]
    }

    /// Lays out the live labels of `t` (all, or the listed `rows`) big enough to read anywhere
    /// in octave `octave`, for cover box `cover`.
    #[allow(clippy::too_many_arguments)] // the inputs a layout depends on, each used once
    pub(crate) fn build(
        &mut self,
        ctx: &egui::Context,
        key: Key,
        cover: [i32; 4],
        octave: f64,
        t: &Texts,
        rows: Option<&[u32]>,
        palette: &Palette,
    ) {
        assert_eq!(t.x.len(), t.end.len(), "text columns in step");
        assert!(octave > 0.0, "an octave is positive, {octave}");
        self.key = key;
        self.cover = cover;
        self.octave = octave;
        self.anchor = (
            (f64::from(cover[0]) + f64::from(cover[2])) / 2.0,
            (f64::from(cover[1]) + f64::from(cover[3])) / 2.0,
        );
        let mut built: Vec<(i32, Shape)> = Vec::new();
        let all = rows.is_none().then_some(0..t.x.len()).into_iter().flatten();
        let listed = all.chain(rows.into_iter().flatten().map(|&i| i as usize));
        for i in listed {
            let layer = usize::from(t.layer[i]);
            let px = f64::from(t.size[i]) * octave;
            if layer >= LAYERS || px < TEXT_MIN_PX {
                continue; // dead, or under the cut even at the octave's top
            }
            let text = &t.text[schemify_canvas::chain(&t.end, i)];
            let base = px.clamp(OCTAVES.0, OCTAVES.1);
            let [r, g, b, a] = palette.color[layer].to_be_bytes();
            let color = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
            let font = FontId::proportional(base as f32);
            let galley = ctx.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, color));
            let rot = t.rot.get(i).copied().unwrap_or(0);
            let angle = f32::from(rot) * std::f32::consts::FRAC_PI_2;
            let at = egui::vec2(
                ((f64::from(t.x[i]) - self.anchor.0) * octave) as f32,
                ((f64::from(t.y[i]) - self.anchor.1) * octave) as f32,
            );
            let mut shape =
                Shape::Text(TextShape::new(egui::Pos2::ZERO, galley, color).with_angle(angle));
            // Scales (copies) the layout only for a label past the clamp.
            if base == px {
                shape.translate(at);
            } else {
                shape.transform(TSTransform::new(at, (px / base) as f32));
            }
            built.push((t.size[i], shape));
        }
        built.sort_by_key(|b| std::cmp::Reverse(b.0));
        (self.sizes, self.shapes) = built.into_iter().unzip();
        self.mesh_n = usize::MAX;
    }

    /// The group's labels big enough to read at `cam` as one mesh (tessellated again only
    /// when that set changed) and the transform from octave space to the screen.
    pub(crate) fn draw(&mut self, ctx: &egui::Context, view: Rect, cam: &Camera) -> LabelMesh {
        assert!(cam.scale > 0.0, "a drawable camera, scale {}", cam.scale);
        assert!(self.octave >= cam.scale, "the octave holds the scale");
        let cut = TEXT_MIN_PX / cam.scale;
        let n = self.sizes.partition_point(|&s| f64::from(s) >= cut);
        if n != self.mesh_n {
            let ppp = ctx.pixels_per_point();
            let options = ctx.tessellation_options(|o| *o);
            self.atlas = ctx.fonts(|f| f.font_image_size());
            let mut t = Tessellator::new(ppp, options, self.atlas, Vec::new());
            t.set_clip_rect(egui::Rect::EVERYTHING);
            let mut mesh = Mesh::default();
            t.tessellate_shape(Shape::Vec(self.shapes[..n].to_vec()), &mut mesh);
            debug_assert_eq!(mesh.texture_id, egui::TextureId::default(), "glyphs only");
            (self.mesh, self.mesh_n) = (Arc::new(mesh), n);
        }
        let (ox, oy) = cam.origin(view);
        let to_screen = TSTransform::new(
            egui::vec2(
                (self.anchor.0 * cam.scale + ox) as f32,
                (self.anchor.1 * cam.scale + oy) as f32,
            ),
            (cam.scale / self.octave) as f32,
        );
        LabelMesh {
            mesh: Arc::clone(&self.mesh),
            to_screen,
            atlas: self.atlas,
        }
    }
}
