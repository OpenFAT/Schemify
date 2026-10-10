//! xschem's main-window shell driven entirely by UI tables: the default-UI records compose the
//! menubar, toolbar, tabs, canvas and status bar; the canvas shows a few hundred synthetic
//! schematic primitives. Middle-drag or arrow keys pan, the wheel zooms, left click selects a
//! segment, keyboard shortcuts print their command.
//!
//! Native: `cargo run --release -p schemify-render --example demo`
//! (set `SCHEMIFY_FPS=1` to print scene-build times; `WGPU_POWER_PREF=high` picks a discrete
//! GPU). Web: see `crates/render/web/README.md`.

use schemify_render::canvas::{self, Camera, CanvasPrims, CanvasScratch, Marks, Palette, PrimKind};
use schemify_render::vello::kurbo::Rect;
use schemify_render::widgets::{UiScratch, draw_ui};
use schemify_render::{App, Painter, run};
use schemify_ui::{
    Button, Contribution, InputEvent, InputEvents, Kind, Layer, Routed, Strings, Sym, TextSystem,
    UiLayout, UiState, UiTables, access, compose, layout, records, route_input,
};

/// 1 xschem unit in milli-units (D33).
const U: i32 = 1000;

struct Demo {
    strings: Strings,
    base: Contribution,
    session: Contribution,
    session_name: Sym,
    tables: UiTables,
    text: TextSystem,
    state: UiState,
    frame: UiLayout,
    viewport: (f32, f32),
    dirty_layout: bool,
    routed: Routed,
    prims: CanvasPrims,
    overlay: CanvasPrims,
    marks: Marks,
    palette: Palette,
    cam: Camera,
    fitted: bool,
    drag: Option<(f32, f32)>,
    grid: bool,
    canvas_scratch: CanvasScratch,
    ui_scratch: UiScratch,
    #[cfg(not(target_arch = "wasm32"))]
    timing: (u32, f64),
}

impl Demo {
    fn new() -> Demo {
        let mut strings = Strings::new();
        let (base, rejects) = records::parse(schemify_ui::DEFAULT_UI, &mut strings);
        assert!(rejects.is_empty(), "default UI rejects: {rejects:?}");
        let session_name = strings.intern("session");
        let mut prims = CanvasPrims::default();
        synthetic_schematic(&mut prims, 6, 4);
        let mut d = Demo {
            strings,
            base,
            session: Contribution::default(),
            session_name,
            tables: UiTables::default(),
            text: TextSystem::new(),
            state: UiState::default(),
            frame: UiLayout::default(),
            viewport: (1.0, 1.0),
            dirty_layout: true,
            routed: Routed::default(),
            prims,
            overlay: CanvasPrims::default(),
            marks: Marks::default(),
            palette: Palette::xschem_dark(),
            cam: Camera {
                cx: 0.0,
                cy: 0.0,
                scale: 0.001,
            },
            fitted: false,
            drag: None,
            grid: true,
            canvas_scratch: CanvasScratch::default(),
            ui_scratch: UiScratch::default(),
            #[cfg(not(target_arch = "wasm32"))]
            timing: (0, 0.0),
        };
        d.recompose();
        d
    }

    /// The app's own state goes back into the UI as a contribution (immediate mode).
    fn recompose(&mut self) {
        let text = format!("set options.draw_grid checked {}\n", u8::from(self.grid));
        let (c, _) = records::parse(&text, &mut self.strings);
        self.session = c;
        self.tables = compose(
            &self.strings,
            &self.base,
            &[(self.session_name, &self.session)],
        );
        self.dirty_layout = true;
    }

    fn canvas_rect(&self) -> Rect {
        let n = self
            .tables
            .nodes
            .kind()
            .iter()
            .position(|&k| k == Kind::Canvas)
            .map(|i| i as u16);
        let r = n
            .and_then(|n| self.frame.row_of(n))
            .map(|r| self.frame.rect()[r])
            .unwrap_or_default();
        Rect::new(
            f64::from(r.x),
            f64::from(r.y),
            f64::from(r.x + r.w),
            f64::from(r.y + r.h),
        )
    }

    fn relayout(&mut self) {
        self.frame = layout(
            &self.tables,
            &self.strings,
            &mut self.text,
            &self.state,
            self.viewport,
        );
        self.dirty_layout = false;
        if !self.fitted {
            self.cam
                .fit(self.canvas_rect(), [-40 * U, -40 * U, 1240 * U, 840 * U]);
            self.fitted = true;
        }
    }

