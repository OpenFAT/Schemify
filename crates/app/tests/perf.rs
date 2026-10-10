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
