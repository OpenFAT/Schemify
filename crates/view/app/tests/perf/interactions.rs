//! Every other interactive frame (hover, select, drag, typing in a field, opening a menu):
//! under 1 ms of CPU each (D39 amended 2026-10-18).

use schemify_editor::editor::{Editor, Options};
use schemify_render::Batch;
use schemify_render::gpu::InstanceRows;
use schemify_ui::{Button, Chord, InputEvent, InputEvents};

use super::{Bench, Gpu, Measure, budget, event, schematic, tick_stats};
use crate::common::{U, VIEW, fixture};

/// Every other interactive frame on an `n`-object schematic, zoomed in so objects are big
/// enough to hit: hover (the pointer crossing the canvas, the status line following it),
/// select (a click on a wire), drag (a press on the selected wire and 60 moves: the move
/// preview), the drop that commits it (an edit) and the tick after it, opening a menu, and
/// typing into a properties dialog field. Pan and zoom are [`frame_times`].
fn interactions(n: u32) {
    let dir = fixture("inv", &format!("perf-int-{n}"));
    std::fs::write(dir.join("big.shcomp"), schematic(n)).expect("the test's temp dir is writable");
    let mut ed = Editor::new(Options {
        no_default_ui: false,
        path: Some(dir.join("big.shcomp")),
        seed: 1,
        ..Default::default()
    })
    .expect("the fixture project opens");
    ed.update(&InputEvents::default(), VIEW, 0);
    let mut b = Bench {
        caches: Default::default(),
        ed,
        scale: 1.0,
        batch: Batch::default(),
        rows: InstanceRows::default(),
        now: 0,
        gpu: Gpu::new(1.0),
        cpu: Vec::new(),
        render: Vec::new(),
        gpu_ms: Vec::new(),
        atlas: Vec::new(),
        instances: Vec::new(),
    };
    let settle = |b: &mut Bench| {
        // Bounded by the background work a file opens: the disk hash takes ~282 ticks at 1M.
        for _ in 0..1024 {
            if b.ed.wake_ms().is_none() {
                break;
            }
            b.frame(&InputEvents::default(), 200, Measure::Skip);
        }
        assert_eq!(b.ed.wake_ms(), None, "settled");
    };
    b.frame(&InputEvents::default(), 1000, Measure::Skip);
    settle(&mut b);
    let (cx, cy) = (640.0f32, 400.0f32);
    b.frame(
        &event(InputEvent::PointerMove { x: cx, y: cy }),
        8,
        Measure::Skip,
    );
    for _ in 0..8 {
        let wheel = InputEvent::Wheel {
            x: cx,
            y: cy,
            dx: 0.0,
            dy: 1.0,
        };
        b.frame(&event(wheel), 16, Measure::Skip);
    }
    settle(&mut b);
    // Hover: 4 px per frame across the canvas.
    for i in 0..240 {
        let x = 200.0 + (i % 120) as f32 * 4.0;
        let y = cy + (i / 120) as f32 * 3.0;
        b.frame(&event(InputEvent::PointerMove { x, y }), 8, Measure::Record);
    }
    let [hover, hover95, _] = b.report("hover");
    // Select: wires of the tiles around the view's center.
    let cam = b.ed.camera();
    let tile = f64::from(200 * U);
    let (tx, ty) = (
        (cam.cx / tile).round() as i32 * 200 * U,
        (cam.cy / tile).round() as i32 * 200 * U,
    );
    let wire = |b: &Bench, k: i32| {
        b.ed.to_screen((tx + 70 * U + 200 * U * (k % 4 - 2), ty - 30 * U))
    };
    let press = |at: (f32, f32)| InputEvent::PointerDown {
        x: at.0,
        y: at.1,
        button: Button::Left,
    };
    let release = |at: (f32, f32)| InputEvent::PointerUp {
        x: at.0,
        y: at.1,
        button: Button::Left,
    };
    for k in 0..12 {
        let at = wire(&b, k);
        b.frame(
            &event(InputEvent::PointerMove { x: at.0, y: at.1 }),
            50,
            Measure::Skip,
        );
        let mut click = event(press(at));
        click.push(release(at));
        b.frame(&click, 500, Measure::Record);
        assert_eq!(b.ed.selection().len(), 1, "a wire under the pointer");
        b.ticks();
    }
    let [select, ..] = b.report("select");
    // Drag the selected wire (past the double-click time), then drop it.
    let at = wire(&b, 11);
    b.frame(&event(press(at)), 1000, Measure::Skip);
    for i in 1..=60 {
        let to = InputEvent::PointerMove {
            x: at.0 + i as f32 * 2.0,
            y: at.1 + i as f32,
        };
        b.frame(&event(to), 8, Measure::Record);
    }
    assert!(!b.ed.overlay().is_empty(), "the move preview");
    let [drag, drag95, _] = b.report("drag");
    let to = (at.0 + 120.0, at.1 + 60.0);
    b.frame(&event(release(to)), 8, Measure::Record);
    b.report("drop");
    eprintln!("  after the drop: {}", tick_stats(b.ticks()));
    // Open the Tools menu (a click on its title), close it.
    let escape = event(InputEvent::Key {
        chord: Chord::parse("Escape").expect("chord"),
    });
    for _ in 0..8 {
        b.click_node("menu.tools", 300, Measure::Record);
        b.frame(&escape, 300, Measure::Skip);
    }
    let [menu, ..] = b.report("menu");
    // Type into the first field of an instance's properties dialog, one character a frame.
    let inst = b.ed.to_screen((tx, ty));
    let mut click = event(InputEvent::PointerMove {
        x: inst.0,
        y: inst.1,
    });
    click.push(press(inst));
    click.push(release(inst));
    b.frame(&click, 500, Measure::Skip);
    let q = event(InputEvent::Key {
        chord: Chord::parse("q").expect("chord"),
    });
    b.frame(&q, 300, Measure::Skip);
    b.click_node("dialog.props.val.0", 300, Measure::Skip);
    for i in 0..60 {
        let mut ev = InputEvents::default();
        ev.push_text(if i % 2 == 0 { "a" } else { "b" });
        b.frame(&ev, 30, Measure::Record);
    }
    let [typing, typing95, _] = b.report("type");
    // At 100k only (the select-all rounds are the slow part at 1M).
    if n == 100_000 {
        // Unselect (HANDOFF bug 3): select everything, then an Alt-drag box around the whole
        // schematic drops it all; the release frame is measured (O(selected × hits) before the
        // sort and binary search: ~10^10 comparisons at 100k).
        b.frame(&escape, 300, Measure::Skip);
        b.frame(&escape, 300, Measure::Skip);
        b.ed.command("view.zoom_full", "");
        b.frame(&InputEvents::default(), 300, Measure::Skip);
        for _ in 0..2 {
            let wheel = InputEvent::Wheel {
                x: cx,
                y: cy,
                dx: 0.0,
                dy: -1.0,
            };
            b.frame(&event(wheel), 16, Measure::Skip);
        }
        settle(&mut b);
        let side = (f64::from(n / 10).sqrt().ceil() as i32).max(1);
        let (lo, hi) = (
            b.ed.to_screen((-200 * U, -200 * U)),
            b.ed.to_screen((side * 200 * U, side * 200 * U)),
        );
        let alt = |m: u8| InputEvent::Mods {
            mods: schemify_ui::Mods(m),
        };
        // Resident MiB (Linux; `None` elsewhere): each select-all frame once grew it by
        // ~0.7 GB, every canvas tile recording every selection mark.
        let rss = || {
            let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
            let pages: u64 = statm.split(' ').nth(1)?.parse().ok()?;
            Some((pages * 4096) >> 20)
        };
        let mut resident = Vec::new();
        for _ in 0..12 {
            b.ed.command("edit.select_all", "");
            b.frame(&InputEvents::default(), 300, Measure::Skip);
            b.ticks();
            resident.extend(rss());
            let all = b.ed.selection().len();
            assert!(all >= n as usize, "everything selected: {all}");
            let mut down = event(alt(schemify_ui::Mods::ALT));
            down.push(InputEvent::PointerMove { x: lo.0, y: lo.1 });
            down.push(press(lo));
            b.frame(&down, 1000, Measure::Skip);
            b.frame(
                &event(InputEvent::PointerMove { x: hi.0, y: hi.1 }),
                8,
                Measure::Skip,
            );
            b.frame(&event(release(hi)), 8, Measure::Record);
            assert!(b.ed.selection().is_empty(), "the box unselected everything");
            b.frame(&event(alt(0)), 8, Measure::Skip);
            b.ticks();
        }
        if let (Some(first), Some(last)) = (resident.get(1), resident.last()) {
            eprintln!("  select-all rounds: resident {resident:?} MiB");
            // Without a GPU nothing rasterizes the layers, so their patches stay queued
            // (~19 MiB a round here); with one, the rounds measured flat (±30 MiB).
            let cap = if b.gpu.is_some() { 96 } else { 384 };
            assert!(
                last.saturating_sub(*first) < cap,
                "select-all frames retain memory: {resident:?} MiB"
            );
        }
        let [unselect, unselect95, _] = b.report("unselect");
        eprintln!("PERF app.unselect_frame_100k_ms {unselect:.3}");
        // Measured 2026-10-24 (release, no GPU, shared host): p50 7.6 ms, p95 11.3 ms with a
        // binary search of the sorted hits per selected key (6.6 ms of `Editor::update`'s 7.2);
        // p50 0.81, p95 0.81 with the hit bitset (`Editor::unselected`). What is left at 100k:
        // box_select 0.23, the bitset pass 0.23, clearing the selected set 0.16, UI 0.13.
        eprintln!("  unselect p95 {unselect95:.3} ms");
        eprintln!("PERF app.hover_frame_100k_ms {hover:.3}");
        eprintln!("PERF app.click_select_100k_ms {select:.3}");
        eprintln!("PERF app.drag_frame_100k_ms {drag:.3}");
        eprintln!("PERF app.menu_open_100k_ms {menu:.3}");
        eprintln!("PERF app.type_frame_100k_ms {typing:.3}");
        // D39 (amended 2026-10-18): under 1 ms of CPU per interactive frame at 100k objects:
        // the p95 of the continuous ones, the median of the clicks (see `frame_times`).
        // Measured 2026-10-18 (load ~20): hover p95 0.21, drag p95 0.21, type p95 0.25,
        // select p50 0.50, menu p50 0.20 (its first open, which shapes the rows: 0.50).
        budget("hover p95", hover95, 1.0);
        budget("drag p95", drag95, 1.0);
        budget("type p95", typing95, 1.0);
        budget("select p50", select, 1.0);
        budget("menu p50", menu, 1.0);
        budget("unselect p50", unselect, 1.0);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn interactions_100k() {
    interactions(100_000);
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn interactions_1m() {
    interactions(1_000_000);
}
