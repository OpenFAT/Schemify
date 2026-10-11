//! The waveform dock (D23): `render::waves` over one `.raw` file, docked under the schematic
//! until closed (`Ctrl+W`, or Escape over the dock with nothing open). Same wiring as the
//! `waves` example in `crates/render`: the viewer tables are `wave::Waves`, its UI is
//! `ui::waves`, and every viewer change is a `wave::Request`.
//!
//! Opened by the Waves command on a file the user picks, or by the results table on a job's
//! raw file. Docked rather than full-window so the canvas stays clickable: a click on a net
//! cross-probes it into the open viewer ([`WavesView::add_trace`]). The dock lays out and
//! routes input in its own coordinates; the editor shifts pointer events into them and
//! paints it through [`Shifted`].

use schemify_render::Painter;
use schemify_render::vello::Glyph;
use schemify_render::vello::kurbo::{Affine, BezPath, Rect as KRect};
use schemify_render::vello::peniko::{Color, FontData};
use schemify_render::waves::{WaveLook, WaveScratch, draw_window};
use schemify_render::widgets::{UiScratch, draw_ui};
use schemify_ui::waves::{Geometry, WAVES_UI, WaveUi};
use schemify_ui::{
    Chord, Contribution, InputEvent, InputEvents, Key, Layer, Mods, Routed, Strings, Sym,
    TextSystem, UiLayout, UiState, UiTables, compose, layout, records, route_input,
};
use schemify_wave::{Request, Waves, group_datasets, parse_raw};

/// One open waveform window.
pub struct WavesView {
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
    look: WaveLook,
    scratch: WaveScratch,
    ui_scratch: UiScratch,
    /// File name shown in the title.
    pub title: String,
}

impl std::fmt::Debug for WavesView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WavesView")
            .field("title", &self.title)
            .finish()
    }
}

impl WavesView {
    /// Parses a `.raw` file (ngspice/Xyce/LTspice, binary or ASCII) and opens the viewer.
    ///
    /// # Errors
    ///
    /// The reader's error text.
    pub fn open(title: &str, bytes: &[u8]) -> Result<WavesView, String> {
        let plots = parse_raw(bytes).map_err(|e| format!("{title}: {e}"))?;
        let waves = Waves::new(group_datasets(plots));
        let mut strings = Strings::new();
        let (base, rejects) = records::parse(WAVES_UI, &mut strings);
        assert!(rejects.is_empty(), "waves UI rejects: {rejects:?}");
        let session_name = strings.intern("waves.session");
        Ok(WavesView {
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
            look: WaveLook::default(),
            scratch: WaveScratch::default(),
            ui_scratch: UiScratch::default(),
            title: title.to_owned(),
        })
    }

    /// Recomposes and lays out; lists are measured empty first so their row count follows
    /// the window size (at most three passes, as in the `waves` example).
    fn rebuild(&mut self) {
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

    /// One frame of input. Returns `(redraw, close)`; `close` asks the editor to return to
    /// the schematic.
    pub fn update(&mut self, events: &InputEvents, viewport: (f32, f32), now: u64) -> (bool, bool) {
        let close_keys = [
            Chord::new(Mods(Mods::CTRL), Key('w' as u32)),
            Chord::new(Mods::default(), Key::ESCAPE),
        ];
        let idle = self.state.open().is_empty() && self.state.dialog().is_none();
        let close = events.events.iter().any(|e| {
            matches!(e, InputEvent::Key { chord } if close_keys.contains(chord)
                && (idle || *chord == close_keys[0]))
        });
        if close {
            return (true, true);
        }
        let mut changed = viewport != self.viewport;
        let mut recompose = changed;
        if changed {
            self.viewport = viewport;
            self.ui.set_capacity(0, 0);
            self.rebuild();
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
        let fx = self
            .ui
            .route(&self.waves, &self.geo, events, now, &mut self.requests);
        changed |= fx.redraw || !self.requests.is_empty();
        recompose |= fx.recompose;
        for r in std::mem::take(&mut self.requests) {
            let res = self.waves.apply(&r);
            recompose |= self.ui.note(&self.waves, &r, &res).recompose;
        }
        if (recompose || routed.relayout) && self.viewport.0 > 0.0 {
            self.rebuild();
        }
        changed |= routed.redraw || recompose;
        self.routed = routed;
        (changed, false)
    }

    /// Adds a trace for calculator expression `src` (cross-probe: `v(net)`, `i(dev)`) to the
    /// active pane, over the last dataset that evaluates it. A trace with the same source is
    /// not added twice.
    ///
    /// # Errors
    ///
    /// No dataset evaluates `src` (the last dataset's reason), or the viewer is full.
    pub fn add_trace(&mut self, src: &str) -> Result<(), String> {
        if self.traces().iter().any(|t| t.eq_ignore_ascii_case(src)) {
            return Ok(());
        }
        let mut why = String::from("no dataset");
        for d in (0..self.waves.datasets().len()).rev() {
            let req = Request::AddTrace {
                src: src.to_owned(),
                dataset: d as u8,
                pane: self.ui.active_pane(),
            };
            let res = self.waves.apply(&req);
            match &res {
                Ok(schemify_wave::Applied::Trace(_)) => {
                    self.ui.note(&self.waves, &req, &res);
                    if self.viewport.0 > 0.0 {
                        self.rebuild();
                    }
                    return Ok(());
                }
                Ok(other) => why = format!("{other:?}"),
                Err(e) => why = e.to_string(),
            }
        }
        Err(format!("{src}: {why}"))
    }

    /// The source text of every trace, in legend order.
    pub fn traces(&self) -> Vec<&str> {
        (0..self.waves.trace_count())
            .map(|i| self.waves.src(i))
            .collect()
    }

    /// Paints the viewer.
    pub fn paint<P: Painter>(&mut self, p: &mut P) {
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
}

/// A painter that draws `dy` logical pixels lower: the waves dock lays out at its own origin
/// and is painted below the editor through this. Paths are copied per call (the dock draws
/// hundreds, not the canvas's hundreds of thousands).
pub struct Shifted<'a, P: Painter> {
    /// The window's painter.
    pub p: &'a mut P,
    /// Vertical offset in logical pixels.
    pub dy: f64,
}

impl<P: Painter> Shifted<'_, P> {
    fn rect(&self, r: KRect) -> KRect {
        KRect::new(r.x0, r.y0 + self.dy, r.x1, r.y1 + self.dy)
    }

    fn path(&self, path: &BezPath) -> BezPath {
        let mut out = path.clone();
        out.apply_affine(Affine::translate((0.0, self.dy)));
        out
    }
}

impl<P: Painter> Painter for Shifted<'_, P> {
    fn fill_rect(&mut self, r: KRect, color: Color) {
        let r = self.rect(r);
        self.p.fill_rect(r, color);
    }

    fn fill_path(&mut self, path: &BezPath, color: Color) {
        let path = self.path(path);
        self.p.fill_path(&path, color);
    }

    fn stroke_path(&mut self, path: &BezPath, width: f64, color: Color) {
        let path = self.path(path);
        self.p.stroke_path(&path, width, color);
    }

    fn glyphs(
        &mut self,
        font: &FontData,
        px: f32,
        coords: &[i16],
        color: Color,
        at: Affine,
        glyphs: &[Glyph],
    ) {
        let at = Affine::translate((0.0, self.dy)) * at;
        self.p.glyphs(font, px, coords, color, at, glyphs);
    }

    fn push_clip(&mut self, r: KRect) {
        let r = self.rect(r);
        self.p.push_clip(r);
    }

    fn pop_clip(&mut self) {
        self.p.pop_clip();
    }
}
