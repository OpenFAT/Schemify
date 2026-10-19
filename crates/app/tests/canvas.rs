//! Oracle for the editor's retained canvas (D39): one session painted every step with a
//! painter that keeps its raster layer (so frames copy or patch it), one painted with a fresh
//! painter (so every frame records the whole canvas), both driven by the same seeded random
//! actions — selections, deletes, undo/redo, shapes drawn with the shape tools, and the view
//! options that change what the canvas draws (hidden layers, fill, line width, colors,
//! symbol boxes and texts, connected-instance highlight). Their windows must agree within
//! one level per channel (a patched part rounds unsnapped coordinates differently).

mod common;

use common::{Session, U, VIEW};
use schemify_app::checks;
use schemify_render::canvas::{CanvasPrims, LAYER_DEAD};
use schemify_render::vello::peniko::Color;
use schemify_render::{CpuPainter, Pixmap};

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) % n
    }
}

/// One action, on both sessions alike. Returns its name (for failure messages).
fn act(s: &mut Session, rng: &mut Rng) -> String {
    let at = |rng: &mut Rng| {
        (
            (rng.below(16) as i32 - 4) * 20 * U,
            (rng.below(12) as i32 - 4) * 20 * U,
        )
    };
    match rng.below(16) {
        0..=2 => {
            // Click an object (or empty canvas): the selection marks change.
            let n = s.ed.scene().objs.len();
            let p = if n > 0 && rng.below(4) != 0 {
                let g = s.ed.scene().objs.geom[rng.below(n as u64) as usize];
                ((g[0] + g[2]) / 2, (g[1] + g[3]) / 2)
            } else {
                at(rng)
            };
            s.click(p);
            format!("click {p:?}")
        }
        3 => {
            s.ed.command("edit.delete", "");
            s.frame(Default::default());
            "delete".into()
        }
        4 => {
            s.ed.command("edit.undo", "");
            s.frame(Default::default());
            "undo".into()
        }
        5 => {
            s.ed.command("edit.redo", "");
            s.frame(Default::default());
            "redo".into()
        }
        6 | 7 => {
            // A shape drawn with its tool: first point under the pointer, a click ends it.
            let tool = [
                "tools.insert_circle",
                "tools.insert_rect",
                "tools.insert_line",
            ][rng.below(3) as usize];
            let (a, b) = (at(rng), at(rng));
            s.move_to(a);
            s.ed.command(tool, "");
            s.frame(Default::default());
            s.click(b);
            s.key("Escape");
            format!("{tool} {a:?} {b:?}")
        }
        8 => {
            let l = rng.below(5) + 1;
            s.ed.command("layers.toggle", &l.to_string());
            s.frame(Default::default());
            format!("toggle layer {l}")
        }
        9 => {
            let cmd = [
                "view.toggle_fill_pattern",
                "view.line_width_increase",
                "view.reset_all_colors_to_default",
            ][rng.below(3) as usize];
            s.ed.command(cmd, "");
            s.frame(Default::default());
            cmd.into()
        }
        10 => {
            s.ed.command("symbol.show_symbols.cycle", "");
            s.frame(Default::default());
            "symbol boxes".into()
        }
        11 => {
            s.ed.command("view.show_hide.symbol_text", "");
            s.frame(Default::default());
            "symbol texts".into()
        }
        14 | 15 => {
            // Drag an object (a move: geometry and violations change).
            let n = s.ed.scene().objs.len();
            if n == 0 {
                return "nothing to drag".into();
            }
            let g = s.ed.scene().objs.geom[rng.below(n as u64) as usize];
            let a = ((g[0] + g[2]) / 2, (g[1] + g[3]) / 2);
            let b = (a.0 + (rng.below(5) as i32 - 2) * 20 * U, a.1 + 40 * U);
            s.click(a);
            s.drag(a, b, 0);
            format!("drag {a:?} -> {b:?}")
        }
        12 => {
            s.ed.command("highlight.enable_highlight_connected_instances", "");
            s.frame(Default::default());
            "highlight instances".into()
        }
        _ => {
            // A layer color: the current layer, then its color.
            let l = rng.below(5) + 1;
            s.ed.command("layers.set_current", &l.to_string());
            let rgb = format!("{:06x}", rng.below(1 << 24));
            s.ed.command("view.change_current_layer_color", &rgb);
            s.frame(Default::default());
            format!("layer {l} color {rgb}")
        }
    }
}

