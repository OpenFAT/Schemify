//! The waveform window (D23, D26), built from UI tables: a static skeleton ([`WAVES_UI`]), the
//! rows that follow the viewer tables ([`WaveUi::contribute`]), and pointer input over the graph
//! panes turned into viewer [`Request`]s ([`WaveUi::route`]). The same pane input drives an
//! embedded graph (D23: one pane, no browser) given just that pane's rectangle.
//!
//! One frame of the window, in spine order (the caller owns the loop):
//!
//! ```text
//! route_input(tables, layout, events)        -> commands          (buttons, keys, list rows)
//! WaveUi::route(waves, geometry, events)     -> requests          (wheel, drags, box zoom)
//! WaveUi::command(waves, command, …)         -> requests
//! Waves::apply(request); WaveUi::note(…)     -> viewer tables, status text
//! WaveUi::contribute(waves)                  -> ui contribution   (only when `note` says so)
//! ```
//!
//! [`WaveUi`] holds only transient interaction state (drag, hover, selection, filter text); the
//! viewer tables stay in [`Waves`], written by `wave` alone.

use schemify_wave::calc::signal_expr;
use schemify_wave::{Applied, Cursor, LineStyle, Request, TRACE_COLORS, ViewError, Waves};

use crate::input::{Button, InputEvent, InputEvents, UiState};
use crate::layout::{Rect, UiLayout};
use crate::strings::{Strings, Sym};
use crate::tables::{Contribution, Edit, Kind, Mods, Prop, StyleProp, UiTables};

/// The window's static skeleton: toolbar, browser, pane column, trace panel, readout, status,
/// theme and keys. Compose it as the base contribution, then [`WaveUi::contribute`] on top.
pub const WAVES_UI: &str = include_str!("../waves-ui.records");

/// Height of one signal or trace row in logical pixels.
pub const ROW_H: f32 = 18.0;

/// Gap between two clicks of a double-click.
pub const DOUBLE_CLICK_MS: u64 = 400;

/// Plot area of a pane: the pane minus room for y tick labels (left), the legend (top) and x
/// tick labels (bottom). [`WaveUi::route`] and the renderer both use it, so a click lands where
/// the trace is drawn.
pub fn plot_area(pane: Rect) -> Rect {
    let (l, t, r, b) = (58.0, 18.0, 10.0, 18.0);
    Rect {
        x: pane.x + l,
        y: pane.y + t,
        w: (pane.w - l - r).max(1.0),
        h: (pane.h - t - b).max(1.0),
    }
}

/// A pointer gesture in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Drag {
    /// Nothing held.
    None,
    /// Left drag in a plot: pan x (shared) and this pane's y. Holds the last pointer position.
    Pan {
        /// Pane.
        pane: u8,
        /// Last x.
        x: f32,
        /// Last y.
        y: f32,
    },
    /// Left drag on the y-axis strip: pan this pane's y only (xschem).
    PanY {
        /// Pane.
        pane: u8,
        /// Last y.
        y: f32,
    },
    /// Left drag that started within 6 px of a cursor line.
    Cursor(Cursor),
    /// Right drag: box zoom from the first corner to the current pointer.
    Box {
        /// Pane.
        pane: u8,
        /// Press x.
        x0: f32,
        /// Press y.
        y0: f32,
        /// Current x.
        x1: f32,
        /// Current y.
        y1: f32,
    },
}

/// Screen rectangles [`WaveUi::route`] hit-tests, read from one layout by [`Geometry::of`] (or
/// built by hand for an embedded graph: one pane, the rest empty).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    /// Each pane's rectangle, top to bottom.
    pub panes: Vec<Rect>,
    /// The signal list.
    pub signals: Rect,
    /// The trace list.
    pub traces: Rect,
    /// The cursor readout panel.
    pub readout: Rect,
}

