//! The waveform viewer (D23) on a `.raw` file: signal browser with live filter, expression bar
//! (the `schemify-expr` calculator), stacked panes sharing x, traces and cursor readout.
//!
//! ```text
//! cargo run --release -p schemify-render --example waves -- FILE.raw [options] [EXPR | --pane]…
//! ```
//!
//! Each `EXPR` plots into the current pane; `--pane` starts the next one. `--embed` shows only
//! the first pane, as a graph embedded in a schematic or document would. `--cursors` places
//! cursors A and B at 30% and 70% of the view; `--logx` sets a log x axis.
//!
//! Mouse: wheel zooms x (Shift or over the y labels: y), left drag pans (or moves a cursor),
//! right drag box-zooms, double-click autoscales, double-click a signal to plot it. Keys: `a`/`b`
//! cursors at the pointer, `s` swap, `f` full view, Del delete the selected trace, arrows pan and
//! zoom. `SCHEMIFY_FPS=1` prints scene-build times.

use std::time::Instant;

use schemify_render::waves::{WaveLook, WaveScratch, draw_pane, draw_window};
use schemify_render::widgets::{UiScratch, draw_ui};
use schemify_render::{App, Painter, run};
use schemify_ui::waves::{Geometry, WAVES_UI, WaveUi};
use schemify_ui::{
    Contribution, InputEvents, Layer, Rect, Routed, Strings, Sym, TextSystem, UiLayout, UiState,
    UiTables, access, compose, layout, records, route_input,
};
use schemify_wave::{Cursor, Request, Waves, group_datasets, parse_raw};

struct Viewer {
    strings: Strings,
    base: Contribution,
    session: Contribution,
    session_name: Sym,
    tables: UiTables,
    text: TextSystem,
    state: UiState,
    frame: UiLayout,
    geo: Geometry,
    viewport: (f32, f32),
    routed: Routed,
    waves: Waves,
    ui: WaveUi,
    requests: Vec<Request>,
    embed: bool,
    look: WaveLook,
    scratch: WaveScratch,
    ui_scratch: UiScratch,
    start: Instant,
    timing: (u32, f64),
}

impl Viewer {
    fn new(waves: Waves, embed: bool) -> Viewer {
        let mut strings = Strings::new();
        let (base, rejects) = records::parse(WAVES_UI, &mut strings);
        assert!(rejects.is_empty(), "waves UI rejects: {rejects:?}");
        let session_name = strings.intern("waves.session");
        Viewer {
            strings,
            base,
            session: Contribution::default(),
            session_name,
            tables: UiTables::default(),
            text: TextSystem::new(),
            state: UiState::default(),
            frame: UiLayout::default(),
            geo: Geometry::default(),
            viewport: (0.0, 0.0),
            routed: Routed::default(),
            waves,
            ui: WaveUi::default(),
            requests: Vec::new(),
            embed,
            look: WaveLook::default(),
            scratch: WaveScratch::default(),
            ui_scratch: UiScratch::default(),
            start: Instant::now(),
            timing: (0, 0.0),
        }
    }

    /// Recomposes the window from the viewer tables and lays it out. The lists are measured
    /// empty first, so their row count follows the window size (at most three passes).
    fn rebuild(&mut self) {
        if self.embed {
            let (w, h) = self.viewport;
            self.geo = Geometry {
                panes: vec![Rect {
                    x: 0.0,
                    y: 0.0,
                    w,
                    h,
                }],
                ..Geometry::default()
            };
            return;
        }
        for _ in 0..3 {
            self.session = self.ui.contribute(&self.waves, &mut self.strings);
            self.tables = compose(
                &self.strings,
                &self.base,
                &[(self.session_name, &self.session)],
            );
            self.frame = layout(
                &self.tables,
                &self.strings,
                &mut self.text,
                &self.state,
                self.viewport,
            );
            self.geo = Geometry::of(&self.tables, &self.frame, &self.strings);
            let fit = (
                WaveUi::rows_fit(self.geo.signals),
                WaveUi::rows_fit(self.geo.traces),
            );
            if !self.ui.set_capacity(fit.0, fit.1) {
                break;
            }
        }
    }

    fn apply(&mut self) -> bool {
        let mut recompose = false;
        for r in std::mem::take(&mut self.requests) {
            let res = self.waves.apply(&r);
            recompose |= self.ui.note(&self.waves, &r, &res).recompose;
        }
        recompose
    }
}