fn paint(s: &mut Session, p: &mut CpuPainter) -> Pixmap {
    p.reset();
    let drawn = s.ed.paint(p);
    s.ed.painted(drawn);
    p.finish(Color::BLACK)
}

/// (largest channel difference, pixels differing by more than 1).
fn compare(a: &Pixmap, b: &Pixmap) -> (u8, usize) {
    let mut worst = 0u8;
    let mut off = 0usize;
    for (x, y) in a.data().iter().zip(b.data()) {
        let d = [
            x.r.abs_diff(y.r),
            x.g.abs_diff(y.g),
            x.b.abs_diff(y.b),
            x.a.abs_diff(y.a),
        ];
        let m = d.into_iter().max().unwrap_or(0);
        worst = worst.max(m);
        off += usize::from(m > 1);
    }
    (worst, off)
}

/// The live rows of a primitive table, as a sorted set of their text.
fn live_rows(p: &CanvasPrims) -> Vec<String> {
    let r = &p.rects;
    let t = &p.texts;
    let mut rows: Vec<String> = (0..r.x0.len())
        .filter(|&i| r.layer[i] != LAYER_DEAD)
        .map(|i| {
            format!(
                "rect {} {} {} {} {}",
                r.x0[i], r.y0[i], r.x1[i], r.y1[i], r.layer[i]
            )
        })
        .collect();
    let mut start = 0;
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
        if t.layer[i] != LAYER_DEAD {
            let text = &t.text[start..end];
            rows.push(format!(
                "text {} {} {} {} {text}",
                t.x[i], t.y[i], t.size[i], t.layer[i]
            ));
        }
        start = end;
    }
    assert!(
        p.lines.x0.is_empty() && p.arcs.cx.is_empty(),
        "markers are rects and texts"
    );
    rows.sort();
    rows
}

/// Oracle for the incremental violation markers: what the editor keeps equals a full
/// placement over the same scene and overlay, as a set of live rows, and covers every
/// `checks` mark on a drawn object.
fn markers_match(s: &Session, ctx: &str) {
    let (h, _) = s.ed.active().expect("a level");
    let doc = s.ed.session().project.doc(h).expect("open");
    let mut full = checks::Markers::default();
    checks::place_markers(&mut full, s.ed.scene(), doc, s.ed.annotations(), None);
    assert_eq!(
        live_rows(s.ed.markers()),
        live_rows(&full.prims),
        "{ctx}: markers"
    );
}

fn walk(seed: u64, steps: usize) {
    let mut kept = Session::open("inv", "top.shcomp", &format!("canvas-kept-{seed}"), true);
    let mut fresh = Session::open("inv", "top.shcomp", &format!("canvas-fresh-{seed}"), true);
    let (w, h) = (VIEW.0 as u16, VIEW.1 as u16);
    let mut layers = CpuPainter::new(w, h, 1.0);
    let (mut rng_a, mut rng_b) = (Rng(seed), Rng(seed));
    paint(&mut kept, &mut layers);
    for step in 0..steps {
        let what = act(&mut kept, &mut rng_a);
        act(&mut fresh, &mut rng_b);
        assert_eq!(
            kept.text(),
            fresh.text(),
            "step {step} ({what}): same document"
        );
        markers_match(&kept, &format!("step {step} ({what})"));
        let got = paint(&mut kept, &mut layers);
        let want = paint(&mut fresh, &mut CpuPainter::new(w, h, 1.0));
        let (worst, off) = compare(&got, &want);
        assert!(
            worst <= 1,
            "step {step} ({what}): the kept canvas differs from a fresh recording by {worst} \
             ({off} pixels off by more than 1)"
        );
    }
}

#[test]
fn a_kept_canvas_equals_a_fresh_recording() {
    walk(1, 40);
    walk(2, 40);
    walk(3, 60);
}

