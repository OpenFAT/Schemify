//! D39 budgets on a 100k-object schematic: open, scene build, per-frame scene encoding while
//! panning/zooming (the CPU half of "pan/zoom ≥ 120 fps"), and an edit round trip.
//!
//! `cargo test --release -p schemify-app --test perf -- --ignored --nocapture`

mod common;
use common::{U, VIEW, fixture};

use std::fmt::Write;
use std::time::Instant;

use schemify_app::editor::{Editor, Options};
use schemify_render::ScenePainter;
use schemify_render::vello::Scene;
use schemify_render::vello::kurbo::Affine;
use schemify_ui::{InputEvent, InputEvents};

fn id(prefix: char, n: u32) -> String {
    const A: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let mut s = String::from(prefix);
    for k in (0..5).rev() {
        s.push(A[((n >> (5 * k)) & 31) as usize] as char);
    }
    s
}

/// 10k instances, 60k wires, 20k labels, 10k texts on a 100×100 tile grid.
fn big_schematic() -> String {
    let mut t = String::from("schemify 1 shcomp\n\nimpl sch schematic\n\n");
    for i in 0..10_000u32 {
        let (x, y) = (i64::from(i % 100) * 200, i64::from(i / 100) * 200);
        let _ = writeln!(
            t,
            "inst {} dev:nmos\n  at {x} {y} R0\n  name M{i}",
            id('m', i)
        );
    }
    for i in 0..60_000u32 {
        let tile = i / 6;
        let (x, y) = (i64::from(tile % 100) * 200, i64::from(tile / 100) * 200);
        let k = i64::from(i % 6);
        let _ = writeln!(
            t,
            "wire {} {} {} {} {}",
            id('w', i),
            x + 20,
            y - 30 - k * 10,
            x + 120,
            y - 30 - k * 10
        );
    }
    for i in 0..20_000u32 {
        let tile = i / 2;
        let (x, y) = (i64::from(tile % 100) * 200, i64::from(tile / 100) * 200);
        let _ = writeln!(
            t,
            "label {} {} {} R0 n{i}",
            id('n', i),
            x + 120,
            y - 30 - i64::from(i % 2) * 10
        );
    }
    for i in 0..10_000u32 {
        let (x, y) = (i64::from(i % 100) * 200, i64::from(i / 100) * 200);
        let _ = writeln!(t, "text {} {} {} R0 t{i}", id('t', i), x - 60, y + 60);
    }
    t
}