    fn command(&mut self, name: &str, arg: &str) -> bool {
        let view = self.canvas_rect();
        let step = view.width().min(view.height()) / 8.0;
        match name {
            "view.zoom_in" => self
                .cam
                .zoom_at(view, view.center().x, view.center().y, 2.0),
            "view.zoom_out" => self
                .cam
                .zoom_at(view, view.center().x, view.center().y, 0.5),
            "view.zoom_full" => self.cam.fit(view, [-40 * U, -40 * U, 1240 * U, 840 * U]),
            "view.pan_left" => self.cam.pan(step, 0.0),
            "view.pan_right" => self.cam.pan(-step, 0.0),
            "view.pan_up" => self.cam.pan(0.0, step),
            "view.pan_down" => self.cam.pan(0.0, -step),
            "options.draw_grid" => {
                self.grid = !self.grid;
                self.recompose();
            }
            "tools.insert_text" => {
                let d = self.strings.intern("dialog.enter_text");
                self.state.set_dialog(d);
                self.dirty_layout = true;
            }
            "dialog.close" | "dialog.enter_text.ok" => {
                self.state.set_dialog(Sym::NONE);
                self.dirty_layout = true;
            }
            _ => {}
        }
        println!(
            "command {name}{}{arg}",
            if arg.is_empty() { "" } else { " " }
        );
        true
    }

    fn canvas_event(&mut self, ev: InputEvent) -> bool {
        let view = self.canvas_rect();
        match ev {
            InputEvent::PointerDown {
                x,
                y,
                button: Button::Middle,
            } => self.drag = Some((x, y)),
            InputEvent::PointerDown {
                x,
                y,
                button: Button::Left,
            } => {
                self.marks = Marks::default();
                if let Some(i) = pick_line(&self.prims, &self.cam, view, x, y) {
                    self.marks.push(PrimKind::Line, i, 0xffff_ffff);
                }
                return true;
            }
            InputEvent::PointerMove { x, y } => {
                let Some((px, py)) = self.drag else {
                    return false;
                };
                self.cam.pan(f64::from(x - px), f64::from(y - py));
                self.drag = Some((x, y));
                return true;
            }
            InputEvent::PointerUp {
                button: Button::Middle,
                ..
            } => self.drag = None,
            InputEvent::Wheel { x, y, dy, .. } => {
                self.cam.zoom_at(
                    view,
                    f64::from(x),
                    f64::from(y),
                    1.25f64.powf(f64::from(dy)),
                );
                return true;
            }
            _ => {}
        }
        false
    }
}

impl App for Demo {
    fn update(&mut self, events: &InputEvents, viewport: (f32, f32)) -> bool {
        let mut changed = viewport != self.viewport;
        if changed {
            self.viewport = viewport;
            self.dirty_layout = true;
        }
        if self.dirty_layout {
            self.relayout();
        }
        let mut routed = std::mem::take(&mut self.routed);
        route_input(
            &self.tables,
            &self.frame,
            &self.strings,
            &mut self.text,
            &mut self.state,
            events,
            &mut routed,
        );
        for i in 0..routed.commands.len() {
            let name = self.strings.get(routed.commands.command[i]).to_owned();
            changed |= self.command(&name, routed.commands.arg(i));
        }
        for &ev in &routed.canvas {
            changed |= self.canvas_event(ev);
        }
        changed |= routed.redraw || routed.relayout || self.dirty_layout;
        if routed.relayout || self.dirty_layout {
            self.relayout();
        }
        self.routed = routed;
        changed
    }

    fn paint<P: Painter>(&mut self, p: &mut P, _viewport: (f32, f32)) {
        #[cfg(not(target_arch = "wasm32"))]
        let t0 = std::time::Instant::now();
        let (tables, frame, strings, state) =
            (&self.tables, &self.frame, &self.strings, &self.state);
        draw_ui(
            p,
            tables,
            frame,
            strings,
            &mut self.text,
            state,
            Layer::Main,
            &[],
            &mut self.ui_scratch,
        );
        let view = self.canvas_rect();
        let grid_step = if self.grid { 20 * U } else { i32::MAX / 4 };
        canvas::draw_canvas(
            p,
            view,
            &self.cam,
            grid_step,
            &self.prims,
            &self.palette,
            &self.overlay,
            &self.marks,
            &mut self.text,
            &mut self.canvas_scratch,
        );
        draw_ui(
            p,
            &self.tables,
            &self.frame,
            &self.strings,
            &mut self.text,
            &self.state,
            Layer::Overlay,
            &[],
            &mut self.ui_scratch,
        );
        #[cfg(not(target_arch = "wasm32"))]
        if std::env::var_os("SCHEMIFY_FPS").is_some() {
            self.timing.0 += 1;
            self.timing.1 += t0.elapsed().as_secs_f64();
            if self.timing.0 == 120 {
                eprintln!(
                    "scene build: {:.3} ms/frame (avg of 120)",
                    self.timing.1 / 120.0 * 1e3
                );
                self.timing = (0, 0.0);
            }
        }
    }

    fn access(&mut self) -> schemify_render::accesskit::TreeUpdate {
        access::access_tree(&self.tables, &self.frame, &self.strings, &self.state)
    }
}