impl App for Viewer {
    fn update(&mut self, events: &InputEvents, viewport: (f32, f32)) -> bool {
        let now = self.start.elapsed().as_millis() as u64;
        let mut changed = viewport != self.viewport;
        let mut recompose = changed;
        if changed {
            self.viewport = viewport;
            self.ui.set_capacity(0, 0);
            self.rebuild();
        }
        let mut routed = std::mem::take(&mut self.routed);
        if !self.embed {
            route_input(
                &self.tables,
                &self.frame,
                &self.strings,
                &mut self.text,
                &mut self.state,
                events,
                &mut routed,
            );
            recompose |= self.ui.sync_fields(&self.state, &self.strings);
            for i in 0..routed.commands.len() {
                let name = self.strings.get(routed.commands.command[i]).to_owned();
                let node = self.strings.get(routed.commands.node[i]).to_owned();
                let fx = self.ui.command(
                    &self.waves,
                    &self.geo,
                    &name,
                    routed.commands.arg(i),
                    &node,
                    now,
                    &mut self.requests,
                );
                changed |= fx.redraw;
                recompose |= fx.recompose;
            }
        }
        let fx = self
            .ui
            .route(&self.waves, &self.geo, events, now, &mut self.requests);
        changed |= fx.redraw || !self.requests.is_empty();
        recompose |= fx.recompose | self.apply();
        if recompose || routed.relayout {
            self.rebuild();
        }
        changed |= routed.redraw || recompose;
        self.routed = routed;
        changed
    }

    fn paint<P: Painter>(&mut self, p: &mut P, _viewport: (f32, f32)) {
        let t0 = Instant::now();
        if self.embed {
            let r = self.geo.panes[0];
            let drag = self.ui.drag();
            draw_pane(
                p,
                r,
                &self.waves,
                0,
                false,
                drag,
                &self.look,
                &mut self.text,
                &mut self.scratch,
            );
        } else {
            draw_ui(
                p,
                &self.tables,
                &self.frame,
                &self.strings,
                &mut self.text,
                &self.state,
                Layer::Main,
                &[],
                &mut self.ui_scratch,
            );
            draw_window(
                p,
                &self.geo,
                &self.waves,
                &self.ui,
                &self.look,
                &mut self.text,
                &mut self.scratch,
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
        }
        if std::env::var_os("SCHEMIFY_FPS").is_some() {
            self.timing.0 += 1;
            self.timing.1 += t0.elapsed().as_secs_f64();
            if self.timing.0 == 60 {
                eprintln!(
                    "scene build: {:.3} ms/frame (avg of 60)",
                    self.timing.1 / 60.0 * 1e3
                );
                self.timing = (0, 0.0);
            }
        }
    }

    fn access(&mut self) -> schemify_render::accesskit::TreeUpdate {
        access::access_tree(&self.tables, &self.frame, &self.strings, &self.state)
    }

    fn background(&self) -> schemify_render::vello::peniko::Color {
        schemify_render::vello::peniko::Color::from_rgba8(0xd9, 0xd9, 0xd9, 0xff)
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: waves FILE.raw [--embed] [--cursors] [--logx] [EXPR | --pane]...");
        std::process::exit(2);
    };
    let t0 = Instant::now();
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    let plots = parse_raw(&bytes).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    let datasets = group_datasets(plots);
    let points: usize = datasets.iter().map(|d| d.len() * d.vars().len()).sum();
    eprintln!(
        "{path}: {} dataset(s), {points} samples, read in {:.1} ms",
        datasets.len(),
        t0.elapsed().as_secs_f64() * 1e3
    );
    let mut waves = Waves::new(datasets);
    let (mut embed, mut pane, mut cursors) = (false, 0u8, false);
    for a in args {
        let r = match a.as_str() {
            "--embed" => {
                embed = true;
                continue;
            }
            "--cursors" => {
                cursors = true;
                continue;
            }
            "--logx" => Request::LogX { on: true },
            "--pane" => {
                pane += 1;
                Request::AddPane
            }
            src => Request::AddTrace {
                src: src.to_owned(),
                dataset: 0,
                pane,
            },
        };
        if let Err(e) = waves.apply(&r) {
            eprintln!("{a}: {e}");
        }
    }
    if cursors {
        for (which, f) in [(Cursor::A, 0.3), (Cursor::B, 0.7)] {
            let at = Some(waves.x().at(f));
            waves.apply(&Request::Cursor { which, at }).expect("cursor");
        }
    }
    if let Err(e) = run("Schemify waves", Viewer::new(waves, embed)) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
