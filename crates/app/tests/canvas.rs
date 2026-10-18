//! Oracle for the editor's retained canvas (D39): one session painted every step with a
//! painter that keeps its raster layer (so frames copy or patch it), one painted with a fresh
//! painter (so every frame records the whole canvas), both driven by the same seeded random
//! actions — selections, deletes, undo/redo, shapes drawn with the shape tools, and the view
//! options that change what the canvas draws (hidden layers, fill, line width, colors,
//! symbol boxes and texts, connected-instance highlight). Their windows must agree within
//! one level per channel (a patched part rounds unsnapped coordinates differently).

mod common;

use common::{Session, U, VIEW};
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
    match rng.below(14) {
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