impl Geometry {
    /// Reads the waves window's rectangles from `layout`. A node not laid out gives an empty
    /// rectangle; panes stop at the first missing one.
    pub fn of(tables: &UiTables, layout: &UiLayout, strings: &Strings) -> Geometry {
        let rect = |id: &str| {
            strings
                .find(id)
                .and_then(|s| tables.nodes.find(s))
                .and_then(|n| layout.row_of(n))
                .map(|r| layout.rect()[r])
        };
        let mut panes = Vec::new();
        for k in 0..schemify_wave::MAX_PANES {
            match rect(&format!("waves.pane.{k}")) {
                Some(r) => panes.push(r),
                None => break,
            }
        }
        let g = Geometry {
            panes,
            signals: rect("waves.signals").unwrap_or_default(),
            traces: rect("waves.traces").unwrap_or_default(),
            readout: rect("waves.readout").unwrap_or_default(),
        };
        debug_assert!(g.panes.len() <= schemify_wave::MAX_PANES);
        g
    }

    fn pane_at(&self, x: f32, y: f32) -> Option<u8> {
        self.panes
            .iter()
            .position(|r| r.contains(x, y))
            .map(|k| k as u8)
    }
}

/// What the caller must do after routing or a command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Effect {
    /// Paint again (drag overlay, hover).
    pub redraw: bool,
    /// Rebuild the contribution and relayout (rows changed).
    pub recompose: bool,
}

impl Effect {
    fn or(&mut self, o: Effect) {
        self.redraw |= o.redraw;
        self.recompose |= o.recompose;
    }
}

/// Transient state of one waveform window (or embedded graph).
#[derive(Clone, Debug)]
pub struct WaveUi {
    dataset: u8,
    filter: String,
    expr: String,
    scroll: u32,
    signal_rows: u16,
    trace_rows: u16,
    selected_signal: Option<u32>,
    selected_trace: Option<u8>,
    active_pane: u8,
    status: String,
    drag: Drag,
    mods: Mods,
    pointer: Option<(f32, f32)>,
    last_click: Option<(u32, u64)>,
    last_down: Option<(f32, f32, u64)>,
    cursors_on: [bool; 2],
}

impl Default for WaveUi {
    fn default() -> Self {
        WaveUi {
            dataset: 0,
            filter: String::new(),
            expr: String::new(),
            scroll: 0,
            signal_rows: 0,
            trace_rows: 0,
            selected_signal: None,
            selected_trace: None,
            active_pane: 0,
            status: String::new(),
            drag: Drag::None,
            mods: Mods::default(),
            pointer: None,
            last_click: None,
            last_down: None,
            cursors_on: [false; 2],
        }
    }
}

impl WaveUi {
    /// The gesture in progress (the renderer draws the box-zoom rubber band from it).
    pub fn drag(&self) -> Drag {
        self.drag
    }

    /// The pane commands act on (the last one hovered or clicked).
    pub fn active_pane(&self) -> u8 {
        self.active_pane
    }

    /// The selected trace row, if any.
    pub fn selected_trace(&self) -> Option<u8> {
        self.selected_trace
    }

    /// Status-line text (last error, measurement or confirmation).
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Dataset the browser lists and new traces use.
    pub fn dataset(&self) -> u8 {
        self.dataset
    }

    /// Sets how many signal and trace rows fit, measured from a layout made with fewer rows
    /// (see [`WaveUi::rows_fit`]); returns true when that changes the contribution.
    pub fn set_capacity(&mut self, signal_rows: u16, trace_rows: u16) -> bool {
        let changed = (signal_rows, trace_rows) != (self.signal_rows, self.trace_rows);
        self.signal_rows = signal_rows;
        self.trace_rows = trace_rows;
        changed
    }

    /// Rows of [`ROW_H`] that fit in `r` (a list's rectangle).
    pub fn rows_fit(r: Rect) -> u16 {
        ((r.h - 2.0) / ROW_H).floor().clamp(0.0, 512.0) as u16
    }

    /// Variables of the current dataset that pass the filter (case-insensitive substring),
    /// skipping the scale column.
    fn visible_signals(&self, waves: &Waves) -> Vec<u32> {
        let Some(d) = waves.datasets().get(usize::from(self.dataset)) else {
            return Vec::new();
        };
        let f = self.filter.to_ascii_lowercase();
        (1..d.vars().len())
            .filter(|&v| f.is_empty() || d.vars()[v].name.to_ascii_lowercase().contains(&f))
            .map(|v| v as u32)
            .collect()
    }