/// Paint feeds the frame scheduler only through its return value: painting changes nothing
/// `wake_ms` reads; a partial outcome handed to `painted` asks for a frame at once, a
/// stretched one for the crisp frame once the zoom has settled (already, here), and the due
/// frame redraws and leaves the editor idle (D39). A finished outcome or no canvas asks for
/// nothing new.
#[test]
fn the_paint_outcome_schedules_the_next_frame_through_painted() {
    use schemify_render::canvas::Drawn;
    let mut s = Session::open("inv", "top.shcomp", "painted", true);
    assert_eq!(s.ed.wake_ms(), None, "idle after opening");
    let mut p = CpuPainter::new(VIEW.0 as u16, VIEW.1 as u16, 1.0);
    let drawn = s.ed.paint(&mut p);
    assert!(drawn.is_some(), "a schematic tab draws its canvas");
    assert_eq!(s.ed.wake_ms(), None, "paint alone schedules nothing");
    for outcome in [Drawn::Partial, Drawn::Stretched] {
        s.ed.painted(Some(outcome));
        assert_eq!(s.ed.wake_ms(), Some(0), "{outcome:?} asks for a frame");
        s.ed.painted(None);
        assert_eq!(s.ed.wake_ms(), Some(0), "no canvas keeps the request");
        assert!(s.frame(Default::default()), "the due frame redraws");
        assert_eq!(s.ed.wake_ms(), None, "{outcome:?}: idle once taken");
    }
    s.ed.painted(Some(Drawn::Partial));
    s.ed.painted(Some(Drawn::Recorded));
    assert_eq!(s.ed.wake_ms(), None, "a whole canvas asks for nothing");
    assert!(!s.frame(Default::default()), "and nothing redraws");
}

/// A frame with no input and nothing due does no work (D39 idle 0%): once the editor is idle
/// (no wake-up asked for), an empty update reports nothing changed, schedules nothing,
/// rebuilds no scene, and its paint copies the canvas tiles it has. The window does not even
/// run such a frame (it sleeps until input while `wake_ms` is `None`).
#[test]
fn an_idle_frame_does_no_work() {
    use schemify_render::canvas::Drawn;
    let mut s = Session::open("inv", "top.shcomp", "idle", true);
    let mut p = CpuPainter::new(VIEW.0 as u16, VIEW.1 as u16, 1.0);
    for _ in 0..8 {
        let drawn = s.ed.paint(&mut p);
        s.ed.painted(drawn);
        if s.ed.wake_ms().is_none() {
            break;
        }
        s.frame(Default::default());
    }
    assert_eq!(s.ed.wake_ms(), None, "idle");
    let built = s.ed.scene().built;
    for k in 0..4 {
        assert!(!s.frame(Default::default()), "frame {k}: nothing changed");
        assert_eq!(s.ed.wake_ms(), None, "frame {k}: nothing scheduled");
        assert_eq!(s.ed.scene().built, built, "frame {k}: no scene build");
        p.reset();
        assert_eq!(
            s.ed.paint(&mut p),
            Some(Drawn::Copied),
            "frame {k}: tiles copied"
        );
    }
    // Negative space: input does work (a wheel notch zooms and asks for the crisp frame).
    let mut ev = schemify_ui::InputEvents::default();
    ev.push(schemify_ui::InputEvent::Wheel {
        x: 300.0,
        y: 300.0,
        dx: 0.0,
        dy: 1.0,
    });
    assert!(s.frame(ev), "a zoom changes the picture");
    p.reset();
    let drawn = s.ed.paint(&mut p);
    assert_eq!(drawn, Some(Drawn::Stretched), "the old tiles stand in");
    s.ed.painted(drawn);
    assert!(s.ed.wake_ms().is_some(), "and the crisp frame is due");
}

