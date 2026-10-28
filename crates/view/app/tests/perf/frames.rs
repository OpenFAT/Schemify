//! Frame times while panning and zooming (10k / 100k / 1M objects), and the grid keeping up
//! with a pan or zoom.

use std::time::Instant;

use schemify_editor::editor::{Editor, Options};
use schemify_render::Batch;
use schemify_render::gpu::InstanceRows;
use schemify_ui::{Button, Chord, InputEvent, InputEvents};

use super::{Bench, Gpu, budget, event, schematic, tick_stats};
use crate::common::{U, VIEW, fixture};

/// Pan (middle drag), zoom (wheel notches) and a local edit (delete a wire, undo) on an
/// `n`-object schematic.
fn frame_times(n: u32) {
    let dir = fixture("inv", &format!("perf-{n}"));
    std::fs::write(dir.join("big.shcomp"), schematic(n)).expect("the test's temp dir is writable");
    let t0 = Instant::now();
    let mut ed = Editor::new(Options {
        no_default_ui: false,
        path: Some(dir.join("big.shcomp")),
        seed: 1,
        ..Default::default()
    })
    .expect("the fixture project opens");
    ed.update(&InputEvents::default(), VIEW, 0);
    let open = t0.elapsed().as_secs_f64() * 1e3;
    let (objs, prims) = (ed.scene().objs.len(), ed.scene().prims.len());
    assert!(objs >= n as usize, "{objs} objects");
    let mut gpu = Gpu::new(1.0);
    let floor = gpu.as_mut().map_or(0.0, Gpu::floor);
    eprintln!(
        "{n} objects ({objs} drawn, {prims} primitives): open + first build {open:.0} ms; GPU: {} \
         (timestamp floor {floor:.3} ms, included in the GPU numbers)",
        gpu.as_ref().map_or("none (skipped)", |g| g.name.as_str())
    );
    let mut b = Bench {
        caches: Default::default(),
        ed,
        scale: 1.0,
        batch: Batch::default(),
        rows: InstanceRows::default(),
        now: 0,
        gpu,
        cpu: Vec::new(),
        render: Vec::new(),
        gpu_ms: Vec::new(),
        atlas: Vec::new(),
        instances: Vec::new(),
    };
    // The whole schematic in view (zoom full), first frame packs the rows.
    b.frame(&InputEvents::default(), 1000, false);

    // Pan: a middle-button drag, 6 px per 8 ms frame, 720 px one way then back.
    let (cx, cy) = (640.0f32, 400.0f32);
    b.frame(
        &event(InputEvent::PointerMove { x: 300.0, y: cy }),
        8,
        false,
    );
    let mid = Button::Middle;
    b.frame(
        &event(InputEvent::PointerDown {
            x: 300.0,
            y: cy,
            button: mid,
        }),
        8,
        false,
    );
    let mut x = 300.0f32;
    for i in 0..240 {
        x += if (i / 120) % 2 == 0 { 6.0 } else { -6.0 };
        b.frame(&event(InputEvent::PointerMove { x, y: cy }), 8, true);
    }
    b.frame(
        &event(InputEvent::PointerUp {
            x,
            y: cy,
            button: mid,
        }),
        8,
        false,
    );
    let [pan, _, pan99] = b.report("pan");

    // Zoom: bursts of 6 wheel notches at the pointer, 16 ms apart, alternately in and out.
    // The wheel turns where the pointer is (as winit reports it).
    b.frame(&event(InputEvent::PointerMove { x: cx, y: cy }), 8, false);
    for burst in 0..10 {
        let dy = if burst % 2 == 0 { 1.0 } else { -1.0 };
        for _ in 0..6 {
            let wheel = InputEvent::Wheel {
                x: cx,
                y: cy,
                dx: 0.0,
                dy,
            };
            b.frame(&event(wheel), 16, true);
        }
        assert_eq!(
            b.ed.wake_ms(),
            None,
            "a zoom frame is whole: idle (D39 idle 0%)"
        );
    }
    let [_, zoom95, zoom99] = b.report("zoom");

    // A selection (click a wire), then a local edit: select a wire, delete it, undo; the
    // click frames, then the delete and undo frames, are measured.
    let key = |c: &str| {
        event(InputEvent::Key {
            chord: Chord::parse(c).expect("chord"),
        })
    };
    let select = |b: &mut Bench, k: i32, measure: bool| {
        let at = b.ed.to_screen((70 * U + 200 * U * (k % 4), -30 * U));
        b.frame(
            &event(InputEvent::PointerMove { x: at.0, y: at.1 }),
            50,
            false,
        );
        let down = InputEvent::PointerDown {
            x: at.0,
            y: at.1,
            button: Button::Left,
        };
        let up = InputEvent::PointerUp {
            x: at.0,
            y: at.1,
            button: Button::Left,
        };
        let mut click = event(down);
        click.push(up);
        b.frame(&click, 500, measure);
        assert_eq!(b.ed.selection().len(), 1, "a wire under the pointer");
    };
    for k in 0..12 {
        select(&mut b, k, true);
        b.ticks();
    }
    let [select50, ..] = b.report("select");
    // The edit's tick (the plugin snapshot behind its check pass) runs as the window runs it:
    // after the frame, before the next input.
    let mut ticks = Vec::new();
    for k in 0..12 {
        select(&mut b, k, false);
        ticks.extend(b.ticks());
        let before = b.ed.scene().objs.len();
        b.frame(&key("Del"), 500, true);
        assert_eq!(b.ed.scene().objs.len(), before - 1, "deleted");
        ticks.extend(b.ticks());
        b.frame(&key("U"), 500, true);
        assert_eq!(b.ed.scene().objs.len(), before, "undone");
        ticks.extend(b.ticks());
    }
    let [edit, ..] = b.report("edit");
    eprintln!("  after the edits: {}", tick_stats(ticks));
    if n == 100_000 {
        // The nightly regression gate's metrics (tests/perf/baseline.txt): CPU p50 per frame,
        // p95 of the zoom.
        eprintln!("PERF app.open_100k_ms {open:.3}");
        eprintln!("PERF app.pan_frame_100k_ms {pan:.3}");
        eprintln!("PERF app.zoom_frame_p95_100k_ms {zoom95:.3}");
        eprintln!("PERF app.select_frame_100k_ms {select50:.3}");
        eprintln!("PERF app.edit_100k_ms {edit:.3}");
        // D39 (amended 2026-10-18): interactive frames under 1 ms of CPU at 100k objects; the
        // tail of a continuous gesture, the median of a dozen clicks (one descheduled frame
        // is a dozen clicks' p95). Measured 2026-10-18 (load ~20): pan p99 0.31, zoom p99
        // 0.54, select p50 0.49.
        budget("pan p99", pan99, 1.0);
        budget("zoom p99", zoom99, 1.0);
        budget("select p50", select50, 1.0);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn frame_times_10k() {
    frame_times(10_000);
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn frame_times_100k() {
    frame_times(100_000);
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn frame_times_1m() {
    frame_times(1_000_000);
}

/// Issue #45: grid dots lag behind pan and zoom. Opens the symbol contact sheet (light content:
/// the picture is mostly grid) at device scale `scale`, pans with a middle drag (`step` px per
/// 8 ms frame) and zooms in bursts of wheel notches; every frame is whole, so none asks for
/// another. `grid` false turns the grid off (its share of the frame). Before the instanced
/// canvas the view switcher floating over the canvas sent every frame through vello (5.5 ms
/// GPU per pan frame at 1×, 14 ms at 2×, on the Intel iGPU); vello is gone (D5, 2026-10-25).
///
/// Prints frame times (`report`); `grid_keeps_up_with_pan_and_zoom` runs it at 1× and 2×, 6
/// and 40 px per frame, grid on and off.
fn grid_lag(scale: f64, step: f32, grid: bool) {
    let dir = fixture("symbols", &format!("perf-grid-{scale}-{step}-{grid}"));
    let ed = Editor::new(Options {
        no_default_ui: false,
        path: Some(dir.join("sheet.shcomp")),
        seed: 1,
        ..Default::default()
    })
    .expect("the fixture project opens");
    let mut b = Bench {
        caches: Default::default(),
        ed,
        scale,
        batch: Batch::default(),
        rows: InstanceRows::default(),
        now: 0,
        gpu: Gpu::new(scale),
        cpu: Vec::new(),
        render: Vec::new(),
        gpu_ms: Vec::new(),
        atlas: Vec::new(),
        instances: Vec::new(),
    };
    let whole = |b: &Bench| assert_eq!(b.ed.wake_ms(), None, "a camera frame asks for no other");
    if !grid {
        b.ed.command("options.draw_grid", "");
    }
    b.frame(&InputEvents::default(), 1000, false);
    let (cx, cy) = (640.0f32, 400.0f32);
    b.frame(&event(InputEvent::PointerMove { x: cx, y: cy }), 8, false);
    let mid = Button::Middle;
    let down = InputEvent::PointerDown {
        x: cx,
        y: cy,
        button: mid,
    };
    b.frame(&event(down), 8, false);
    let mut x = cx;
    for i in 0..240 {
        x += if (i / 60) % 2 == 0 { step } else { -step };
        b.frame(&event(InputEvent::PointerMove { x, y: cy }), 8, true);
        whole(&b);
    }
    let up = InputEvent::PointerUp {
        x,
        y: cy,
        button: mid,
    };
    b.frame(&event(up), 8, false);
    b.report("pan");
    for burst in 0..10 {
        let dy = if burst % 2 == 0 { 1.0 } else { -1.0 };
        for _ in 0..3 {
            let wheel = InputEvent::Wheel {
                x: cx,
                y: cy,
                dx: 0.0,
                dy,
            };
            b.frame(&event(wheel), 16, true);
            whole(&b);
        }
    }
    b.report("zoom");
    eprintln!("grid {grid} @{scale}x, pan {step} px/frame");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn grid_keeps_up_with_pan_and_zoom() {
    for scale in [1.0, 2.0] {
        for step in [6.0, 40.0] {
            for grid in [true, false] {
                grid_lag(scale, step, grid);
            }
        }
    }
}