    /// The rows that follow the viewer tables: signal rows (filtered, scrolled), one graph node
    /// per pane, trace rows, toggle states, field values and status. Interns every string it
    /// emits into `strings` (bounded by datasets' variable names and user input).
    pub fn contribute(&self, waves: &Waves, strings: &mut Strings) -> Contribution {
        let mut e = Vec::with_capacity(256);
        let mut s = |t: &str| strings.intern(t);
        let node = |e: &mut Vec<Edit>, id: Sym, kind: Kind, parent: Sym| {
            e.push(Edit::Node { id, kind });
            e.push(Edit::Parent { id, parent });
        };
        let set = |e: &mut Vec<Edit>, id: Sym, prop: Prop, value: Sym| {
            e.push(Edit::Set { id, prop, value });
        };
        // Row styles: one class per trace color, selected variants on a light background.
        let style = |e: &mut Vec<Edit>, sel: Sym, prop: StyleProp, value: u32| {
            e.push(Edit::Style {
                selector: sel,
                prop,
                value,
            });
        };
        for (class, bg) in [("wsig", 0), ("wsig-sel", 0xb4d4_ffff)] {
            let c = s(class);
            style(&mut e, c, StyleProp::Height, ROW_H as u32);
            style(&mut e, c, StyleProp::Pad, 4);
            style(&mut e, c, StyleProp::Font, 12);
            if bg != 0 {
                style(&mut e, c, StyleProp::Bg, bg);
            }
        }
        for (k, &color) in TRACE_COLORS.iter().enumerate() {
            for (suffix, bg) in [("", 0x2020_20ff), ("-sel", 0x5a5a_5aff)] {
                let c = s(&format!("wtrace{k}{suffix}"));
                style(&mut e, c, StyleProp::Height, ROW_H as u32);
                style(&mut e, c, StyleProp::Pad, 4);
                style(&mut e, c, StyleProp::Font, 12);
                style(&mut e, c, StyleProp::Fg, color);
                style(&mut e, c, StyleProp::Bg, bg);
                style(&mut e, c, StyleProp::HoverBg, 0x4040_40ff);
            }
        }

        // Browser: dataset title, then the visible window of the filtered signals.
        let datasets = waves.datasets();
        let title = match datasets.get(usize::from(self.dataset)) {
            Some(d) => {
                let mut name: String = d.name().chars().take(22).collect();
                if d.name().chars().count() > 22 {
                    name.push('…');
                }
                format!("{}/{} {name}", self.dataset + 1, datasets.len())
            }
            None => "no data".to_owned(),
        };
        let (dn, label) = (s("waves.dataset_name"), s(&title));
        set(&mut e, dn, Prop::Label, label);
        if let Some(d) = datasets.get(usize::from(self.dataset)) {
            let list = s("waves.signals");
            let sig_cmd = s("waves.signal");
            let (plain, sel) = (s("wsig"), s("wsig-sel"));
            let vis = self.visible_signals(waves);
            let start = (self.scroll as usize).min(vis.len());
            let end = (start + usize::from(self.signal_rows)).min(vis.len());
            for &v in &vis[start..end] {
                let var = &d.vars()[v as usize];
                let id = s(&format!("waves.sig.{v}"));
                node(&mut e, id, Kind::Item, list);
                let unit = var.kind.unit();
                let text = if unit.is_empty() {
                    var.name.clone()
                } else {
                    format!("{}  [{unit}]", var.name)
                };
                let label = s(&text);
                set(&mut e, id, Prop::Label, label);
                set(&mut e, id, Prop::Command, sig_cmd);
                let class = if self.selected_signal == Some(v) {
                    sel
                } else {
                    plain
                };
                set(&mut e, id, Prop::Style, class);
            }
        }

        // One graph node per pane, in pane order.
        let panes = s("waves.panes");
        for k in 0..waves.panes().len() {
            let id = s(&format!("waves.pane.{k}"));
            node(&mut e, id, Kind::Graph, panes);
        }

        // Trace rows (as many as fit), colored like their trace.
        let list = s("waves.traces");
        let trace_cmd = s("waves.trace");
        let shown = waves.trace_count().min(usize::from(self.trace_rows));
        for i in 0..shown {
            let id = s(&format!("waves.trace.{i}"));
            node(&mut e, id, Kind::Item, list);
            let bold = if waves.style(i) == LineStyle::Bold {
                " (bold)"
            } else {
                ""
            };
            let mag = if waves.is_magnitude(i) { "|·| " } else { "" };
            let label = s(&format!(
                "{} {mag}{}{bold}",
                waves.pane_of(i) + 1,
                waves.src(i)
            ));
            set(&mut e, id, Prop::Label, label);
            set(&mut e, id, Prop::Command, trace_cmd);
            let k = TRACE_COLORS
                .iter()
                .position(|&c| c == waves.color(i))
                .unwrap_or(0);
            let sel = if self.selected_trace == Some(i as u8) {
                "-sel"
            } else {
                ""
            };
            let class = s(&format!("wtrace{k}{sel}"));
            set(&mut e, id, Prop::Style, class);
        }
        if waves.trace_count() > shown {
            let id = s("waves.trace.more");
            node(&mut e, id, Kind::Label, list);
            let label = s(&format!("+{} more", waves.trace_count() - shown));
            set(&mut e, id, Prop::Label, label);
        }

        // Toggle states, field values, status.
        let (on, off) = (s("1"), s("0"));
        let pane = waves.panes().get(usize::from(self.active_pane));
        let cur = waves.cursors();
        for (id, v) in [
            ("waves.log_x", waves.x().log),
            ("waves.log_y", pane.is_some_and(|p| p.log)),
            ("waves.cursor_a", cur[0].is_some()),
            ("waves.cursor_b", cur[1].is_some()),
        ] {
            let id = s(id);
            set(&mut e, id, Prop::Checked, if v { on } else { off });
        }
        for (id, prop, v) in [
            ("waves.filter", Prop::Value, self.filter.as_str()),
            ("waves.expr", Prop::Value, self.expr.as_str()),
            ("waves.status_msg", Prop::Label, self.status.as_str()),
        ] {
            let (id, v) = (s(id), s(v));
            set(&mut e, id, prop, v);
        }
        assert!(
            e.len() <= Contribution::CAPACITY,
            "waves contribution too large"
        );
        Contribution { edits: e }
    }

