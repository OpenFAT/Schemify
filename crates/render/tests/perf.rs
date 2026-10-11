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
            layers: None,
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

/// D39 for the waveform viewer: pan/zoom redraw of one pane holding three 1M-point traces must
/// build its scene well inside the 8 ms frame. Decimation (one min/max scan of the visible
/// samples per trace) dominates; tick and legend text is the rest.
#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn waves_pane_three_1m_point_traces() {
    use schemify_render::waves::{WaveLook, WaveScratch, draw_pane};
    use schemify_ui::waves::Drag;
    use schemify_wave::{RawPlot, Request, VarKind, Variable, Waves};

    let n = 1_000_000usize;
    let mut re: Vec<f64> = (0..n).map(|i| i as f64 * 1e-9).collect();
    let mut seed = 0x2545_f491_u32;
    for k in 0..3 {
        re.extend((0..n).map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let t = i as f64 * 1e-9;
            (t * 2e4 * (k + 1) as f64).sin() + f64::from(seed % 1000) * 1e-4
        }));
    }
    let vars = ["time", "v(a)", "v(b)", "v(c)"]
        .iter()
        .enumerate()
        .map(|(k, name)| Variable {
            name: (*name).into(),
            kind: if k == 0 {
                VarKind::Time
            } else {
                VarKind::Voltage
            },
        })
        .collect();
    let mut w = Waves::new(vec![
        RawPlot::from_columns("big", vars, re, vec![]).unwrap(),
    ]);
    for src in ["v(a)", "v(b)", "v(c)"] {
        let r = Request::AddTrace {
            src: src.into(),
            dataset: 0,
            pane: 0,
        };
        w.apply(&r).unwrap();
    }
    w.apply(&Request::ZoomX {
        at: 0.5,
        factor: 0.9,
    })
    .unwrap(); // samples on both sides
    let area = schemify_ui::Rect {
        x: 0.0,
        y: 0.0,
        w: 1500.0,
        h: 420.0,
    };
    let (look, mut text) = (WaveLook::default(), TextSystem::new());
    let mut s = WaveScratch::default();
    let mut scene = Scene::new();
    let mut times = Vec::with_capacity(200);
    for i in 0..210 {
        // A drag: pan one pixel per frame, alternating, so every frame decimates afresh.
        let by = if i % 2 == 0 {
            1.0 / 1442.0
        } else {
            -1.0 / 1442.0
        };
        w.apply(&Request::PanX { by }).unwrap();
        let t0 = Instant::now();
        scene.reset();
        let mut p = ScenePainter {
            scene: &mut scene,
            base: Affine::IDENTITY,
            layers: None,
        };
        draw_pane(
            &mut p,
            area,
            &w,
            0,
            false,
            Drag::None,
            &look,
            &mut text,
            &mut s,
        );
        let dt = t0.elapsed().as_secs_f64() * 1e3;
        if i >= 10 {
            times.push(dt);
        }
    }
    times.sort_by(f64::total_cmp);
    let (median, p95) = (times[times.len() / 2], times[times.len() * 95 / 100]);
    println!("waves pane, 3 x 1M points: median {median:.2} ms, p95 {p95:.2} ms per scene build");
    if cfg!(debug_assertions) {
        println!("debug build: budget not enforced");
        return;
    }
    assert!(
        median <= 4.0,
        "D39: waves scene build {median:.2} ms > 4 ms (half the 8 ms frame)"
    );
}
