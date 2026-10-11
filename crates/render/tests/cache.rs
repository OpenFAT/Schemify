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
) {
    let palette = Palette::xschem_dark();
    p.fill_rect(view, rgba(palette.background));
    canvas::draw_grid(p, view, cam, 10_000, &palette, scratch);
    canvas::draw_prims(p, view, cam, prims, &palette, text, scratch);
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
    let mut rng = Rng(seed);
    let mut key = 1u64;
    let (mut copied, mut recorded) = (0, 0);
    for step in 0..60 {
        // One random camera move (or a content change), then the frame both ways.
        let expect = match rng.below(8) {
            0..=3 => {
                let (dx, dy) = (
                    f64::from(rng.below(81)) - 40.0,
                    f64::from(rng.below(61)) - 30.0,
                );
                cam.pan(dx, dy);
                None // a whole-pixel pan: copied while it stays inside the margin
            }
            4 => {
                cam.pan(0.5, 0.25);
                Some(Drawn::Recorded) // off the pixel grid: never served from the layer
            }
            5 => {
                let f = [0.5, 0.8, 1.25, 2.0][rng.below(4) as usize];
                let (sx, sy) = (
                    view.x0 + f64::from(rng.below(view.width() as u32)),
                    view.y0 + f64::from(rng.below(view.height() as u32)),
                );
                cam.zoom_at(view, sx, sy, f);
                Some(Drawn::Recorded) // a crisp frame at a new zoom records
            }
            6 => {
                key += 1;
                Some(Drawn::Recorded) // the content changed
            }
            _ => Some(Drawn::Copied), // nothing moved
        };
        cached.reset();
        let drawn = canvas::draw_cached(
            &mut cached,
            &mut cache,
            view,
            &cam,
            key,
            true,
            bg(),
            |p, v, c| content(p, v, c, &prims, &mut text, &mut scratch),
        );
        if let Some(e) = expect {
            assert_eq!(drawn, e, "step {step}");
        }
        assert_ne!(drawn, Drawn::Direct, "the CPU painter keeps layers");
        copied += usize::from(drawn == Drawn::Copied);
        recorded += usize::from(drawn == Drawn::Recorded);
        let got = cached.finish(Color::BLACK);

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
        copied >= 10 && recorded >= 10,
        "{copied} copied, {recorded} recorded"
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

/// A zoom while not crisp stretches the recorded layer (a stand-in) and never re-records, even
/// once a zoom-out uncovers the view (background shows there); the next crisp frame records.
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
    let mut frame = |p: &mut CpuPainter, cam: &Camera, crisp: bool| {
        p.reset();
        let drawn = canvas::draw_cached(p, &mut cache, view, cam, 7, crisp, bg(), |p, v, c| {
            content(p, v, c, &prims, &mut text, &mut scratch)
        });
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
    assert_eq!(
        frame(&mut p, &cam, true).0,
        Drawn::Recorded,
        "crisp re-records"
    );
    assert_eq!(frame(&mut p, &cam, true).0, Drawn::Copied);
}

/// A painter that never recorded the layer (a fresh one) cannot be served from the cache: it
/// records instead of drawing nothing.
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
    for _ in 0..2 {
        let mut p = CpuPainter::new(200, 100, 1.0);
        let drawn =
            canvas::draw_cached(&mut p, &mut cache, view, &cam, 1, true, bg(), |p, v, c| {
                content(p, v, c, &prims, &mut text, &mut scratch)
            });
        assert_eq!(drawn, Drawn::Recorded);
    }
}