    /// Copies the focused field's text into the live state (the filter applies as typed).
    /// Returns true when the filter changed, so the signal rows must be rebuilt.
    pub fn sync_fields(&mut self, state: &UiState, strings: &Strings) -> bool {
        let (Some(ed), focus) = (state.editor(), state.focus()) else {
            return false;
        };
        let text = ed.raw_text();
        if Some(focus) == strings.find("waves.expr") {
            if self.expr != text {
                self.expr = text.to_owned();
            }
            return false;
        }
        if Some(focus) == strings.find("waves.filter") && self.filter != text {
            self.filter = text.to_owned();
            self.scroll = 0;
            return true;
        }
        false
    }

    /// Routes one frame of pointer input over the panes (and the wheel over the signal list):
    /// wheel zooms x about the pointer (y with Shift or over the y-axis strip), left drag pans
    /// or moves a cursor, right drag box-zooms, double-click autoscales. Appends to `out`.
    ///
    /// # Panics
    ///
    /// Panics if `geo` lists more panes than `waves` has.
    pub fn route(
        &mut self,
        waves: &Waves,
        geo: &Geometry,
        events: &InputEvents,
        now_ms: u64,
        out: &mut Vec<Request>,
    ) -> Effect {
        assert!(
            geo.panes.len() <= waves.panes().len(),
            "more pane rects than panes"
        );
        let mut fx = Effect::default();
        for &ev in &events.events {
            fx.or(self.event(waves, geo, ev, now_ms, out));
        }
        fx
    }

