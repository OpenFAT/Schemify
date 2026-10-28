//! `canvas`: a [`CanvasPicture`] → the painter's canvas rows and label texts (architecture.md,
//! frame spine step 12; D39, `docs/dev/perf.md`). Between `Painter::canvas_begin` and
//! `canvas_end`: the four canvas tables (scene as shown, decor, violation markers, parity
//! extra) through `Painter::canvas_rows`, each patched by its change since the last frame
//! and followed by its texts; the selection marks; then the gesture overlay, repacked whole.
//!
//! A table's change comes from its producer when it reports one (the scene's and the
//! markers' killed rows, O(change)), else from [`CanvasDiff`]'s edit script of the hashed
//! table. The rows bake the palette and grid (the style), so a style change repacks all.
//! Labels are looked up near the view through a [`PrimIndex`] per large table and skipped
//! whole when even the table's largest is under [`draw::TEXT_MIN_PX`].
//!
//! Contents:
//! - `pub(crate) struct CanvasCache`, `CanvasCache::paint<P: Painter>(&mut self, p: &mut P,
//!   cp: &CanvasPicture<'_>, text: &mut TextSystem) -> bool`

use std::hash::{Hash, Hasher};

use schemify_canvas::kurbo::Rect as KRect;
use schemify_canvas::{Camera, CanvasDiff, CanvasPrims, PrimIndex, Rows, TableChange};
use schemify_editor::editor::CanvasPicture;
use schemify_render::Painter;
use schemify_render::canvas::{self as draw, CanvasScratch};
use schemify_render::gpu::{Change, Frame, OVERLAY};
use schemify_ui::TextSystem;

/// What paint keeps of the canvas between frames; rebuilt from the tables if dropped.
#[derive(Default)]
pub(crate) struct CanvasCache {
    /// Label glyphs and path buffers of every canvas table.
    scratch: CanvasScratch,
    /// The diff's snapshot and what it was taken at.
    diff: Diffed,
    /// Per table in drawing order: its label index and largest label.
    tables: [TableCache; TABLES],
    /// The rows near the view of the table whose texts are being drawn.
    near: Rows,
}

/// Canvas tables in drawing order: scene as shown, decor, violation markers, parity extra.
const TABLES: usize = 4;
/// The `CanvasPicture::gens` entry of each table in drawing order; entry 3 is the
/// selection marks, which recolor scene rows.
const TABLE_GEN: [usize; TABLES] = [0, 1, 4, 2];
/// `CanvasPicture::gens` entry of the selection marks.
const MARKS_GEN: usize = 3;
/// Tables smaller than this draw every label: culling them costs less than an index.
const INDEX_ROWS_MIN: usize = 4096;
/// Logical px a label may reach beyond its row's box in the index (its glyphs past the
/// anchor, antialiasing): the rows near the view are looked up this much wider.
const NEAR_PAD_PX: f64 = 8.0;

// Every table and the marks read their own generation.
const _: () = {
    let mut k = 0;
    while k < TABLES {
        assert!(TABLE_GEN[k] < 5);
        assert!(TABLE_GEN[k] != MARKS_GEN);
        k += 1;
    }
};

/// The canvas diff and the inputs it last read.
#[derive(Default)]
struct Diffed {
    diff: CanvasDiff,
    /// `CanvasPicture::gens` the diff was taken at.
    gens: [u64; 5],
    /// The style key ([`style_key`]) the diff was taken at.
    style: u64,
    /// Whether the diff last read the scene's own table (not `shown`): only then do the
    /// scene's reported edits describe what changed since.
    read_scene: bool,
}

/// One canvas table's label caches. Generations are stored plus 1, so 0 is never built.
#[derive(Default)]
struct TableCache {
    /// The generation `index` is current for, plus 1.
    index_gen: u64,
    index: PrimIndex,
    /// The generation `label_max` was taken at, plus 1.
    label_gen: u64,
    /// The table's largest label size (milli-units).
    label_max: i32,
}

