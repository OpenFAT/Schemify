//! Oracles for the waveform window (spec waves): its composition fits the window, pointer
//! gestures map to the viewer requests they mean, commands drive the viewer tables end to end,
//! the graph properties dialog round-trips, and the readout strings read the cursors.
//!
//! Contents: `pub(crate) const VIEW`, `pub(crate) struct Win` (`new`, `rebuild`, `apply`,
//! `route`, `show`, `command`, `rect`, `signal_rows`), `pub(crate) fn data() -> Waves`,
//! `pub(crate) fn inside(r, outer) -> bool`; the tests live in `window`, `pointer`,
//! `command`, `dialog` and `readout`.

mod command;
mod dialog;
mod pointer;
mod readout;
mod window;

use schemify_ui::waves::{DialogFx, Effect, Geometry, PROPS_DIALOG, WAVES_UI, WaveCommand, WaveUi};
use schemify_ui::{
    Contribution, InputEvent, InputEvents, Rect, Strings, Sym, TextSystem, UiLayout, UiState,
    UiTables, compose, layout, records,
};
use schemify_wave::{RawPlot, Request, VarKind, Variable, Waves};

/// The window's viewport in logical pixels.
pub(crate) const VIEW: (f32, f32) = (1200.0, 760.0);

/// One waveform window under test: its tables, layout, interaction state and viewer.
pub(crate) struct Win {
    pub(crate) strings: Strings,
    pub(crate) base: Contribution,
    pub(crate) tables: UiTables,
    pub(crate) frame: UiLayout,
    pub(crate) geo: Geometry,
    pub(crate) text: TextSystem,
    pub(crate) state: UiState,
    pub(crate) ui: WaveUi,
    pub(crate) waves: Waves,
    /// Effect of the last `route` or `command`.
    pub(crate) last: Effect,
}

/// 120 signals named `net<k>` on a 1 ms time axis, `net<k>(t) = sin(k t)`.
pub(crate) fn data() -> Waves {
    let n = 2001;
    let mut vars = vec![Variable {
        name: "time".into(),
        kind: VarKind::Time,
    }];
    let mut re: Vec<f64> = (0..n).map(|i| f64::from(i) * 5e-7).collect();
    for k in 1..=120 {
        vars.push(Variable {
            name: format!("v(net{k})"),
            kind: VarKind::Voltage,
        });
        re.extend((0..n).map(|i| (f64::from(k) * f64::from(i) * 1e-3).sin()));
    }
    let mut w = Waves::new(vec![
        RawPlot::from_columns("tran", vars, re, vec![]).expect("full columns"),
    ]);
    w.apply(&Request::AddTrace {
        src: "v(net1)".into(),
        dataset: 0,
        pane: 0,
    })
    .expect("v(net1) is in the dataset");
    w
}

impl Win {
    pub(crate) fn new(waves: Waves) -> Win {
        let mut strings = Strings::new();
        let (base, rejects) = records::parse(WAVES_UI, &mut strings);
        assert!(rejects.is_empty(), "{rejects:?}");
        let mut w = Win {
            strings,
            base,
            tables: UiTables::default(),
            frame: UiLayout::default(),
            geo: Geometry::default(),
            text: TextSystem::new(),
            state: UiState::default(),
            ui: WaveUi::default(),
            waves,
            last: Effect::default(),
        };
        w.rebuild();
        w
    }

    pub(crate) fn rebuild(&mut self) {
        let name = self.strings.intern("session");
        for _ in 0..3 {
            let c = self.ui.contribute(&self.waves, &mut self.strings);
            self.tables = compose(&self.strings, &self.base, &[(name, &c)]);
            self.frame = layout(
                &self.tables,
                &self.strings,
                &mut self.text,
                &self.state,
                VIEW,
            );
            self.geo = Geometry::of(&self.tables, &self.frame, &self.strings);
            let fit = (
                WaveUi::rows_fit(self.geo.signals),
                WaveUi::rows_fit(self.geo.traces),
            );
            if !self.ui.set_capacity(fit.0, fit.1) {
                return;
            }
        }
        panic!("list capacity does not settle");
    }

    pub(crate) fn apply(&mut self, reqs: Vec<Request>) -> Effect {
        let mut fx = Effect::default();
        for r in reqs {
            let res = self.waves.apply(&r);
            let e = self.ui.note(&self.waves, &r, res.as_ref());
            fx.redraw |= e.redraw;
            fx.recompose |= e.recompose;
        }
        if fx.recompose {
            self.rebuild();
        }
        fx
    }

    pub(crate) fn route(&mut self, evs: &[InputEvent], now: u64) -> Vec<Request> {
        let events = InputEvents {
            events: evs.to_vec(),
            text: String::new(),
        };
        let mut out = Vec::new();
        self.last = self
            .ui
            .route(&self.waves, &self.geo, &events, now, &mut out);
        self.show(self.last.dialog);
        out
    }

    /// Shows or closes the properties dialog as the app does.
    pub(crate) fn show(&mut self, fx: DialogFx) {
        match fx {
            DialogFx::Keep => return,
            DialogFx::Open => {
                let id = self.strings.intern(PROPS_DIALOG);
                self.state.set_dialog(id);
            }
            DialogFx::Close => self.state.set_dialog(Sym::NONE),
        }
        self.rebuild();
    }

    pub(crate) fn command(&mut self, name: &str, arg: &str, node: &str, now: u64) -> Vec<Request> {
        let mut out = Vec::new();
        let command = WaveCommand {
            name,
            arg,
            node,
            now_ms: now,
        };
        let fx = self.ui.command(&self.waves, &self.geo, command, &mut out);
        self.last = fx;
        if fx.recompose {
            self.rebuild();
        }
        self.show(fx.dialog);
        out
    }

    pub(crate) fn rect(&self, id: &str) -> Rect {
        let sym = self.strings.find(id).expect("the id is interned");
        let n = self.tables.nodes.find(sym).expect("the id names a node");
        let row = self.frame.row_of(n).expect("the node is laid out");
        self.frame.rect()[row]
    }

    pub(crate) fn signal_rows(&self) -> Vec<Sym> {
        let sym = self
            .strings
            .find("waves.signals")
            .expect("the signal list is interned");
        let list = self
            .tables
            .nodes
            .find(sym)
            .expect("the window has a signal list");
        self.tables
            .nodes
            .children(list)
            .map(|c| self.tables.nodes.id()[c as usize])
            .collect()
    }
}

pub(crate) fn inside(r: Rect, outer: Rect) -> bool {
    r.x >= outer.x - 0.5
        && r.y >= outer.y - 0.5
        && r.x + r.w <= outer.x + outer.w + 0.5
        && r.y + r.h <= outer.y + outer.h + 0.5
}