    fn event(
        &mut self,
        waves: &Waves,
        geo: &Geometry,
        ev: InputEvent,
        now_ms: u64,
        out: &mut Vec<Request>,
    ) -> Effect {
        let mut fx = Effect::default();
        // A gesture never outlives its pane (a pane removed mid-drag, a stale geometry).
        let held = match self.drag {
            Drag::Pan { pane, .. } | Drag::PanY { pane, .. } | Drag::Box { pane, .. } => Some(pane),
            Drag::Cursor(_) => Some(0),
            Drag::None => None,
        };
        if held.is_some_and(|p| usize::from(p) >= geo.panes.len()) {
            self.drag = Drag::None;
        }
        let plot = |k: u8| plot_area(geo.panes[usize::from(k)]);
        let x_scale = |k: u8| {
            let p = plot(k);
            waves.x().scale(p.x, p.x + p.w)
        };
        match ev {
            InputEvent::Mods { mods } => self.mods = mods,
            InputEvent::PointerMove { x, y } => {
                self.pointer = Some((x, y));
                match self.drag {
                    Drag::Pan { pane, x: x0, y: y0 } => {
                        let p = plot(pane);
                        if x != x0 {
                            out.push(Request::PanX {
                                by: f64::from((x0 - x) / p.w),
                            });
                        }
                        if y != y0 {
                            out.push(Request::PanY {
                                pane,
                                by: f64::from((y - y0) / p.h),
                            });
                        }
                        self.drag = Drag::Pan { pane, x, y };
                    }
                    Drag::PanY { pane, y: y0 } => {
                        let p = plot(pane);
                        out.push(Request::PanY {
                            pane,
                            by: f64::from((y - y0) / p.h),
                        });
                        self.drag = Drag::PanY { pane, y };
                    }
                    Drag::Cursor(which) => {
                        let p = plot(0);
                        let at = x_scale(0).value(x.clamp(p.x, p.x + p.w));
                        out.push(Request::Cursor {
                            which,
                            at: Some(at),
                        });
                    }
                    Drag::Box { pane, x0, y0, .. } => {
                        self.drag = Drag::Box {
                            pane,
                            x0,
                            y0,
                            x1: x,
                            y1: y,
                        };
                        fx.redraw = true;
                    }
                    Drag::None => {
                        if let Some(k) = geo.pane_at(x, y)
                            && k != self.active_pane
                        {
                            self.active_pane = k;
                            fx.recompose = true; // the Log Y box shows the active pane
                        }
                    }
                }
            }
            InputEvent::PointerDown { x, y, button } => {
                let Some(k) = geo.pane_at(x, y) else {
                    return fx;
                };
                if k != self.active_pane {
                    self.active_pane = k;
                    fx.recompose = true;
                }
                let p = plot(k);
                let in_plot = p.contains(x, y);
                match button {
                    Button::Left if in_plot => {
                        let double = self.last_down.is_some_and(|(px, py, t)| {
                            now_ms.saturating_sub(t) <= DOUBLE_CLICK_MS
                                && (px - x).abs() <= 4.0
                                && (py - y).abs() <= 4.0
                        });
                        self.last_down = Some((x, y, now_ms));
                        if double {
                            out.push(Request::Autoscale);
                            self.last_down = None;
                            return fx;
                        }
                        let sx = x_scale(k);
                        let near = [Cursor::A, Cursor::B].into_iter().find(|&c| {
                            waves.cursors()[c as usize]
                                .is_some_and(|cx| (sx.px(cx) - x).abs() <= 6.0)
                        });
                        self.drag = match near {
                            Some(c) => Drag::Cursor(c),
                            None => Drag::Pan { pane: k, x, y },
                        };
                    }
                    Button::Left if x < p.x => self.drag = Drag::PanY { pane: k, y },
                    Button::Right if in_plot => {
                        self.drag = Drag::Box {
                            pane: k,
                            x0: x,
                            y0: y,
                            x1: x,
                            y1: y,
                        };
                    }
                    _ => {}
                }
            }
            InputEvent::PointerUp { button, .. } => {
                if let (
                    Button::Right,
                    Drag::Box {
                        pane,
                        x0,
                        y0,
                        x1,
                        y1,
                    },
                ) = (button, self.drag)
                {
                    let p = plot(pane);
                    if (x1 - x0).abs() >= 4.0 {
                        out.push(Request::BoxX {
                            from: f64::from((x0.min(x1) - p.x) / p.w),
                            to: f64::from((x0.max(x1) - p.x) / p.w),
                        });
                    }
                    if (y1 - y0).abs() >= 4.0 {
                        let bottom = p.y + p.h;
                        out.push(Request::BoxY {
                            pane,
                            from: f64::from((bottom - y0.max(y1)) / p.h),
                            to: f64::from((bottom - y0.min(y1)) / p.h),
                        });
                    }
                }
                fx.redraw |= self.drag != Drag::None;
                self.drag = Drag::None;
            }
            InputEvent::Wheel { x, y, dy, .. } => {
                let factor = 1.25f64.powf(-f64::from(dy));
                if let Some(k) = geo.pane_at(x, y) {
                    let p = plot(k);
                    let fy = f64::from((p.y + p.h - y) / p.h);
                    if x < p.x || self.mods.0 & Mods::SHIFT != 0 {
                        out.push(Request::ZoomY {
                            pane: k,
                            at: fy,
                            factor,
                        });
                    } else {
                        out.push(Request::ZoomX {
                            at: f64::from((x - p.x) / p.w),
                            factor,
                        });
                    }
                } else if geo.signals.contains(x, y) {
                    let n = self.visible_signals(waves).len() as u32;
                    let max = n.saturating_sub(u32::from(self.signal_rows));
                    let step = (-dy * 3.0).round() as i64;
                    let to = (i64::from(self.scroll) + step).clamp(0, i64::from(max)) as u32;
                    fx.recompose |= to != self.scroll;
                    self.scroll = to;
                }
            }
            InputEvent::PointerLeave => self.pointer = None,
            _ => {}
        }
        fx
    }