impl CanvasCache {
    /// Paints `cp` between `Painter::canvas_begin` and `canvas_end`. Returns whether the
    /// diff took the scene's and the markers' reported edits (`Editor::painted`): true when
    /// the generations or the style moved, also for a painter that shows no canvas.
    pub(crate) fn paint<P: Painter>(
        &mut self,
        p: &mut P,
        cp: &CanvasPicture<'_>,
        text: &mut TextSystem,
    ) -> bool {
        assert!(cp.cam.scale > 0.0, "a camera scale is positive");
        let style = style_key(cp);
        let shown = cp.shown.unwrap_or(cp.scene);
        let diffed = self.diff.gens != cp.gens || self.diff.style != style;
        let (changes, marks_changed) = match diffed {
            true => self.diff.take(cp, shown, style, &mut self.tables),
            false => ([Change::Same; TABLES], false),
        };
        let frame = Frame {
            view: cp.view,
            cam: cp.cam,
            grid: cp.grid,
            background: cp.palette.background,
            grid_color: cp.palette.grid,
        };
        if !p.canvas_begin(&frame) {
            return diffed; // a painter that shows no canvas
        }
        let (view, cam, palette) = (cp.view, &cp.cam, &cp.palette);
        let near = world_box(view, cam);
        let tables = [shown, cp.decor, cp.markers, cp.extra];
        for (k, (t, change)) in tables.into_iter().zip(changes).enumerate() {
            p.canvas_rows(k, t, palette, change);
            // A table whose largest label is under the text cut at this zoom draws none (the
            // far zoom's frames look at no label); else only the rows near the view.
            let cache = &mut self.tables[k];
            let g = cp.gens[TABLE_GEN[k]];
            if cache.label_gen != g + 1 {
                cache.label_gen = g + 1;
                cache.label_max = t.texts.size.iter().copied().max().unwrap_or(0);
            }
            if f64::from(cache.label_max) * cam.scale < draw::TEXT_MIN_PX {
                continue;
            }
            let listed = cache.rows_near(t, g, near, &mut self.near);
            let listed = listed.then_some(&self.near.texts[..]);
            let scratch = &mut self.scratch;
            draw::draw_texts(p, view, cam, &t.texts, listed, palette, text, scratch);
        }
        p.canvas_marks(cp.scene, cp.marks, marks_changed);
        if !cp.overlay.is_empty() {
            let (o, scratch) = (cp.overlay, &mut self.scratch);
            p.canvas_rows(OVERLAY, o, palette, Change::All);
            draw::draw_texts(p, view, cam, &o.texts, None, palette, text, scratch);
        }
        p.canvas_end();
        assert_eq!(self.diff.gens, cp.gens, "the diff is current");
        diffed
    }
}

// ---- private ----

/// The palette as the View/Layers options shape it (hidden layers, dimming, colors, widths)
/// and the grid: the rows bake them, so a change of this key repacks every row.
fn style_key(cp: &CanvasPicture<'_>) -> u64 {
    let palette = &cp.palette;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let colors = (
        palette.color,
        palette.fill,
        palette.background,
        palette.grid,
    );
    (cp.grid, colors.0, colors.1, colors.2, colors.3).hash(&mut h);
    palette.width.map(f32::to_bits).hash(&mut h);
    h.finish()
}

/// The world box `view` shows through `cam`, widened by [`NEAR_PAD_PX`].
fn world_box(view: KRect, cam: &Camera) -> [i32; 4] {
    let (a0, b0) = cam.to_world(view, view.x0, view.y0);
    let (a1, b1) = cam.to_world(view, view.x1, view.y1);
    let pad = NEAR_PAD_PX / cam.scale;
    let c = |v: f64| v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let b = [
        c((a0.min(a1) - pad).floor()),
        c((b0.min(b1) - pad).floor()),
        c((a0.max(a1) + pad).ceil()),
        c((b0.max(b1) + pad).ceil()),
    ];
    assert!(b[0] <= b[2]);
    assert!(b[1] <= b[3]);
    b
}

