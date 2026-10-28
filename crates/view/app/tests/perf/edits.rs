//! Edit frames: a local edit round trip at 100k objects, a shared var edit, and a var edit
//! with 1k dependents through the docs page.

use std::fmt::Write;
use std::time::Instant;

use schemify_editor::editor::{Editor, Options};
use schemify_render::Batch;
use schemify_render::gpu::InstanceRows;
use schemify_ui::{Button, InputEvent, InputEvents};

use super::{Bench, Gpu, budget, event, id, key_event, schematic, tick_stats};
use crate::common::{self, U, VIEW, fixture};

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
    let mut s = common::Session {
        ed,
        now: 1,
        dir,
        caches: Default::default(),
    };
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
    eprintln!("PERF app.var_edit_1k_dependents_ms {:.3}", plain[4]);
    eprintln!("PERF app.var_edit_1k_dependents_gmid_ms {:.3}", plugin[4]);
}

/// Every edit frame on an `n`-object schematic, zoomed in as [`interactions`] is: the first
/// edit of the document (it creates the journal), then a dozen each of delete, undo, redo,
/// move (the drop of a drag), a wire drawn with the wire tool, a rename (an instance's name
/// through its properties dialog) and a parameter change (its first parameter). Each kind's
/// frames are reported apart, with the ticks the editor asks for after them (the window runs
/// those before the next input). Returns (kind, CPU p50/p95/p99, largest tick) per kind.
fn edit_frames(n: u32) -> Vec<(&'static str, [f64; 3], f64)> {
    let dir = fixture("inv", &format!("perf-edit-{n}"));
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
            b.frame(&InputEvents::default(), 200, false);
        }
        assert_eq!(b.ed.wake_ms(), None, "settled");
    };
    b.frame(&InputEvents::default(), 1000, false);
    settle(&mut b);
    let (cx, cy) = (640.0f32, 400.0f32);
    b.frame(&event(InputEvent::PointerMove { x: cx, y: cy }), 8, false);
    for _ in 0..8 {
        let wheel = InputEvent::Wheel {
            x: cx,
            y: cy,
            dx: 0.0,
            dy: 1.0,
        };
        b.frame(&event(wheel), 16, false);
    }
    settle(&mut b);
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
    let click = |b: &mut Bench, at: (f32, f32)| {
        b.frame(
            &event(InputEvent::PointerMove { x: at.0, y: at.1 }),
            50,
            false,
        );
        let mut c = event(press(at));
        c.push(release(at));
        b.frame(&c, 500, false);
        b.ticks();
    };
    let mut out = Vec::new();
    let mut ticks: Vec<f64> = Vec::new();
    let mut done = |b: &mut Bench, what: &'static str, ticks: &mut Vec<f64>| {
        let pct = b.report(what);
        let worst = ticks.iter().copied().fold(0.0, f64::max);
        eprintln!("  after {what}: {}", tick_stats(std::mem::take(ticks)));
        out.push((what, pct, worst));
    };
    // The first edit of the document: its journal starts.
    let at = wire(&b, 0);
    click(&mut b, at);
    assert_eq!(b.ed.selection().len(), 1, "a wire under the pointer");
    b.frame(&key_event("Del"), 500, true);
    ticks.extend(b.ticks());
    done(&mut b, "first", &mut ticks);
    b.frame(&key_event("U"), 500, false);
    b.ticks();
    // Delete, undo, redo (then undo again, so the document stays as it was).
    let objs = b.ed.scene().objs.len();
    let (mut del, mut undo, mut redo) = (Vec::new(), Vec::new(), Vec::new());
    let (mut tdel, mut tundo, mut tredo) = (Vec::new(), Vec::new(), Vec::new());
    for k in 0..12 {
        let at = wire(&b, k);
        click(&mut b, at);
        assert_eq!(b.ed.selection().len(), 1, "a wire under the pointer");
        for (key, times, t, len) in [
            ("Del", &mut del, &mut tdel, objs - 1),
            ("U", &mut undo, &mut tundo, objs),
            ("Shift+U", &mut redo, &mut tredo, objs - 1),
        ] {
            b.frame(&key_event(key), 500, true);
            times.push(b.cpu.pop().expect("measured"));
            assert_eq!(b.ed.scene().objs.len(), len, "{key}");
            t.extend(b.ticks());
        }
        b.frame(&key_event("U"), 500, false);
        b.ticks();
    }
    for (what, times, t) in [
        ("delete", del, tdel),
        ("undo", undo, tundo),
        ("redo", redo, tredo),
    ] {
        b.cpu = times;
        ticks = t;
        done(&mut b, what, &mut ticks);
    }
    // Move: press on a wire, drag it, drop it (the drop commits).
    for k in 0..12 {
        let at = wire(&b, k);
        click(&mut b, at);
        b.frame(&event(press(at)), 1000, false);
        for i in 1..=8 {
            let to = InputEvent::PointerMove {
                x: at.0 + i as f32 * 2.0,
                y: at.1 + i as f32,
            };
            b.frame(&event(to), 8, false);
        }
        b.frame(&event(release((at.0 + 16.0, at.1 + 8.0))), 8, true);
        ticks.extend(b.ticks());
        b.frame(&key_event("U"), 500, false);
        b.ticks();
    }
    done(&mut b, "move", &mut ticks);
    // A wire drawn with the wire tool: `w` at a free point, then a click.
    for k in 0..12 {
        let from =
            b.ed.to_screen((tx - 90 * U + 200 * U * (k % 4 - 2), ty + 90 * U));
        b.frame(
            &event(InputEvent::PointerMove {
                x: from.0,
                y: from.1,
            }),
            50,
            false,
        );
        b.frame(&key_event("w"), 300, false);
        let to = (from.0 + 30.0, from.1);
        b.frame(
            &event(InputEvent::PointerMove { x: to.0, y: to.1 }),
            50,
            false,
        );
        let mut c = event(press(to));
        c.push(release(to));
        b.frame(&c, 500, true);
        ticks.extend(b.ticks());
        b.frame(&key_event("Escape"), 300, false);
        b.frame(&key_event("U"), 500, false);
        b.ticks();
    }
    done(&mut b, "wire", &mut ticks);
    // Property changes through an instance's properties dialog: field 0 is its name, field
    // 1 its first parameter.
    for (what, field) in [("rename", 0), ("param", 1)] {
        for k in 0..12 {
            let inst = b.ed.to_screen((tx + 200 * U * (k % 4 - 2), ty));
            click(&mut b, inst);
            b.frame(&key_event("q"), 300, false);
            b.click_node(&format!("dialog.props.val.{field}"), 300, false);
            b.frame(&key_event("Ctrl+A"), 30, false);
            // A new value each time (an unchanged field commits nothing): unique names and,
            // every other time, the name of another instance (a duplicate, E009).
            let mut ev = InputEvents::default();
            ev.push_text(&match field {
                0 if k % 2 == 1 => format!("M{}", 100 + k),
                0 => format!("MX{k}"),
                _ => format!("{}", 2 + k),
            });
            b.frame(&ev, 30, false);
            b.click_node("dialog.props.ok", 300, true);
            ticks.extend(b.ticks());
        }
        done(&mut b, what, &mut ticks);
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn edit_frames_100k() {
    let r = edit_frames(100_000);
    for (what, [p50, p95, p99], tick) in &r {
        eprintln!("PERF app.edit_{what}_100k_ms {p50:.3}");
        eprintln!("  {what}: p95 {p95:.3}, p99 {p99:.3}, tick max {tick:.3}");
        // D39 (amended 2026-10-18, edit frames): a delete, an undo, a redo, the drop of a move,
        // a wire drawn, an instance rename and a param change cost under 1 ms of CPU at 100k
        // objects, and so do the ticks after them. Measured 2026-10-18 (load < 1): p95
        // 0.43-0.61 ms, ticks 0.18-0.64 ms.
        if ["delete", "undo", "redo", "move", "wire", "rename", "param"].contains(what) {
            budget(&format!("{what} p95"), *p95, 1.0);
            budget(&format!("{what} tick"), *tick, 1.0);
        }
    }
    // The first edit of a document (its journal starts) has no hash or rewrite of the file
    // left in it; it still pays first-use growth of the tables it patches (2.7 ms measured).
    let (_, [first, ..], _) = r[0];
    budget("first edit", first, 4.0);
}

/// A var many instances read, edited at 100k objects: O(dependents) by nature (each
/// dependent is evaluated and rechecked, and its value texts rewritten in place while their
/// length holds, else it is re-stamped). Every tenth instance reads `wl` (1k dependents) and
/// every instance reads `wv` (10k dependents).
#[test]
#[ignore = "release-mode benchmark; run explicitly"]
fn shared_var_edit_frames_100k() {
    use schemify_doc::{Edit, EditRequest, Mode, Op, Target, Transaction};
    use schemify_format::RecordKind;
    let dir = fixture("inv", "perf-shared-var");
    let mut text = String::new();
    for line in schematic(100_000).lines() {
        text.push_str(line);
        text.push('\n');
        if let Some(i) = line.strip_prefix("  name M") {
            text.push_str("  W wv\n");
            if i.parse::<u32>().expect("an index") % 10 == 0 {
                text.push_str("  L wl\n");
            }
        }
    }
    text.push_str("\nvar wv 2\nvar wl 0.5\n");
    std::fs::write(dir.join("big.shcomp"), text).unwrap();
    let ed = Editor::new(Options {
        no_default_ui: false,
        path: Some(dir.join("big.shcomp")),
        seed: 1,
        ..Default::default()
    })
    .unwrap();
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
    for _ in 0..8 {
        b.frame(&InputEvents::default(), 1000, false);
    }
    let (h, _) = b.ed.active().expect("a document");
    // Fitted to the window (every dependent on screen), then zoomed in as `edit_frames`.
    let cases = [
        ("wl", 1000, "fit"),
        ("wv", 10_000, "fit"),
        ("wl", 1000, "zoomed"),
        ("wv", 10_000, "zoomed"),
    ];
    let mut zoomed = false;
    for (var, dependents, view) in cases {
        if view == "zoomed" && !zoomed {
            zoomed = true;
            let (cx, cy) = (640.0f32, 400.0f32);
            b.frame(&event(InputEvent::PointerMove { x: cx, y: cy }), 8, false);
            for _ in 0..8 {
                let wheel = InputEvent::Wheel {
                    x: cx,
                    y: cy,
                    dx: 0.0,
                    dy: 1.0,
                };
                b.frame(&event(wheel), 16, false);
            }
            for _ in 0..256 {
                if b.ed.wake_ms().is_none() {
                    break;
                }
                b.frame(&InputEvents::default(), 200, false);
            }
        }
        let mut ticks = Vec::new();
        for k in 0..12 {
            let op = Op::SetField {
                target: Target::new(RecordKind::Var, var),
                index: 1,
                value: format!("{}", 3 + k),
            };
            b.ed.request_edits([EditRequest {
                doc: h,
                edit: Edit::Tx(Transaction {
                    label: "var".into(),
                    mode: Mode::Commit,
                    ops: vec![op],
                }),
                unix_ms: 0,
            }]);
            b.frame(&InputEvents::default(), 500, true);
            let st = b.ed.cell().expect("elaborated");
            assert_eq!(
                st.revalued.len(),
                dependents,
                "{var}: every dependent revalued"
            );
            ticks.extend(b.ticks());
        }
        let [p50, p95, _] = b.report(&format!("{var} {view}"));
        let tick = ticks.iter().copied().fold(0.0, f64::max);
        eprintln!("PERF app.edit_var_{dependents}_dependents_{view}_100k_ms {p50:.3}");
        eprintln!("  {var} {view}: p95 {p95:.3}, tick max {tick:.3}");
        // D39's param-edit budget holds at 1k dependents (texts rewritten in place); 10k
        // dependents are O(dependents) over budget (docs/dev/perf.md, "Shared-variable edits").
        if dependents == 1000 {
            budget(&format!("var edit, 1k dependents, {view}, p50"), p50, 8.0);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