    /// Runs one command from the waves window (buttons, keys, list rows). `node` is the issuing
    /// node's id (`waves.sig.12`, `waves.trace.3`), `arg` a submitted field's text, `now_ms` a
    /// monotonic clock for double-clicks. Unknown names do nothing (and ask for no redraw).
    #[allow(clippy::too_many_arguments)]
    pub fn command(
        &mut self,
        waves: &Waves,
        geo: &Geometry,
        name: &str,
        arg: &str,
        node: &str,
        now_ms: u64,
        out: &mut Vec<Request>,
    ) -> Effect {
        let mut fx = Effect {
            redraw: true,
            recompose: false,
        };
        let pane = self.active_pane;
        let trace = self.selected_trace;
        let pointer_frac = || {
            let (x, y) = self.pointer?;
            let k = geo.pane_at(x, y)?;
            let p = plot_area(geo.panes[usize::from(k)]);
            Some(f64::from((x - p.x) / p.w).clamp(0.0, 1.0))
        };
        match name {
            "waves.plot" | "waves.plot_button" => {
                if name == "waves.plot" {
                    self.expr = arg.to_owned();
                }
                let src = self.expr.trim();
                if !src.is_empty() {
                    out.push(Request::AddTrace {
                        src: src.to_owned(),
                        dataset: self.dataset,
                        pane,
                    });
                }
            }
            "waves.filter" => {
                self.filter = arg.to_owned();
                self.scroll = 0;
                fx.recompose = true;
            }
            "waves.signal" => {
                let Some(v) = node.strip_prefix("waves.sig.").and_then(|v| v.parse().ok()) else {
                    return fx;
                };
                let double = self.last_click.is_some_and(|(last, t)| {
                    last == v && now_ms.saturating_sub(t) <= DOUBLE_CLICK_MS
                });
                self.selected_signal = Some(v);
                self.last_click = Some((v, now_ms));
                fx.recompose = true;
                if double {
                    self.add_signal(waves, v, out);
                    self.last_click = None;
                }
            }
            "waves.add_signal" => {
                if let Some(v) = self.selected_signal {
                    self.add_signal(waves, v, out);
                }
            }
            "waves.trace" => {
                if let Some(i) = node
                    .strip_prefix("waves.trace.")
                    .and_then(|v| v.parse().ok())
                {
                    self.selected_trace = Some(i);
                    fx.recompose = true;
                }
            }
            "waves.delete_trace" => {
                if let Some(t) = trace {
                    out.push(Request::RemoveTrace { trace: t });
                }
            }
            "waves.bold" => {
                if let Some(t) = trace.filter(|&t| usize::from(t) < waves.trace_count()) {
                    let style = match waves.style(usize::from(t)) {
                        LineStyle::Thin => LineStyle::Bold,
                        LineStyle::Bold => LineStyle::Thin,
                    };
                    out.push(Request::SetStyle { trace: t, style });
                }
            }
            "waves.move_trace" => {
                if let Some(t) = trace.filter(|&t| usize::from(t) < waves.trace_count()) {
                    let n = waves.panes().len() as u8;
                    let to = (waves.pane_of(usize::from(t)) + 1) % n;
                    out.push(Request::MoveTrace { trace: t, pane: to });
                }
            }
            "waves.add_pane" => out.push(Request::AddPane),
            "waves.remove_pane" => out.push(Request::RemovePane { pane }),
            "waves.autoscale" => out.push(Request::Autoscale),
            "waves.log_x" => out.push(Request::LogX { on: !waves.x().log }),
            "waves.log_y" => {
                let on = waves.panes().get(usize::from(pane)).is_some_and(|p| !p.log);
                out.push(Request::LogY { pane, on });
            }
            "waves.cursor_a" | "waves.cursor_b" => {
                let which = if name == "waves.cursor_a" {
                    Cursor::A
                } else {
                    Cursor::B
                };
                let at = match waves.cursors()[which as usize] {
                    Some(_) => None,
                    None => {
                        let f = pointer_frac().unwrap_or(if which == Cursor::A {
                            1.0 / 3.0
                        } else {
                            2.0 / 3.0
                        });
                        Some(waves.x().at(f))
                    }
                };
                out.push(Request::Cursor { which, at });
            }
            "waves.swap_cursors" => out.push(Request::SwapCursors),
            "waves.pan_left" | "waves.pan_right" => {
                let by = if name == "waves.pan_left" {
                    -0.05
                } else {
                    0.05
                };
                out.push(Request::PanX { by });
            }
            "waves.zoom_in" | "waves.zoom_out" => {
                let factor = if name == "waves.zoom_in" { 0.8 } else { 1.25 };
                out.push(Request::ZoomX {
                    at: pointer_frac().unwrap_or(0.5),
                    factor,
                });
            }
            "waves.dataset_prev" | "waves.dataset_next" => {
                let n = waves.datasets().len().max(1) as u8;
                self.dataset = if name == "waves.dataset_prev" {
                    (self.dataset + n - 1) % n
                } else {
                    (self.dataset + 1) % n
                };
                self.scroll = 0;
                self.selected_signal = None;
                fx.recompose = true;
            }
            _ => fx.redraw = false,
        }
        fx
    }

