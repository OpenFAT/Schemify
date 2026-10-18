//! Oracle for the canvas raster cache (`canvas::draw_cached`, D39): on the CPU path, a frame
//! served from the cache equals a direct render of the same camera (within the golden
//! tolerance), for a seeded random walk of pans and zooms; and the cache serves exactly the
//! frames it may (whole-pixel pans inside the margin) and re-records the rest.

use schemify_render::canvas::{
    self, Camera, CanvasCache, CanvasPrims, CanvasScratch, Drawn, Palette,
};
use schemify_render::vello::kurbo::Rect;
use schemify_render::vello::peniko::Color;
use schemify_render::{CpuPainter, Painter, Pixmap, rgba};
use schemify_ui::TextSystem;

mod common;

struct Rng(u32);

impl Rng {
    fn below(&mut self, n: u32) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 % n
    }
}

/// Background, grid and primitives: what the editor records into its layer.
fn content<P: Painter>(
    p: &mut P,
    view: Rect,
    cam: &Camera,
    prims: &CanvasPrims,
    text: &mut TextSystem,
    scratch: &mut CanvasScratch,
) -> usize {
    let palette = Palette::xschem_dark();
    p.fill_rect(view, rgba(palette.background));
    canvas::draw_grid(p, view, cam, 10_000, &palette, scratch);
    canvas::draw_prims(p, view, cam, prims, &palette, text, scratch)
}

/// The canvas background `content` fills first.
fn bg() -> Color {
    rgba(Palette::xschem_dark().background)
}

/// Pixels whose channels differ by more than 2 (the golden tolerance).
fn differing(a: &Pixmap, b: &Pixmap) -> usize {
    a.data()
        .iter()
        .zip(b.data())
        .filter(|(a, b)| {
            let (a, b) = ([a.r, a.g, a.b, a.a], [b.r, b.g, b.b, b.a]);
            a.iter().zip(b).any(|(x, y)| x.abs_diff(y) > 2)
        })
        .count()
}

fn walk(scale: f64, seed: u32) {
    let prims = common::thousand_prims();
    let (w, h) = (480u16, 320u16);
    let view = Rect::new(13.0, 21.0, 460.0, 300.0);
    let mut cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    cam.fit(view, [0, 0, 600_000, 380_000]);
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    let mut cached = CpuPainter::new(
        (f64::from(w) * scale) as u16,
        (f64::from(h) * scale) as u16,
        scale,
    );
    let mut cache = CanvasCache::default();
    cache.tiles_per_frame = 4; // small content costs little: cap the tiles to see partial frames
    let mut rng = Rng(seed);
    let mut key = 1u64;
    let (mut copied, mut partial) = (0, 0);
    for step in 0..60 {
        // One random camera move (or a content change), then frames until the view is whole.
        let expect = match rng.below(8) {
            0..=3 => {
                let (dx, dy) = (
                    f64::from(rng.below(81)) - 40.0,
                    f64::from(rng.below(61)) - 30.0,
                );
                cam.pan(dx, dy);
                None // a whole-pixel pan: copied, plus the tiles it uncovers
            }
            4 => {
                cam.pan(0.5, 0.25);
                None // off the pixel grid: every tile is recorded again, a few per frame
            }
            5 => {
                let f = [0.5, 0.8, 1.25, 2.0][rng.below(4) as usize];
                let (sx, sy) = (
                    view.x0 + f64::from(rng.below(view.width() as u32)),
                    view.y0 + f64::from(rng.below(view.height() as u32)),
                );
                cam.zoom_at(view, sx, sy, f);
                None // a crisp frame at a new zoom records, a few tiles per frame
            }
            6 => {
                key += 1;
                Some(Drawn::Patched) // the content changed: the visible tiles at once
            }
            _ => Some(Drawn::Copied), // nothing moved (the ring may still fill in)
        };
        let (mut frames, mut drawn) = (0, Drawn::Partial);
        let mut got = None;
        while drawn == Drawn::Partial && frames < 128 {
            cached.reset();
            drawn = canvas::draw_cached(
                &mut cached,
                &mut cache,
                view,
                &cam,
                key,
                None, // where the content changed is not known: record
                true,
                bg(),
                |p, v, c| content(p, v, c, &prims, &mut text, &mut scratch),
            );
            if frames == 0
                && let Some(e) = expect
            {
                assert_eq!(drawn, e, "step {step}");
            }
            frames += 1;
            partial += usize::from(drawn == Drawn::Partial);
            got = Some(cached.finish(Color::BLACK));
        }
        assert_ne!(drawn, Drawn::Partial, "step {step}: the view completes");
        assert_ne!(drawn, Drawn::Direct, "the CPU painter keeps layers");
        copied += usize::from(drawn == Drawn::Copied);
        let got = got.expect("drawn");

        let mut direct = CpuPainter::new(got.width(), got.height(), scale);
        direct.push_clip(view);
        content(&mut direct, view, &cam, &prims, &mut text, &mut scratch);
        direct.pop_clip();
        let want = direct.finish(Color::BLACK);
        let bad = differing(&got, &want);
        assert!(
            bad <= want.data().len() / 1000,
            "step {step} ({drawn:?}, scale {scale}): {bad} pixels differ from a direct render"
        );
    }
    assert!(
        copied >= 10 && partial >= 3,
        "{copied} copied, {partial} partial frames"
    );
}

