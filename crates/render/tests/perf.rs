//! D39 budget check: building the vello scene for 100k visible canvas primitives (plus the UI
//! shell) must take ≤ 4 ms so an 8 ms (120 fps) frame leaves room for the GPU.
//!
//! `cargo test --release -p schemify-render --test perf -- --ignored --nocapture`

use std::time::Instant;

use schemify_render::ScenePainter;
use schemify_render::canvas::{self, Camera, CanvasPrims, CanvasScratch, Marks, Palette};
use schemify_render::vello::Scene;
use schemify_render::vello::kurbo::{Affine, Rect};
use schemify_render::widgets::{UiScratch, draw_ui};
use schemify_ui::{Layer, Strings, TextSystem, UiState, compose, layout, records};

mod common;
use common::hundred_k;

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn scene_build_100k_primitives() {
    let prims = hundred_k();
    let view = Rect::new(0.0, 0.0, 1600.0, 900.0);
    let mut cam = Camera {
        cx: 0.0,
        cy: 0.0,
        scale: 1.0,
    };
    cam.fit(view, [0, 0, 1_600_000, 900_000]);
    let mut strings = Strings::new();
    let (base, _) = records::parse(schemify_ui::DEFAULT_UI, &mut strings);
    let tables = compose(&strings, &base, &[]);
    let mut text = TextSystem::new();
    let state = UiState::default();
    let frame = layout(&tables, &strings, &mut text, &state, (1600.0, 1000.0));
    let (palette, none, marks) = (
        Palette::xschem_dark(),
        CanvasPrims::default(),
        Marks::default(),
    );
    let (mut scratch, mut ui) = (CanvasScratch::default(), UiScratch::default());
    let mut scene = Scene::new();
    let mut times = Vec::with_capacity(200);
    for i in 0..210 {
        let t0 = Instant::now();
        scene.reset();
        let mut p = ScenePainter {
            scene: &mut scene,
            base: Affine::IDENTITY,
        };
        draw_ui(
            &mut p,
            &tables,
            &frame,
            &strings,
            &mut text,
            &state,
            Layer::Main,
            &[],
            &mut ui,
        );
        // Jitter one pixel per frame, as a drag does, keeping all 100k primitives in view.
        cam.pan(if i % 2 == 0 { 1.0 } else { -1.0 }, 0.0);
        canvas::draw_canvas(
            &mut p,
            view,
            &cam,
            20_000,
            &prims,
            &palette,
            &none,
            &marks,
            &mut text,
            &mut scratch,
        );
        draw_ui(
            &mut p,
            &tables,
            &frame,
            &strings,
            &mut text,
            &state,
            Layer::Overlay,
            &[],
            &mut ui,
        );
        let dt = t0.elapsed().as_secs_f64() * 1e3;
        if i >= 10 {
            times.push(dt); // first frames grow the scratch buffers
        }
    }
    times.sort_by(f64::total_cmp);
    let (median, p95) = (times[times.len() / 2], times[times.len() * 95 / 100]);
    let bytes = scene.encoding().path_data.len() * 4;
    println!(
        "scene build, 100k primitives + shell: median {median:.2} ms, p95 {p95:.2} ms (path data {} KiB)",
        bytes / 1024
    );
    if cfg!(debug_assertions) {
        println!("debug build: budget not enforced");
        return;
    }
    assert!(
        median <= 4.0,
        "D39: scene build {median:.2} ms > 4 ms budget"
    );
}