    fn add_signal(&self, waves: &Waves, v: u32, out: &mut Vec<Request>) {
        let Some(d) = waves.datasets().get(usize::from(self.dataset)) else {
            return;
        };
        let e = signal_expr(d, v as usize);
        // On AC data a plain node is shown as its Bode magnitude.
        let src = if d.is_complex() {
            format!("db({e})")
        } else {
            e
        };
        out.push(Request::AddTrace {
            src,
            dataset: self.dataset,
            pane: self.active_pane,
        });
    }

    /// Records the outcome of one applied request: status text, selection and pane bookkeeping.
    /// Returns what the window must redo.
    pub fn note(
        &mut self,
        waves: &Waves,
        req: &Request,
        res: &Result<Applied, ViewError>,
    ) -> Effect {
        let mut fx = Effect {
            redraw: true,
            recompose: false,
        };
        match res {
            Err(e) => {
                self.status = e.to_string();
                fx.recompose = true;
                return fx;
            }
            Ok(Applied::Value(v)) => {
                self.status = v.clone();
                fx.recompose = true;
                return fx;
            }
            Ok(Applied::Trace(i)) => {
                self.selected_trace = Some(*i);
                self.status = format!("plotted {}", waves.src(usize::from(*i)));
            }
            Ok(Applied::Done) => {}
        }
        match req {
            Request::RemoveTrace { trace } => {
                self.selected_trace = match self.selected_trace {
                    Some(s) if s == *trace => None,
                    Some(s) if s > *trace => Some(s - 1),
                    s => s,
                };
            }
            Request::RemovePane { .. } => {
                let last = waves.panes().len().saturating_sub(1) as u8;
                self.active_pane = self.active_pane.min(last);
                self.selected_trace = None;
            }
            _ => {}
        }
        let on = waves.cursors().map(|c| c.is_some());
        fx.recompose |= on != self.cursors_on
            || matches!(
                req,
                Request::AddTrace { .. }
                    | Request::RemoveTrace { .. }
                    | Request::MoveTrace { .. }
                    | Request::SetStyle { .. }
                    | Request::AddPane
                    | Request::RemovePane { .. }
                    | Request::LogX { .. }
                    | Request::LogY { .. }
            );
        self.cursors_on = on;
        debug_assert!(usize::from(self.active_pane) < waves.panes().len());
        fx
    }
}
