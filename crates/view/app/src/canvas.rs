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
//!   picture: &CanvasPicture<'_>, text: &mut TextSystem) -> bool`

use std::hash::DefaultHasher;

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
/// Entries of `CanvasPicture::gens`: the four tables and the selection marks.
const GENS: usize = 5;
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

const _: () = assert!(MARKS_GEN < GENS);
const _: () = assert!(NEAR_PAD_PX >= 0.0);

// Every table and the marks read their own generation.
const _: () = {
    let mut k = 0;
    while k < TABLES {
        assert!(TABLE_GEN[k] < GENS);
        assert!(TABLE_GEN[k] != MARKS_GEN);
        k += 1;
    }
};

/// The canvas diff and the inputs it last read.
#[derive(Default)]
struct Diffed {
    diff: CanvasDiff,
    /// `CanvasPicture::gens` the diff was taken at.
    gens: [u64; GENS],
    /// The style key ([`style_key`]) the diff was taken at.
    style: u64,
    /// Whether the diff last read the scene's own table (not `shown`): only then do the
    /// scene's reported edits describe what changed since.
    read_scene: bool,
}

/// What [`Diffed::take`] found changed since the diff last read its inputs.
struct Taken<'picture> {
    /// Per table in drawing order: what its rows rewrite.
    changes: [Change<'picture>; TABLES],
    /// Whether the selection marks must be repainted.
    marks_changed: bool,
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
    /// Paints `picture` between `Painter::canvas_begin` and `canvas_end`. Returns whether
    /// the diff took the scene's and the markers' reported edits (`Editor::painted`): true
    /// when the generations or the style moved, also for a painter that shows no canvas.
    pub(crate) fn paint<P: Painter>(
        &mut self,
        painter: &mut P,
        picture: &CanvasPicture<'_>,
        text: &mut TextSystem,
    ) -> bool {
        assert!(picture.cam.scale > 0.0, "a camera scale is positive");
        assert!(picture.cam.scale.is_finite(), "a camera scale is finite");
        let style = style_key(picture);
        let shown = picture.shown.unwrap_or(picture.scene);
        let diffed = self.diff.gens != picture.gens || self.diff.style != style;
        let Taken {
            changes,
            marks_changed,
        } = if diffed {
            self.diff.take(picture, shown, style, &mut self.tables)
        } else {
            Taken {
                changes: [Change::Same; TABLES],
                marks_changed: false,
            }
        };
        let frame = Frame {
            view: picture.view,
            cam: picture.cam,
            grid: picture.grid,
            background: picture.palette.background,
            grid_color: picture.palette.grid,
        };
        if !painter.canvas_begin(&frame) {
            assert_eq!(self.diff.gens, picture.gens, "the diff is current");
            return diffed; // a painter that shows no canvas
        }
        let (view, cam, palette) = (picture.view, &picture.cam, &picture.palette);
        let near = world_box(view, cam);
        let tables = [shown, picture.decor, picture.markers, picture.extra];
        let tables_changes = tables.into_iter().zip(changes).enumerate();
        for (k, (table, change)) in tables_changes {
            painter.canvas_rows(k, table, palette, change);
            // A table whose largest label is under the text cut at this zoom draws none (the
            // far zoom's frames look at no label); else only the rows near the view.
            let cache = &mut self.tables[k];
            let generation = picture.gens[TABLE_GEN[k]];
            if cache.label_gen != generation + 1 {
                cache.label_gen = generation + 1;
                cache.label_max = table.texts.size.iter().copied().max().unwrap_or(0);
            }
            if f64::from(cache.label_max) * cam.scale < draw::TEXT_MIN_PX {
                continue;
            }
            let listed = cache.rows_near(table, generation, near, &mut self.near);
            let listed = listed.then_some(&self.near.texts[..]);
            let scratch = &mut self.scratch;
            draw::draw_texts(
                painter,
                view,
                cam,
                &table.texts,
                listed,
                palette,
                text,
                scratch,
            );
        }
        painter.canvas_marks(picture.scene, picture.marks, marks_changed);
        if !picture.overlay.is_empty() {
            let (overlay, scratch) = (picture.overlay, &mut self.scratch);
            painter.canvas_rows(OVERLAY, overlay, palette, Change::All);
            draw::draw_texts(
                painter,
                view,
                cam,
                &overlay.texts,
                None,
                palette,
                text,
                scratch,
            );
        }
        painter.canvas_end();
        assert_eq!(self.diff.gens, picture.gens, "the diff is current");
        assert_eq!(self.diff.style, style, "the diff is current");
        diffed
    }
}

// ---- private ----

