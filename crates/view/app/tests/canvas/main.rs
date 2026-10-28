//! Oracle for the editor's retained canvas rows (D39): one session painted every step with a
//! painter that keeps its instance rows (so frames patch them where the tables changed), one
//! painted with a fresh painter (so every frame packs every row afresh), both driven by the
//! same seeded random
//! actions — selections, deletes, undo/redo, shapes drawn with the shape tools, and the view
//! options that change what the canvas draws (hidden layers, fill, line width, colors,
//! symbol boxes and texts, connected-instance highlight). Their windows must agree within
//! one level per channel (patched rows sit in other slots, so a layer's paths list them in
//! another order).

mod bytes;
#[path = "../common/mod.rs"]
mod common;
mod snapshot;

use common::{Session, U, Ui, VIEW};
use schemify_canvas::{CanvasPrims, LAYER_DEAD};
use schemify_editor::checks;
use schemify_render::peniko::Color;
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
    schemify_app::paint::frame(&mut s.ed, &mut s.caches, p);
    p.finish(Color::BLACK)
}

/// (largest channel difference, pixels differing by more than 1).
fn compare(a: &Pixmap, b: &Pixmap) -> (u8, usize) {
    let mut worst = 0u8;
    let mut off = 0usize;
    let pixel_pairs = a.data().iter().zip(b.data());
    for (x, y) in pixel_pairs {
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
    let mut kept = Session::open(
        "inv",
        "top.shcomp",
        &format!("canvas-kept-{seed}"),
        Ui::Default,
    );
    let mut fresh = Session::open(
        "inv",
        "top.shcomp",
        &format!("canvas-fresh-{seed}"),
        Ui::Default,
    );
    let (w, h) = (VIEW.0 as u16, VIEW.1 as u16);
    let mut rows = CpuPainter::new(w, h, 1.0);
    let (mut rng_a, mut rng_b) = (Rng(seed), Rng(seed));
    paint(&mut kept, &mut rows);
    for step in 0..steps {
        let what = act(&mut kept, &mut rng_a);
        act(&mut fresh, &mut rng_b);
        assert_eq!(
            kept.text(),
            fresh.text(),
            "step {step} ({what}): same document"
        );
        markers_match(&kept, &format!("step {step} ({what})"));
        let got = paint(&mut kept, &mut rows);
        let want = paint(&mut fresh, &mut CpuPainter::new(w, h, 1.0));
        let (worst, off) = compare(&got, &want);
        assert!(
            worst <= 1,
            "step {step} ({what}): the kept rows draw differently from fresh ones by {worst} \
             ({off} pixels off by more than 1)"
        );
    }
}

#[test]
fn a_kept_canvas_equals_a_fresh_pack() {
    walk(1, 40);
    walk(2, 40);
    walk(3, 60);
}

/// A frame with no input and nothing due does no work (D39 idle 0%): once the editor is idle
/// (no wake-up asked for), an empty update reports nothing changed, schedules nothing and
/// rebuilds no scene, and painting schedules nothing either. The window does not even run
/// such a frame (it sleeps until input while `wake_ms` is `None`).
#[test]
fn an_idle_frame_does_no_work() {
    let mut s = Session::open("inv", "top.shcomp", "idle", Ui::Default);
    let mut p = CpuPainter::new(VIEW.0 as u16, VIEW.1 as u16, 1.0);
    for _ in 0..8 {
        schemify_app::paint::frame(&mut s.ed, &mut s.caches, &mut p);
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
        schemify_app::paint::frame(&mut s.ed, &mut s.caches, &mut p);
        assert_eq!(s.ed.wake_ms(), None, "frame {k}: paint schedules nothing");
    }
    // Negative space: input does work (a wheel notch zooms); its frame is complete, so no
    // settle frame follows it.
    let mut ev = schemify_ui::InputEvents::default();
    ev.push(schemify_ui::InputEvent::Wheel {
        x: 300.0,
        y: 300.0,
        dx: 0.0,
        dy: 1.0,
    });
    assert!(s.frame(ev), "a zoom changes the picture");
    p.reset();
    schemify_app::paint::frame(&mut s.ed, &mut s.caches, &mut p);
    assert_eq!(s.ed.wake_ms(), None, "a zoom frame asks for no other");
    assert!(
        !s.frame(Default::default()),
        "and the next frame has nothing to do"
    );
}