/// Oracle for the patched plugin snapshot (`Host::snapshot`, whose debug build compares every
/// fill with a full refill on documents this small): random edits of a schematic of floating
/// wires and instances (hundreds of violations, several gone and added per pass, not side by
/// side), undo and redo, each followed by the ticks the editor asks for, so every fill follows
/// the change log and the previous check pass. A scene and markers check rides along.
#[test]
fn the_plugin_snapshot_follows_random_edits() {
    use schemify_doc::{Edit, EditRequest, Mode, Transaction, ops_from_text};
    use schemify_format::FileKind;
    let mut top = String::from("schemify 1 shcomp\n\nimpl sch schematic\n\n");
    for i in 0..40 {
        let (x, y) = ((i % 8) * 200, (i / 8) * 200);
        top.push_str(&format!(
            "inst m{i:05} dev:nmos\n  at {x} {y} R0\n  name M{i}\n"
        ));
        for k in 0..4 {
            let yy = y - 30 - k * 10;
            top.push_str(&format!(
                "wire w{i:03}{k:02} {} {yy} {} {yy}\n",
                x + 20,
                x + 120
            ));
        }
        top.push_str(&format!("label n{i:05} {} {} R0 n{i}\n", x + 120, y - 30));
    }
    top.push_str("\nvar wv 3\n");
    let dir = common::fixture("inv", "snapshot-walk");
    std::fs::write(dir.join("walk.shcomp"), top).unwrap();
    let ed = schemify_app::editor::Editor::new(schemify_app::editor::Options {
        no_default_ui: false,
        path: Some(dir.join("walk.shcomp")),
        seed: 3,
        ..Default::default()
    })
    .unwrap();
    let mut s = Session { ed, now: 1, dir };
    s.frame(Default::default());
    let mut rng = Rng(11);
    let (h, _) = s.ed.active().unwrap();
    let mut next = 0;
    for step in 0..120 {
        let doc = s.ed.session().project.doc(h).unwrap();
        let (w, inst) = (doc.wires(), doc.instances());
        let pick = |rng: &mut Rng, n: usize| rng.below(n.max(1) as u64) as usize;
        let text = match rng.below(10) {
            7 if !inst.id.is_empty() => {
                // A rename: unique, a duplicate (E009), a bus (more elements), back.
                let r = pick(&mut rng, inst.id.len());
                let name = ["MX", "M3", "MB[1:0]", "M7"][pick(&mut rng, 4)];
                format!(
                    "set_attr inst {} name {name}\n",
                    inst.id[r].text(doc.strings())
                )
            }
            8 if !inst.id.is_empty() => {
                // A param shown on the symbol: a value, a var, dropped.
                let r = pick(&mut rng, inst.id.len());
                let id = inst.id[r].text(doc.strings());
                match pick(&mut rng, 4) {
                    0 => format!("remove_attr inst {id} W\n"),
                    1 => format!("set_attr inst {id} W wv*2\n"),
                    _ => format!("set_attr inst {id} W {}\n", 1 + pick(&mut rng, 9)),
                }
            }
            9 => format!("set_field var wv 1 {}\n", 1 + pick(&mut rng, 9)),
            0 | 1 if !w.id.is_empty() => {
                // Two wires far apart in the violation order.
                let (a, b) = (pick(&mut rng, w.id.len()), pick(&mut rng, w.id.len()));
                let ids: Vec<String> = [a, b]
                    .iter()
                    .map(|&r| w.id[r].text(doc.strings()))
                    .collect();
                if ids[0] == ids[1] {
                    format!("delete wire {}\n", ids[0])
                } else {
                    format!("delete wire {}\ndelete wire {}\n", ids[0], ids[1])
                }
            }
            2 if !inst.id.is_empty() => {
                let r = pick(&mut rng, inst.id.len());
                format!("delete inst {}\n", inst.id[r].text(doc.strings()))
            }
            3 => {
                next += 1;
                let (x, y) = (pick(&mut rng, 1600) as i64, pick(&mut rng, 1000) as i64);
                format!("insert wire x{next:05} {x} {y} {} {y}\n", x + 60)
            }
            4 if !w.id.is_empty() => {
                let r = pick(&mut rng, w.id.len());
                let id = w.id[r].text(doc.strings());
                let (x, y) = (pick(&mut rng, 1600) as i64, pick(&mut rng, 1000) as i64);
                format!("put wire {id} {id} {x} {y} {} {y}\n", x + 40)
            }
            5 => "undo".into(),
            _ => "redo".into(),
        };
        let edit = match text.as_str() {
            "undo" => Edit::Undo,
            "redo" => Edit::Redo,
            t => Edit::Tx(Transaction {
                label: "walk".into(),
                mode: Mode::Commit,
                ops: ops_from_text(t, FileKind::Shcomp).unwrap_or_else(|e| panic!("{t}: {e:?}")),
            }),
        };
        s.ed.request_edits([EditRequest {
            doc: h,
            edit,
            unix_ms: 0,
        }]);
        s.frame(Default::default());
        for _ in 0..4 {
            if s.ed.wake_ms() == Some(0) {
                s.frame(Default::default());
            }
        }
        markers_match(&s, &format!("step {step} ({text:?})"));
        if step % 10 == 9 {
            same_as_reopened(&mut s, &format!("step {step} ({text:?})"));
        }
    }
}