/// D39 "param edit → recompute + redraw ≤ 8 ms for 1k dependents": a var edited on the docs
/// page (Enter in its field) re-evaluates 1000 instance overrides, re-projects the page,
/// recomposes the UI and rebuilds the scene with the new symbol labels — one `update`. Once
/// for a plain var, once for a var behind a `gmid` plugin call (D20: plugin call + cache
/// write folded into the edit's undo entry; the amend's undo + recommit nets to the cache
/// ops, so the cell state stays incremental).
#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn var_edit_with_1k_dependents_through_the_docs_page() {
    let dir = fixture("ota", "perf-docs");
    let mut t = String::from("schemify 1 shcomp\n\nimpl sch schematic\n\n");
    for i in 0..1000u32 {
        let (x, y) = (i64::from(i % 40) * 120, i64::from(i / 40) * 120);
        let _ = writeln!(
            t,
            "inst {} dev:nfet_01v8\n  at {x} {y} R0\n  name M{i}\n  W Wg\n  L L1",
            id('m', i)
        );
    }
    t.push_str("\nvar L1 0.5u\nvar k 1\nvar g 15\n");
    t.push_str("var Wg \"gmid.w(\\\"nfet_01v8\\\", g, 0.5u, 10u)*k\"\n");
    std::fs::write(dir.join("big.shcomp"), t).unwrap();
    let ed = Editor::new(Options {
        no_default_ui: false,
        path: Some(dir.join("big.shcomp")),
        seed: 1,
        ..Default::default()
    })
    .unwrap();
    let mut s = common::Session { ed, now: 1, dir };
    for _ in 0..3 {
        s.frame(InputEvents::default());
    }
    let measure = |s: &mut common::Session, row: u32, expr: &str| {
        s.click_node(&format!("docs.b.{row}.expr"));
        s.key("Ctrl+A");
        s.type_text(expr);
        let t = Instant::now();
        s.key("Enter");
        t.elapsed().as_secs_f64() * 1e3
    };
    let mut plain = Vec::new();
    let mut plugin = Vec::new();
    for k in 0..9 {
        plain.push(measure(&mut s, 0, &format!("0.{}u", 5 + k % 4)));
        plugin.push(measure(&mut s, 1, &format!("1.0{k}")));
    }
    let w = s.text();
    assert!(w.contains("  cached "), "the plugin path wrote its cache");
    plain.sort_by(f64::total_cmp);
    plugin.sort_by(f64::total_cmp);
    eprintln!(
        "var edit, 1000 dependents, one frame: plain median {:.2} ms (max {:.2}); \
         gmid-backed median {:.2} ms (max {:.2})",
        plain[4], plain[8], plugin[4], plugin[8]
    );
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn hundred_k_objects_open_build_pan_edit() {
    let dir = fixture("inv", "perf");
    std::fs::write(dir.join("big.shcomp"), big_schematic()).unwrap();
    let t0 = Instant::now();
    let mut ed = Editor::new(Options {
        no_default_ui: false,
        path: Some(dir.join("big.shcomp")),
        seed: 1,
        ..Default::default()
    })
    .unwrap();
    ed.update(&InputEvents::default(), VIEW, 0);
    let open = t0.elapsed();
    let prims = ed.scene().prims.len();
    let objs = ed.scene().objs.len();
    assert!(objs >= 100_000, "{objs} objects");

    // Pan/zoom frames: update (camera only) + encode the whole window into a vello scene.
    let mut scene = Scene::new();
    let mut times = Vec::new();
    let mut update_ms = Vec::new();
    for i in 0..60 {
        let mut ev = InputEvents::default();
        let (x, y) = (640.0 + (i % 2) as f32, 400.0);
        ev.push(InputEvent::Wheel {
            x,
            y,
            dx: 0.0,
            dy: if i % 2 == 0 { 0.05 } else { -0.05 },
        });
        let t = Instant::now();
        ed.update(&ev, VIEW, 1000 + i);
        let tu = t.elapsed().as_secs_f64() * 1e3;
        scene.reset();
        let mut p = ScenePainter {
            scene: &mut scene,
            base: Affine::IDENTITY,
        };
        ed.paint(&mut p);
        times.push(t.elapsed().as_secs_f64() * 1e3);
        update_ms.push(tu);
    }
    times.sort_by(f64::total_cmp);
    update_ms.sort_by(f64::total_cmp);
    let median = times[times.len() / 2];
    eprintln!("update median {:.2} ms", update_ms[update_ms.len() / 2]);

    // An edit: click selects the object under the pointer, Del deletes it (one transaction,
    // journal append, full scene rebuild).
    let (x, y) = ed.to_screen((100 * U, 0));
    let mut ev = InputEvents::default();
    ev.push(InputEvent::PointerMove { x, y });
    let b = schemify_ui::Button::Left;
    ev.push(InputEvent::PointerDown { x, y, button: b });
    ev.push(InputEvent::PointerUp { x, y, button: b });
    ed.update(&ev, VIEW, 5000);
    assert_eq!(ed.selection().len(), 1);
    let before = ed.scene().objs.len();
    let mut del = InputEvents::default();
    del.push(InputEvent::Key {
        chord: schemify_ui::Chord::parse("Del").unwrap(),
    });
    let t = Instant::now();
    ed.update(&del, VIEW, 6000);
    let edit = t.elapsed();
    assert_eq!(ed.scene().objs.len(), before - 1);

    eprintln!(
        "100k: {objs} objects, {prims} primitives; open+first build {:.0} ms; \
         pan/zoom frame (update + scene encode) median {median:.2} ms ({:.0} fps CPU side); \
         edit round trip {:.0} ms",
        open.as_secs_f64() * 1e3,
        1e3 / median,
        edit.as_secs_f64() * 1e3
    );
    let _ = std::fs::remove_dir_all(&dir);
}
