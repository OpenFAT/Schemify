//! Commands of the waveform window (buttons, keys, list rows, the dialog) turned into viewer
//! requests, and the bookkeeping after each request is applied.
//!
//! Contents:
//! - `WaveUi::command(waves, geo, name, arg, node, now_ms, out) -> Effect`
//! - `WaveUi::note(waves, req, res) -> Effect`

use schemify_wave::calc::signal_expr;
use schemify_wave::{
    Applied, Cursor, LineStyle, PALETTE, PaneMode, Radix, Request, ViewError, Waves,
};

use super::dialog::field_index;
use super::{DOUBLE_CLICK_MS, DialogFx, Effect, Geometry, WaveUi};

impl WaveUi {
    /// Runs one command from the waves window (buttons, keys, list rows, the dialog). `node` is
    /// the issuing node's id (`waves.sig.12`, `waves.trace.3`, `waves.props.f.4`), `arg` a
    /// submitted field's text, `now_ms` a monotonic clock for double-clicks. Unknown names do
    /// nothing (and ask for no redraw).
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
            ..Effect::default()
        };
        let active = self.active_pane;
        let trace = self.selected_trace;
        let at_pointer = self.pointer_at(geo);
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
                        pane: active,
                    });
                }
            }
            // The fields' `input` commands (plugin-abi §11.14): the filter applies as typed,
            // the expression and the dialog's fields are kept for Enter/OK.
            "waves.expr" => {
                self.expr = arg.to_owned();
                fx.redraw = false;
            }
            "waves.filter" => {
                self.filter = arg.to_owned();
                self.scroll = 0;
                fx.recompose = true;
            }
            "waves.props.field" => {
                if let (Some(k), Some(d)) = (field_index(node), self.dialog.as_mut()) {
                    d.text[k] = arg.to_owned();
                    fx.recompose = true;
                }
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
            "waves.remove_pane" => out.push(Request::RemovePane { pane: active }),
            "waves.autoscale" => out.push(Request::Autoscale),
            "waves.log_x" => {
                let on = !waves.x_of(usize::from(active)).log;
                out.push(Request::LogX { pane: active, on });
            }
            "waves.log_y" => {
                let on = waves
                    .panes()
                    .get(usize::from(active))
                    .is_some_and(|p| !p.log);
                out.push(Request::LogY { pane: active, on });
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
                        // At the pointer when it is over a pane showing the shared x.
                        let f = at_pointer
                            .filter(|&(k, ..)| !waves.props(usize::from(k)).own_x())
                            .map_or(cursor_home(which), |(_, f, _)| f);
                        Some(waves.x().at(f))
                    }
                };
                out.push(Request::Cursor { which, at });
            }
            "waves.hcursor_a" | "waves.hcursor_b" => {
                let which = if name == "waves.hcursor_a" {
                    Cursor::A
                } else {
                    Cursor::B
                };
                let (pane, fy) = match at_pointer {
                    Some((k, _, fy)) => (k, Some(fy)),
                    None => (active, None),
                };
                let p = usize::from(pane);
                if p < waves.panes().len() {
                    let at = match waves.hcursors(p)[which as usize] {
                        Some(_) => None,
                        None => Some(waves.panes()[p].at(fy.unwrap_or(cursor_home(which)))),
                    };
                    out.push(Request::HCursor { pane, which, at });
                }
            }
            "waves.swap_cursors" => out.push(Request::SwapCursors),
            "waves.measure" => {
                self.measure = !self.measure;
                self.status = if self.measure {
                    "measure: on (y= x= at the pointer)".into()
                } else {
                    "measure: off".into()
                };
                fx.recompose = true;
            }
            "waves.track" => {
                fx.recompose = true;
                let Some((k, f, fy)) = at_pointer else {
                    self.status = "track: point at a pane".into();
                    return fx;
                };
                let p = usize::from(k);
                let mut props = waves.props(p);
                if props.step.is_some() {
                    props.step = None;
                    self.status = "tracking off: every step".into();
                } else {
                    let (x, y) = (waves.x_of(p).at(f), waves.panes()[p].at(fy));
                    let Some(s) = waves.closest_step(k, x, y) else {
                        self.status = "track: no step under the pointer".into();
                        return fx;
                    };
                    props.step = Some(s);
                    self.status = format!("tracking step {s}");
                }
                out.push(Request::SetPane { pane: k, props });
            }
            "waves.pan_left" | "waves.pan_right" => {
                let by = if name == "waves.pan_left" {
                    -0.05
                } else {
                    0.05
                };
                out.push(Request::PanX { pane: active, by });
            }
            "waves.zoom_in" | "waves.zoom_out" => {
                let factor = if name == "waves.zoom_in" { 0.8 } else { 1.25 };
                let (pane, at) = at_pointer.map_or((active, 0.5), |(k, f, _)| (k, f));
                out.push(Request::ZoomX { pane, at, factor });
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
            "waves.props" => {
                let p = active.min(waves.panes().len().saturating_sub(1) as u8);
                fx.or(self.open_dialog(waves, p));
            }
            "waves.props.mode" | "waves.props.radix" | "waves.props.unlock" => {
                if let Some(d) = self.dialog.as_mut() {
                    let p = &mut d.props;
                    match name {
                        "waves.props.mode" => {
                            p.mode = match p.mode {
                                PaneMode::Analog => PaneMode::Digital,
                                PaneMode::Digital => PaneMode::Histogram,
                                PaneMode::Histogram => PaneMode::Analog,
                            }
                        }
                        "waves.props.radix" => {
                            p.radix = match p.radix {
                                Radix::Hex => Radix::Dec,
                                Radix::Dec => Radix::Hex,
                            }
                        }
                        _ => p.locked = !p.locked,
                    }
                    fx.recompose = true;
                }
            }
            "waves.props.swatch" => {
                let k = node
                    .strip_prefix("waves.props.swatch.")
                    .and_then(|k| k.parse::<usize>().ok())
                    .filter(|&k| k < PALETTE.len());
                match (k, trace.filter(|&t| usize::from(t) < waves.trace_count())) {
                    (Some(k), Some(t)) => out.push(Request::SetColor {
                        trace: t,
                        color: PALETTE[k],
                    }),
                    _ => {
                        self.status = "colour: select a trace first".into();
                        fx.recompose = true;
                    }
                }
            }
            "waves.props.ok" => {
                let Some(mut d) = self.dialog.take() else {
                    return fx;
                };
                // Enter in a field submits it with its text (the editor may be gone already).
                if let Some(k) = field_index(node) {
                    d.text[k] = arg.to_owned();
                }
                fx.recompose = true;
                if usize::from(d.pane) >= waves.panes().len() {
                    fx.dialog = DialogFx::Close;
                    return fx;
                }
                match self.dialog_requests(waves, &d) {
                    Ok(reqs) => {
                        out.extend(reqs);
                        fx.dialog = DialogFx::Close;
                    }
                    Err(why) => {
                        self.status = why;
                        self.dialog = Some(d);
                    }
                }
            }
            "waves.props.cancel" => {
                self.dialog = None;
                fx.dialog = DialogFx::Close;
            }
            _ => fx.redraw = false,
        }
        fx
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
            ..Effect::default()
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
                    | Request::SetColor { .. }
                    | Request::SetPane { .. }
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

// ---- private ----

impl WaveUi {
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
}

/// Where a cursor turned on away from the pointer goes, as a fraction of its axis: a at a third,
/// b at two thirds.
fn cursor_home(which: Cursor) -> f64 {
    match which {
        Cursor::A => 1.0 / 3.0,
        Cursor::B => 2.0 / 3.0,
    }
}