#[test]
fn cached_frames_equal_direct_renders() {
    walk(1.0, 0x9e37_79b9);
    walk(1.0, 0x2545_f491);
}

#[test]
fn cached_frames_equal_direct_renders_at_scale_two() {
    walk(2.0, 0x1234_5678);
}

/// The ring ahead of the view is recorded on frames something else asked for (an input frame
/// that only hovers), so it takes a quarter of the frame's budget at most: once the view is
/// whole, each further frame records within that share, and the ring is whole after a bounded
/// number of them (after which a frame records nothing).
#[test]
fn the_ring_takes_a_share_of_the_budget() {
    let prims = common::thousand_prims();
    let view = Rect::new(0.0, 0.0, 400.0, 300.0);
    let cam = Camera {
        cx: 300_000.0,
        cy: 190_000.0,
        scale: 0.004,
    };
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    // The frame's recording cost, with `cost_per_frame` = `budget`.
    let mut run = |budget: usize, frames: usize| -> Vec<usize> {
        let mut p = CpuPainter::new(400, 300, 1.0);
        let mut cache = CanvasCache::default();
        (cache.tiles_per_frame, cache.cost_per_frame) = (usize::MAX, budget);
        let mut costs = Vec::new();
        for _ in 0..frames {
            p.reset();
            let mut cost = 0;
            canvas::draw_cached(
                &mut p,
                &mut cache,
                view,
                &cam,
                7,
                Some(&[]),
                true,
                bg(),
                |p, v, c| {
                    let k = content(p, v, c, &prims, &mut text, &mut scratch);
                    cost += k;
                    k
                },
            );
            costs.push(cost);
        }
        costs
    };
    // An unbounded budget: the view on the first frame (a fresh atlas), the whole ring on the
    // second.
    let free = run(usize::MAX, 3);
    let ring = free[1];
    assert!(ring > 0 && free[2] == 0, "the ring, then nothing: {free:?}");
    // A budget near the whole ring: without the share, the next frame records most of it.
    let budget = ring - ring / 8;
    let costs = run(budget, 64);
    let after: Vec<usize> = costs[1..].to_vec();
    assert!(
        after.iter().all(|&c| c <= budget / 4),
        "every ring frame stays within the ring's share (a quarter) of {budget}: {after:?}"
    );
    let busy = after.iter().take_while(|&&c| c > 0).count();
    assert!(busy >= 2, "the ring is spread over frames: {after:?}");
    assert!(
        after[busy..].iter().all(|&c| c == 0),
        "then it is whole: {after:?}"
    );
}