/// The palette as the View/Layers options shape it (hidden layers, dimming, colors, widths)
/// and the grid: the rows bake them, so a change of this key repacks every row.
fn style_key(picture: &CanvasPicture<'_>) -> u64 {
    use std::hash::{Hash, Hasher};

    let palette = &picture.palette;
    let mut hasher = DefaultHasher::new();
    let style = (
        picture.grid,
        palette.color,
        palette.fill,
        palette.background,
        palette.grid,
    );
    style.hash(&mut hasher);
    palette.width.map(f32::to_bits).hash(&mut hasher);
    hasher.finish()
}

/// The world box `view` shows through `cam`, widened by [`NEAR_PAD_PX`].
fn world_box(view: KRect, cam: &Camera) -> [i32; 4] {
    assert!(cam.scale > 0.0, "a camera scale is positive");
    let (x0, y0) = cam.to_world(view, view.x0, view.y0);
    let (x1, y1) = cam.to_world(view, view.x1, view.y1);
    let pad = NEAR_PAD_PX / cam.scale;
    // Saturating: a far zoom's box clamps to the i32 world, never wraps.
    let to_i32 = |v: f64| v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let bounds = [
        to_i32((x0.min(x1) - pad).floor()),
        to_i32((y0.min(y1) - pad).floor()),
        to_i32((x0.max(x1) + pad).ceil()),
        to_i32((y0.max(y1) + pad).ceil()),
    ];
    assert!(bounds[0] <= bounds[2], "a box is ordered: {bounds:?}");
    assert!(bounds[1] <= bounds[3], "a box is ordered: {bounds:?}");
    bounds
}

impl Diffed {
    /// Takes the tables' changes since the generations and style the diff last read: per
    /// table in drawing order what its rows rewrite, and whether the marks changed. Indexes
    /// current before the change follow it; the rest rebuild when their texts next need it.
    fn take<'picture>(
        &'picture mut self,
        picture: &CanvasPicture<'picture>,
        shown: &'picture CanvasPrims,
        style: u64,
        caches: &mut [TableCache; TABLES],
    ) -> Taken<'picture> {
        assert!(
            self.gens != picture.gens || self.style != style,
            "called once the inputs moved"
        );
        let changed = |generation: usize| self.gens[generation] != picture.gens[generation];
        // The scene reports its own edits (O(change)) when the diff last read it; the
        // markers always do; decor and the parity extra are hashed when they changed.
        let scene_edits = (picture.shown.is_none() && self.read_scene)
            .then_some(picture.scene_edits)
            .flatten();
        let reports = [scene_edits, None, picture.markers_edits, None];
        let prims = [shown, picture.decor, picture.markers, picture.extra];
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
            .update(&tables, (picture.scene, picture.marks, marks_changed));
        for (k, cache) in caches.iter_mut().enumerate() {
            let generation = TABLE_GEN[k];
            let current = cache.index_gen == self.gens[generation] + 1;
            if known && changed(generation) && current {
                let (prims, change) = tables[k];
                let follows = match change {
                    TableChange::Rows(_) => cache.index.follow(prims),
                    TableChange::Any | TableChange::Same => {
                        cache.index.apply(self.diff.edits_of(k))
                    }
                };
                if follows {
                    cache.index_gen = picture.gens[generation] + 1;
                }
            }
        }
        // Each table's own change: a producer's report is exact whatever the diff made of
        // the other tables; a hashed table's edit script only when the diff could follow.
        let restyled = self.style != style;
        (self.gens, self.style, self.read_scene) = (picture.gens, style, picture.shown.is_none());
        let changes = std::array::from_fn(|k| match tables[k].1 {
            _ if restyled => Change::All,
            TableChange::Same => Change::Same,
            TableChange::Rows(killed) => Change::Killed(killed),
            TableChange::Any if known => Change::Edits(self.diff.edits_of(k)),
            TableChange::Any => Change::All,
        });
        Taken {
            changes,
            marks_changed: restyled || marks_changed,
        }
    }
}

impl TableCache {
    /// Lists in `out` the rows of `t` near world box `near`, from the index (rebuilt for
    /// generation `g` when stale). Returns false (draw every row) for a small table.
    fn rows_near(
        &mut self,
        table: &CanvasPrims,
        generation: u64,
        near: [i32; 4],
        out: &mut Rows,
    ) -> bool {
        assert!(near[0] <= near[2], "a box is ordered: {near:?}");
        assert!(near[1] <= near[3], "a box is ordered: {near:?}");
        if table.len() < INDEX_ROWS_MIN {
            return false;
        }
        if self.index_gen != generation + 1 {
            (self.index_gen, self.index) = (generation + 1, PrimIndex::build(table));
        }
        assert_eq!(self.index_gen, generation + 1, "the index is current");
        self.index.query(table, near, out);
        let rows = table.texts.size.len();
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