/// Index of the segment within 4 px of `(x, y)`, if any. O(lines); fine for a demo pick.
fn pick_line(prims: &CanvasPrims, cam: &Camera, view: Rect, x: f32, y: f32) -> Option<u32> {
    let l = &prims.lines;
    let (px, py) = (f64::from(x), f64::from(y));
    (0..l.x0.len())
        .map(|i| {
            let (ax, ay) = cam.to_screen(view, l.x0[i], l.y0[i]);
            let (bx, by) = cam.to_screen(view, l.x1[i], l.y1[i]);
            let (dx, dy) = (bx - ax, by - ay);
            let t =
                (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy).max(1e-9)).clamp(0.0, 1.0);
            let (qx, qy) = (ax + t * dx - px, ay + t * dy - py);
            (i, qx * qx + qy * qy)
        })
        .filter(|&(_, d2)| d2 <= 16.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i as u32)
}

/// A grid of amplifier-ish cells: resistors, capacitors, a source, a box symbol with pins, wires
/// and net labels. Layers follow xschem: 1 wire, 3 text, 4 symbol, 5 pin.
fn synthetic_schematic(p: &mut CanvasPrims, cols: i32, rows: i32) {
    for r in 0..rows {
        for c in 0..cols {
            let (ox, oy) = (c * 200 * U, r * 200 * U);
            // Resistor: vertical zigzag between two pins.
            let mut zig = vec![(ox + 40 * U, oy + 20 * U), (ox + 40 * U, oy + 40 * U)];
            for k in 0..6 {
                let dx = if k % 2 == 0 { 8 } else { -8 };
                zig.push((ox + (40 + dx) * U, oy + (45 + k * 6) * U));
            }
            zig.extend([(ox + 40 * U, oy + 80 * U), (ox + 40 * U, oy + 100 * U)]);
            p.poly(&zig, false, 4);
            p.rect(ox + 38 * U, oy + 18 * U, ox + 42 * U, oy + 22 * U, 5);
            p.rect(ox + 38 * U, oy + 98 * U, ox + 42 * U, oy + 102 * U, 5);
            // Capacitor.
            p.line(ox + 100 * U, oy + 20 * U, ox + 100 * U, oy + 55 * U, 4);
            p.line(ox + 88 * U, oy + 55 * U, ox + 112 * U, oy + 55 * U, 4);
            p.line(ox + 88 * U, oy + 63 * U, ox + 112 * U, oy + 63 * U, 4);
            p.line(ox + 100 * U, oy + 63 * U, ox + 100 * U, oy + 100 * U, 4);
            // Voltage source: circle with + / -.
            p.arc(ox + 150 * U, oy + 60 * U, 15 * U, 0, 360_000, 4);
            p.line(ox + 150 * U, oy + 20 * U, ox + 150 * U, oy + 45 * U, 4);
            p.line(ox + 150 * U, oy + 75 * U, ox + 150 * U, oy + 100 * U, 4);
            p.line(ox + 146 * U, oy + 52 * U, ox + 154 * U, oy + 52 * U, 4);
            p.line(ox + 150 * U, oy + 48 * U, ox + 150 * U, oy + 56 * U, 4);
            // Box symbol with pins.
            p.rect(ox + 60 * U, oy + 120 * U, ox + 140 * U, oy + 170 * U, 4);
            for k in 0..3 {
                let y = oy + (130 + k * 15) * U;
                p.line(ox + 50 * U, y, ox + 60 * U, y, 4);
                p.rect(ox + 48 * U, y - 2 * U, ox + 52 * U, y + 2 * U, 5);
            }
            p.text(ox + 66 * U, oy + 140 * U, 10 * U, 3, "amp");
            // Wires and labels.
            p.line(ox + 40 * U, oy + 20 * U, ox + 150 * U, oy + 20 * U, 1);
            p.line(ox + 40 * U, oy + 100 * U, ox + 150 * U, oy + 100 * U, 1);
            p.line(ox + 20 * U, oy + 130 * U, ox + 50 * U, oy + 130 * U, 1);
            p.line(ox + 20 * U, oy + 100 * U, ox + 20 * U, oy + 130 * U, 1);
            p.line(ox + 20 * U, oy + 100 * U, ox + 40 * U, oy + 100 * U, 1);
            p.poly(
                &[
                    (ox + 140 * U, oy + 145 * U),
                    (ox + 180 * U, oy + 145 * U),
                    (ox + 180 * U, oy + 110 * U),
                ],
                false,
                1,
            );
            p.text(
                ox + 44 * U,
                oy + 6 * U,
                12 * U,
                1,
                if r % 2 == 0 { "VPP" } else { "VDD" },
            );
            p.text(ox + 44 * U, oy + 104 * U, 12 * U, 1, "VSS");
            p.text(ox + 150 * U, oy + 150 * U, 12 * U, 1, "OUT");
        }
    }
}

fn main() {
    if let Err(e) = run("Schemify (shell demo)", Demo::new()) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