/// A zoom while not crisp stretches the recorded tiles (stand-ins) and never records, even once
/// a zoom-out uncovers the view (background shows there); crisp frames then record the new
/// zoom's tiles, a few per frame, until the view is whole.
#[test]
fn zoom_stretches_until_crisp() {
    let prims = common::thousand_prims();
    let view = Rect::new(0.0, 0.0, 400.0, 300.0);
    let mut cam = Camera {
        cx: 300_000.0,
        cy: 190_000.0,
        scale: 0.002,
    };
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    let mut p = CpuPainter::new(400, 300, 1.0);
    let mut cache = CanvasCache::default();
    cache.tiles_per_frame = 1;
    let mut frame = |p: &mut CpuPainter, cam: &Camera, crisp: bool| {
        p.reset();
        let drawn = canvas::draw_cached(
            p,
            &mut cache,
            view,
            cam,
            7,
            Some(&[]),
            crisp,
            bg(),
            |p, v, c| content(p, v, c, &prims, &mut text, &mut scratch),
        );
        (drawn, p.finish(Color::WHITE))
    };
    assert_eq!(frame(&mut p, &cam, false).0, Drawn::Recorded);
    cam.zoom_at(view, 200.0, 150.0, 1.2);
    assert_eq!(frame(&mut p, &cam, false).0, Drawn::Stretched, "zoom in");
    cam.zoom_at(view, 200.0, 150.0, 1.0 / 1.2 / 1.2);
    assert_eq!(
        frame(&mut p, &cam, false).0,
        Drawn::Stretched,
        "zoom out within the margin"
    );
    cam.zoom_at(view, 200.0, 150.0, 1.0 / 4.0);
    let (drawn, px) = frame(&mut p, &cam, false);
    assert_eq!(drawn, Drawn::Stretched, "zoom out past the margin");
    let corner = px.data()[0];
    let want = bg().premultiply().to_rgba8();
    assert_eq!(
        [corner.r, corner.g, corner.b, corner.a],
        [want.r, want.g, want.b, want.a],
        "the uncovered corner shows the background, not the white under it"
    );
    cam.zoom_at(view, 200.0, 150.0, 1.2);
    let mut drawn = frame(&mut p, &cam, true).0;
    assert_eq!(
        drawn,
        Drawn::Partial,
        "crisp records, a few tiles per frame"
    );
    for _ in 0..16 {
        if drawn != Drawn::Partial {
            break;
        }
        drawn = frame(&mut p, &cam, true).0;
    }
    assert_eq!(drawn, Drawn::Recorded, "the last tiles complete the view");
    assert_eq!(frame(&mut p, &cam, true).0, Drawn::Copied);
}

/// A painter that never recorded the layer (a fresh one) cannot be served from the cache, nor
/// patched (new content with known, even empty, damage): it records instead of drawing
/// nothing.
#[test]
fn a_fresh_painter_records() {
    let prims = common::thousand_prims();
    let view = Rect::new(0.0, 0.0, 200.0, 100.0);
    let cam = Camera {
        cx: 300_000.0,
        cy: 190_000.0,
        scale: 0.002,
    };
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    let mut cache = CanvasCache::default();
    for key in [1, 1, 2] {
        let mut p = CpuPainter::new(200, 100, 1.0);
        let drawn = canvas::draw_cached(
            &mut p,
            &mut cache,
            view,
            &cam,
            key,
            Some(&[]),
            true,
            bg(),
            |p, v, c| content(p, v, c, &prims, &mut text, &mut scratch),
        );
        assert_eq!(drawn, Drawn::Recorded);
    }
}

/// One edit the way the editor's tables change: a row swap-removed, a row pushed, a label
/// moved, or a different row selected. Returns whether `prims` changed.
fn edit(prims: &mut CanvasPrims, sel: &mut Vec<u32>, rng: &mut Rng) -> bool {
    let l = &mut prims.lines;
    match rng.below(5) {
        0 if l.x0.len() > 10 => {
            let i = rng.below(l.x0.len() as u32) as usize;
            for col in [&mut l.x0, &mut l.y0, &mut l.x1, &mut l.y1] {
                col.swap_remove(i);
            }
            l.layer.swap_remove(i);
            l.style.swap_remove(i);
            sel.retain(|&s| (s as usize) < l.x0.len());
        }
        1 => {
            let (x, y) = (rng.below(600) as i32 * 1000, rng.below(380) as i32 * 1000);
            prims.line(
                x,
                y,
                x + 30_000,
                y + 7_000,
                [1, 4, 5][rng.below(3) as usize],
            );
        }
        2 => {
            let t = &mut prims.texts;
            let i = rng.below(t.x.len() as u32) as usize;
            t.x[i] += 5_000;
            t.y[i] -= 3_000;
        }
        3 => {
            let r = &mut prims.rects;
            let i = rng.below(r.x0.len() as u32) as usize;
            r.x1[i] += 4_000;
        }
        _ => {
            *sel = vec![rng.below(prims.lines.x0.len() as u32)];
            return false;
        }
    }
    true
}