impl Diffed {
    /// Takes the tables' changes since the generations and style the diff last read: per
    /// table in drawing order what its rows rewrite, and whether the marks changed. Indexes
    /// current before the change follow it; the rest rebuild when their texts next need it.
    fn take<'a>(
        &'a mut self,
        cp: &CanvasPicture<'a>,
        shown: &'a CanvasPrims,
        style: u64,
        caches: &mut [TableCache; TABLES],
    ) -> ([Change<'a>; TABLES], bool) {
        assert!(
            self.gens != cp.gens || self.style != style,
            "called once the inputs moved"
        );
        let changed = |g: usize| self.gens[g] != cp.gens[g];
        // The scene reports its own edits (O(change)) when the diff last read it; the
        // markers always do; decor and the parity extra are hashed when they changed.
        let scene_edits = (cp.shown.is_none() && self.read_scene)
            .then_some(cp.scene_edits)
            .flatten();
        let reports = [scene_edits, None, cp.markers_edits, None];
        let prims = [shown, cp.decor, cp.markers, cp.extra];
        let tables: [_; TABLES] = std::array::from_fn(|k| {
            let change = match (changed(TABLE_GEN[k]), reports[k]) {
                (true, Some(killed)) => TableChange::Rows(killed),
                (true, None) => TableChange::Any,
                (false, _) => TableChange::Same,
            };
            (prims[k], change)
        });
        let marks_changed = changed(TABLE_GEN[0]) || changed(MARKS_GEN);
        let known = self
            .diff
            .update(&tables, (cp.scene, cp.marks, marks_changed));
        for (k, cache) in caches.iter_mut().enumerate() {
            let g = TABLE_GEN[k];
            if known && changed(g) && cache.index_gen == self.gens[g] + 1 {
                let follows = match tables[k].1 {
                    TableChange::Rows(_) => cache.index.follow(tables[k].0),
                    _ => cache.index.apply(self.diff.edits_of(k)),
                };
                if follows {
                    cache.index_gen = cp.gens[g] + 1;
                }
            }
        }
        // Each table's own change: a producer's report is exact whatever the diff made of
        // the other tables; a hashed table's edit script only when the diff could follow.
        let restyled = self.style != style;
        (self.gens, self.style, self.read_scene) = (cp.gens, style, cp.shown.is_none());
        let changes = std::array::from_fn(|k| match tables[k].1 {
            _ if restyled => Change::All,
            TableChange::Same => Change::Same,
            TableChange::Rows(killed) => Change::Killed(killed),
            TableChange::Any if known => Change::Edits(self.diff.edits_of(k)),
            TableChange::Any => Change::All,
        });
        (changes, restyled || marks_changed)
    }
}

impl TableCache {
    /// Lists in `out` the rows of `t` near world box `near`, from the index (rebuilt for
    /// generation `g` when stale). Returns false (draw every row) for a small table.
    fn rows_near(&mut self, t: &CanvasPrims, g: u64, near: [i32; 4], out: &mut Rows) -> bool {
        if t.len() < INDEX_ROWS_MIN {
            return false;
        }
        if self.index_gen != g + 1 {
            (self.index_gen, self.index) = (g + 1, PrimIndex::build(t));
        }
        self.index.query(t, near, out);
        let rows = t.texts.size.len();
        assert!(
            out.texts.last().is_none_or(|&r| (r as usize) < rows),
            "rows of the table"
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: the camera's definition (view center = (cx, cy), `scale` px per milli-unit),
    /// so the box is the view's half extents around the center plus the pad in world units.
    #[test]
    fn world_box_is_the_view_around_the_center_widened_by_the_pad() {
        let view = KRect::new(0.0, 0.0, 100.0, 50.0);
        let cam = Camera {
            cx: 1000.0,
            cy: -200.0,
            scale: 2.0,
        };
        // Half extents 25 x 12.5 world units, pad 8 / 2 = 4.
        assert_eq!(world_box(view, &cam), [971, -217, 1029, -183]);
        let far = Camera { scale: 1e-9, ..cam };
        let b = world_box(view, &far);
        assert!(
            b[0] < -i32::MAX / 2,
            "a far zoom clamps, never wraps: {b:?}"
        );
        assert!(b[2] > i32::MAX / 2, "{b:?}");
    }
}