/// The scene's texts (symbol texts show evaluated params) and the violations of an editor
/// that followed edits equal those of an editor that opens the same text afresh.
fn same_as_reopened(s: &mut Session, ctx: &str) {
    let dir = common::fixture("inv", "snapshot-walk-reopened");
    let path = dir.join("walk.shcomp");
    std::fs::write(&path, s.text()).unwrap();
    let ed = schemify_app::editor::Editor::new(schemify_app::editor::Options {
        no_default_ui: false,
        path: Some(path),
        seed: 3,
        ..Default::default()
    })
    .unwrap();
    let mut fresh = Session { ed, now: 1, dir };
    for _ in 0..8 {
        fresh.frame(Default::default());
    }
    let texts = |s: &Session| {
        let t = &s.ed.scene().prims.texts;
        let mut rows: Vec<String> = Vec::new();
        let mut start = 0;
        for i in 0..t.x.len() {
            let end = t.end[i] as usize;
            if t.layer[i] != LAYER_DEAD {
                rows.push(format!("{} {} {}", t.x[i], t.y[i], &t.text[start..end]));
            }
            start = end;
        }
        rows.sort();
        rows
    };
    let violations = |s: &Session| {
        let st = s.ed.cell().expect("elaborated");
        let n = |x: schemify_elab::Name| {
            if x == schemify_elab::Name::NONE {
                String::new()
            } else {
                st.names.get(x).to_owned()
            }
        };
        let rows: Vec<String> = st
            .violations
            .rows
            .iter()
            .map(|v| {
                format!(
                    "{} {} {} {}",
                    v.code.as_str(),
                    v.object.0,
                    n(v.param),
                    v.detail
                )
            })
            .collect();
        rows
    };
    assert_eq!(texts(s), texts(&fresh), "{ctx}: scene texts");
    assert_eq!(violations(s), violations(&fresh), "{ctx}: violations");
}

/// An edit's check pass comes after the frame delivered `DocChanged` to the plugins, so their
/// snapshot lags it: the editor asks for one tick at once, which refills it, and is idle
/// after it (D39). Without plugins there is no snapshot to refill.
#[test]
fn an_edit_refills_the_plugin_snapshot_on_the_next_tick() {
    for plugins in [true, false] {
        let mut s = Session::open("inv", "top.shcomp", &format!("snap-{plugins}"), plugins);
        s.frame(Default::default());
        assert_eq!(s.ed.wake_ms(), None, "idle after opening");
        let before = s.text();
        s.click((10 * U, 0));
        assert_eq!(s.ed.selection().len(), 1, "instance x1");
        s.ed.command("edit.delete", "");
        s.frame(Default::default());
        assert_ne!(s.text(), before, "deleted");
        if plugins {
            assert_eq!(s.ed.wake_ms(), Some(0), "the snapshot is due");
        }
        // The due tick, and the user config's copy of `DocChanged` (delivered on the next).
        for _ in 0..2 {
            if s.ed.wake_ms().is_some() {
                s.frame(Default::default());
            }
        }
        assert_eq!(
            s.ed.wake_ms(),
            None,
            "idle after the edit (plugins: {plugins})"
        );
    }
}