/// D39 oracle for edits: tiles patched where [`CanvasDiff`] says the tables changed equal the
/// same frame recorded afresh within one level per channel, pans included (tiles are anchored
/// to the world, so a pan by whole pixels keeps their pixel grid).
fn patch_walk(scale: f64, seed: u32) {
    use schemify_render::canvas::{CanvasDiff, Marks, PrimKind};
    let mut prims = common::thousand_prims();
    let (w, h) = (480u16, 320u16);
    let view = Rect::new(13.0, 21.0, 460.0, 300.0);
    let mut cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    cam.fit(view, [0, 0, 600_000, 380_000]);
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    let size = |v: u16| (f64::from(v) * scale) as u16;
    let mut p = CpuPainter::new(size(w), size(h), scale);
    let (mut cache, mut diff, mut boxes) = (CanvasCache::default(), CanvasDiff::default(), vec![]);
    let (mut rng, mut sel, mut key) = (Rng(seed), vec![3u32, 8], 1u64);
    let mut draws = |p: &mut CpuPainter,
                     cache: &mut CanvasCache,
                     cam: &Camera,
                     key: u64,
                     damage: Option<&[[i32; 4]]>,
                     prims: &CanvasPrims,
                     marks: &Marks| {
        // Frames until the view is whole (a pan may uncover more tiles than one frame records).
        let mut damage = damage;
        for _ in 0..64 {
            p.reset();
            let drawn =
                canvas::draw_cached(p, cache, view, cam, key, damage, true, bg(), |p, v, c| {
                    let cost = content(p, v, c, prims, &mut text, &mut scratch);
                    canvas::draw_marks(p, v, c, prims, marks, &mut scratch);
                    cost
                });
            damage = Some(&[]);
            if drawn != Drawn::Partial {
                return (drawn, p.finish(Color::BLACK));
            }
        }
        panic!("the view never completed");
    };
    let mut patched = 0;
    for step in 0..50 {
        let mut changed = false;
        if rng.below(6) == 0 {
            cam.pan(
                f64::from(rng.below(21)) - 10.0,
                f64::from(rng.below(21)) - 10.0,
            );
        } else {
            changed = edit(&mut prims, &mut sel, &mut rng);
            key += 1;
        }
        let mut marks = Marks::default();
        for &s in &sel {
            marks.push(PrimKind::Line, s, 0xffff_00ff);
        }
        boxes.clear();
        // Unchanged prims are not rehashed: only the marks then (a selection).
        let change = if changed {
            canvas::TableChange::Any
        } else {
            canvas::TableChange::Same
        };
        let known = diff.update(&[(&prims, change)], (&prims, &marks, true), &mut boxes);
        let damage = known.then_some(&boxes[..]);
        let (drawn, got) = draws(&mut p, &mut cache, &cam, key, damage, &prims, &marks);
        patched += usize::from(drawn == Drawn::Patched);
        let mut fresh = CpuPainter::new(size(w), size(h), scale);
        let (again, want) = draws(
            &mut fresh,
            &mut CanvasCache::default(),
            &cam,
            key,
            None,
            &prims,
            &marks,
        );
        assert_eq!(again, Drawn::Recorded);
        // A stale row would differ by its full contrast; a part redrawn alone may round its
        // unsnapped coordinates (arcs, glyphs) differently, by 1.
        let worst = got
            .data()
            .iter()
            .zip(want.data())
            .map(|(a, b)| {
                let (a, b) = ([a.r, a.g, a.b, a.a], [b.r, b.g, b.b, b.a]);
                a.iter()
                    .zip(b)
                    .map(|(x, y)| x.abs_diff(y))
                    .max()
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0);
        let bad = differing(&got, &want);
        assert!(
            worst <= 1 && bad == 0,
            "step {step} ({drawn:?}, scale {scale}): off by {worst}, {bad} pixels by more than 2"
        );
    }
    assert!(patched >= 20, "{patched} frames patched");
}

#[test]
fn patched_layers_equal_recordings() {
    patch_walk(1.0, 0x9e37_79b9);
    patch_walk(1.0, 0x0bad_cafe);
}

#[test]
fn patched_layers_equal_recordings_at_scale_two() {
    patch_walk(2.0, 0x1234_5678);
}

/// `prims` without its dead rows (what a producer that rebuilt its table would hold).
fn compacted(prims: &CanvasPrims) -> CanvasPrims {
    use schemify_render::canvas::LAYER_DEAD;
    let mut out = CanvasPrims::default();
    let l = &prims.lines;
    for i in (0..l.x0.len()).filter(|&i| l.layer[i] != LAYER_DEAD) {
        out.line_styled(l.x0[i], l.y0[i], l.x1[i], l.y1[i], l.layer[i], l.style[i]);
    }
    let r = &prims.rects;
    for i in (0..r.x0.len()).filter(|&i| r.layer[i] != LAYER_DEAD) {
        out.rect_styled(r.x0[i], r.y0[i], r.x1[i], r.y1[i], r.layer[i], r.style[i]);
    }
    let a = &prims.arcs;
    for i in (0..a.cx.len()).filter(|&i| a.layer[i] != LAYER_DEAD) {
        let (s, w) = (a.start[i], a.sweep[i]);
        out.arc_styled(a.cx[i], a.cy[i], a.r[i], s, w, a.layer[i], a.style[i]);
    }
    for (polys, closed) in [(&prims.polylines, false), (&prims.polygons, true)] {
        for k in (0..polys.end.len()).filter(|&k| polys.layer[k] != LAYER_DEAD) {
            let start = if k == 0 { 0 } else { polys.end[k - 1] as usize };
            let pts: Vec<(i32, i32)> = (start..polys.end[k] as usize)
                .map(|j| (polys.x[j], polys.y[j]))
                .collect();
            out.poly_styled(&pts, closed, polys.layer[k], polys.style[k]);
        }
    }
    let t = &prims.texts;
    for i in (0..t.x.len()).filter(|&i| t.layer[i] != LAYER_DEAD) {
        let start = if i == 0 { 0 } else { t.end[i - 1] as usize };
        let s = &t.text[start..t.end[i] as usize];
        out.text_rot(t.x[i], t.y[i], t.size[i], t.layer[i], s, t.rot[i]);
    }
    out
}

/// D39 oracle for producer-reported edits (`TableChange::Rows`, the editor's scene and
/// violation markers): rows killed and rows appended, damage from the report alone, tiles
/// drawn from a `PrimIndex` that follows the edits; every frame equals a fresh recording of
/// the table without its dead rows, so a dead row draws nothing and no change is missed.
fn reported_walk(seed: u32) {
    use schemify_render::canvas::{CanvasDiff, LAYER_DEAD, Marks, PrimIndex, Rows, TableChange};
    let mut prims = common::thousand_prims();
    let (w, h) = (480u16, 320u16);
    let view = Rect::new(13.0, 21.0, 460.0, 300.0);
    let mut cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    cam.fit(view, [0, 0, 600_000, 380_000]);
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    let mut p = CpuPainter::new(w, h, 1.0);
    let (mut cache, mut diff, mut boxes) = (CanvasCache::default(), CanvasDiff::default(), vec![]);
    let (mut rng, mut key) = (Rng(seed), 1u64);
    let mut ix = PrimIndex::build(&prims);
    let mut rows = Rows::default();
    let marks = Marks::default();
    assert!(!diff.update(
        &[(&prims, TableChange::Any)],
        (&prims, &marks, true),
        &mut boxes
    ));
    let (mut patched, mut killed_total) = (0, 0);
    for step in 0..40 {
        // An edit: a few rows killed (any kind), a few appended.
        let mut killed: [Vec<u32>; 6] = Default::default();
        for _ in 0..1 + rng.below(3) {
            let kind = [0u8, 1, 2, 5][rng.below(4) as usize];
            let layers = match kind {
                0 => &prims.lines.layer,
                1 => &prims.rects.layer,
                2 => &prims.arcs.layer,
                _ => &prims.texts.layer,
            };
            let i = rng.below(layers.len() as u32) as usize;
            if layers[i] != LAYER_DEAD {
                prims.kill(kind, i);
                killed[usize::from(kind)].push(i as u32);
                killed_total += 1;
            }
        }
        for _ in 0..rng.below(3) {
            let (x, y) = (rng.below(600) as i32 * 1000, rng.below(380) as i32 * 1000);
            match rng.below(3) {
                0 => prims.line(x, y, x + 30_000, y + 7_000, 1),
                1 => prims.rect(x, y, x + 20_000, y + 9_000, 4),
                _ => prims.text(x, y, 9_000, 3, "new"),
            }
        }
        key += 1;
        boxes.clear();
        let known = diff.update(
            &[(&prims, TableChange::Rows(&killed))],
            (&prims, &marks, false),
            &mut boxes,
        );
        assert!(known, "step {step}: reported edits are known");
        assert!(ix.follow(&prims), "step {step}: the index follows");
        let mut damage = Some(&boxes[..]);
        let got = loop {
            p.reset();
            let drawn = canvas::draw_cached(
                &mut p,
                &mut cache,
                view,
                &cam,
                key,
                damage,
                true,
                bg(),
                |p, v, c| {
                    let palette = Palette::xschem_dark();
                    p.fill_rect(v, rgba(palette.background));
                    canvas::draw_grid(p, v, c, 10_000, &palette, &mut scratch);
                    let (a, b) = (c.to_world(v, v.x0, v.y0), c.to_world(v, v.x1, v.y1));
                    let pad = 4_000;
                    let near = [
                        a.0.min(b.0) as i32 - pad,
                        a.1.min(b.1) as i32 - pad,
                        a.0.max(b.0) as i32 + pad,
                        a.1.max(b.1) as i32 + pad,
                    ];
                    ix.query(&prims, near, &mut rows);
                    let listed = Some(&rows);
                    canvas::draw_prims_in(
                        p,
                        v,
                        c,
                        &prims,
                        listed,
                        &palette,
                        &mut text,
                        &mut scratch,
                    )
                },
            );
            damage = Some(&[]);
            patched += usize::from(drawn == Drawn::Patched);
            if drawn != Drawn::Partial {
                break p.finish(Color::BLACK);
            }
        };
        let live = compacted(&prims);
        let mut fresh = CpuPainter::new(w, h, 1.0);
        let want = loop {
            fresh.reset();
            let drawn = canvas::draw_cached(
                &mut fresh,
                &mut CanvasCache::default(),
                view,
                &cam,
                key,
                None,
                true,
                bg(),
                |p, v, c| content(p, v, c, &live, &mut text, &mut scratch),
            );
            if drawn != Drawn::Partial {
                break fresh.finish(Color::BLACK);
            }
        };
        let bad = differing(&got, &want);
        assert_eq!(
            bad, 0,
            "step {step}: {bad} pixels differ from a fresh recording"
        );
        if step % 10 == 9 {
            cam.pan(3.0, -2.0);
        }
    }
    assert!(
        patched >= 20 && killed_total >= 40,
        "{patched} patched, {killed_total} killed"
    );
}

#[test]
fn reported_edits_patch_what_a_recording_draws() {
    reported_walk(0x9e37_79b9);
    reported_walk(0x5eed_1234);
}

/// Negative space for the oracle above: damage that misses the change leaves the old pixels
/// (the comparison would catch a diff that forgot a row), and unknown damage redraws.
#[test]
fn withheld_damage_shows_stale_pixels() {
    let mut prims = common::thousand_prims();
    let view = Rect::new(0.0, 0.0, 400.0, 300.0);
    let cam = Camera {
        cx: 300_000.0,
        cy: 190_000.0,
        scale: 0.001,
    };
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    let mut frame = |p: &mut CpuPainter, cache: &mut CanvasCache, key, damage, prims: &_| {
        p.reset();
        let drawn =
            canvas::draw_cached(p, cache, view, &cam, key, damage, true, bg(), |p, v, c| {
                content(p, v, c, prims, &mut text, &mut scratch)
            });
        (drawn, p.finish(Color::BLACK))
    };
    let (mut p, mut cache) = (CpuPainter::new(400, 300, 1.0), CanvasCache::default());
    assert_eq!(
        frame(&mut p, &mut cache, 1, None, &prims).0,
        Drawn::Recorded
    );
    prims.line(100_000, 100_000, 500_000, 300_000, 1);
    let (drawn, stale) = frame(&mut p, &mut cache, 2, Some(&[]), &prims);
    assert_eq!(drawn, Drawn::Copied, "no damage, nothing redrawn");
    let (_, fresh) = frame(
        &mut CpuPainter::new(400, 300, 1.0),
        &mut CanvasCache::default(),
        2,
        None,
        &prims,
    );
    assert!(differing(&stale, &fresh) > 100, "the new line is missing");
    let (drawn, again) = frame(&mut p, &mut cache, 3, None, &prims);
    assert_eq!(
        drawn,
        Drawn::Patched,
        "unknown damage redraws the visible tiles"
    );
    assert!(differing(&again, &fresh) == 0, "and they show the line");
}

/// Oracle (D39 level of detail): at far zooms, where most rows are drawn as the pixels their
/// box touches, the tiled frame equals a direct render pixel for pixel, at two device scales.
#[test]
fn far_zoom_tiles_equal_direct_renders() {
    let prims = common::thousand_prims();
    let view = Rect::new(13.0, 21.0, 460.0, 300.0);
    let mut fit = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    fit.fit(view, [0, 0, 600_000, 380_000]);
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    for (f, s) in [(0.25, 1.0), (0.125, 1.0), (1.0 / 32.0, 1.0), (0.125, 2.0)] {
        let cam = Camera {
            cx: fit.cx + 1234.5,
            cy: fit.cy - 777.0,
            scale: fit.scale * f,
        };
        let (w, h) = ((480.0 * s) as u16, (320.0 * s) as u16);
        let mut tiled = CpuPainter::new(w, h, s);
        let mut cache = CanvasCache::default();
        (cache.tiles_per_frame, cache.cost_per_frame) = (usize::MAX, usize::MAX);
        let drawn = canvas::draw_cached(
            &mut tiled,
            &mut cache,
            view,
            &cam,
            1,
            None,
            true,
            bg(),
            |p, v, c| content(p, v, c, &prims, &mut text, &mut scratch),
        );
        assert_eq!(drawn, Drawn::Recorded);
        let got = tiled.finish(Color::BLACK);
        let mut direct = CpuPainter::new(w, h, s);
        direct.push_clip(view);
        content(&mut direct, view, &cam, &prims, &mut text, &mut scratch);
        direct.pop_clip();
        let want = direct.finish(Color::BLACK);
        assert_eq!(differing(&got, &want), 0, "zoom ×{f}, device scale {s}");
    }
}

/// Oracle: tiles recorded in parts across frames (a budget smaller than any tile) show what
/// whole tiles do, pixel for pixel, after a fresh atlas and after pans that uncover tiles.
#[test]
fn tiles_recorded_in_parts_equal_direct_renders() {
    let prims = common::thousand_prims();
    let view = Rect::new(13.0, 21.0, 460.0, 300.0);
    let mut cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    cam.fit(view, [0, 0, 600_000, 380_000]);
    cam.scale *= 1.7;
    let (mut text, mut scratch) = (TextSystem::new(), CanvasScratch::default());
    for s in [1.0, 2.0] {
        let (w, h) = ((480.0 * s) as u16, (320.0 * s) as u16);
        let mut tiled = CpuPainter::new(w, h, s);
        let mut cache = CanvasCache::default();
        cache.cost_per_frame = 1; // every tile after the first costs more: 16 parts each
        let mut cam = cam;
        let mut partial = 0;
        for (dx, dy) in [(0.0, 0.0), (-300.0, -170.0), (260.0, 90.0)] {
            cam.pan(dx, dy);
            let mut drawn = Drawn::Partial;
            for _ in 0..4096 {
                tiled.reset();
                drawn = canvas::draw_cached(
                    &mut tiled,
                    &mut cache,
                    view,
                    &cam,
                    1,
                    Some(&[]),
                    true,
                    bg(),
                    |p, v, c| content(p, v, c, &prims, &mut text, &mut scratch),
                );
                if drawn != Drawn::Partial {
                    break;
                }
                partial += 1;
            }
            assert_ne!(drawn, Drawn::Partial, "the view completes");
            let got = tiled.finish(Color::BLACK);
            let mut direct = CpuPainter::new(w, h, s);
            direct.push_clip(view);
            content(&mut direct, view, &cam, &prims, &mut text, &mut scratch);
            direct.pop_clip();
            let want = direct.finish(Color::BLACK);
            assert_eq!(differing(&got, &want), 0, "pan ({dx}, {dy}), scale {s}");
        }
        assert!(partial >= 16, "{partial} partial frames: parts span frames");
    }
}
