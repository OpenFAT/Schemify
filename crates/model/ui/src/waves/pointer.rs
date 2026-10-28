//! Pointer input over the graph panes (spec waves §7): wheel zoom, pans, cursor drags, box
//! zoom and the double-click that opens the properties dialog, turned into viewer requests.
//!
//! Contents:
//! - `WaveUi::route(waves, geo, events, now_ms, out) -> Effect`
//! - `pub(super) struct PaneFraction`
//! - `pub(super) WaveUi::pointer_at(geo) -> Option<PaneFraction>`

use schemify_wave::{Cursor, PaneMode, Request, Scale, Waves};

use super::{DOUBLE_CLICK_MS, Drag, Effect, Geometry, PlotPress, WaveUi};
use crate::input::{Button, InputEvent, InputEvents};
use crate::keys::Mods;

/// A point inside a pane's plot as fractions of it, clamped to `[0, 1]`.
#[derive(Clone, Copy, Debug)]
pub(super) struct PaneFraction {
    /// The pane.
    pub(super) pane: u8,
    /// From the plot's left edge.
    pub(super) x: f64,
    /// From the plot's bottom edge.
    pub(super) y: f64,
}

/// Pointer distance in logical pixels within which a press grabs a cursor line.
const GRAB_PX: f32 = 6.0;

impl WaveUi {
    /// Routes one frame of pointer input over the panes (and the wheel over the signal list):
    /// wheel zooms x about the pointer (y with Shift or over the y-axis strip), left drag pans
    /// or moves a cursor, right drag box-zooms, double-click (or a right click on a cursor)
    /// opens the properties dialog. Appends to `out`.
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
            let e = self.event(waves, geo, ev, now_ms, out);
            fx.or(e);
        }
        fx
    }

    /// The pane under the pointer with the pointer's fractions of its plot (x from the left, y
    /// from the bottom), clamped to `[0, 1]`.
    pub(super) fn pointer_at(&self, geo: &Geometry) -> Option<PaneFraction> {
        let (x, y) = self.pointer?;
        let k = geo.pane_at(x, y)?;
        let p = super::plot_area(geo.panes[usize::from(k)]);
        let fx = f64::from((x - p.x) / p.w).clamp(0.0, 1.0);
        let fy = f64::from((p.y + p.h - y) / p.h).clamp(0.0, 1.0);
        debug_assert!((0.0..=1.0).contains(&fx), "clamped x fraction");
        debug_assert!((0.0..=1.0).contains(&fy), "clamped y fraction");
        Some(PaneFraction {
            pane: k,
            x: fx,
            y: fy,
        })
    }
}

// ---- private ----

impl WaveUi {
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
            Drag::Pan { pane, .. }
            | Drag::PanY { pane, .. }
            | Drag::Box { pane, .. }
            | Drag::Cursor { pane, .. }
            | Drag::HCursor { pane, .. } => Some(pane),
            Drag::None => None,
        };
        if held.is_some_and(|p| usize::from(p) >= geo.panes.len()) {
            self.drag = Drag::None;
        }
        let plot = |k: u8| super::plot_area(geo.panes[usize::from(k)]);
        let x_scale = |k: u8| {
            let p = plot(k);
            waves.x_of(usize::from(k)).scale(p.x, p.x + p.w)
        };
        let y_scale = |k: u8| {
            let p = plot(k);
            waves.panes()[usize::from(k)].scale(p.y + p.h, p.y)
        };
        match ev {
            InputEvent::Mods { mods } => self.mods = mods,
            InputEvent::PointerMove { x, y } => {
                self.pointer = Some((x, y));
                fx.redraw |= self.measure;
                match self.drag {
                    Drag::Pan { pane, x: x0, y: y0 } => {
                        let p = plot(pane);
                        if x != x0 {
                            out.push(Request::PanX {
                                pane,
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
                    Drag::Cursor { pane, which } => {
                        let p = plot(pane);
                        let at = x_scale(pane).value(x.clamp(p.x, p.x + p.w));
                        out.push(Request::Cursor {
                            which,
                            at: Some(at),
                        });
                    }
                    Drag::HCursor { pane, which } => {
                        let p = plot(pane);
                        let at = y_scale(pane).value(y.clamp(p.y, p.y + p.h));
                        out.push(Request::HCursor {
                            pane,
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
                            fx.recompose = true; // the Log X/Y boxes show the active pane
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
                let grabbed = if in_plot {
                    grab(waves, k, &x_scale(k), &y_scale(k), x, y)
                } else {
                    None
                };
                match button {
                    Button::Left if in_plot => {
                        let double = self.last_down.is_some_and(|last| {
                            now_ms.saturating_sub(last.at_ms) <= DOUBLE_CLICK_MS
                                && (last.x - x).abs() <= 4.0
                                && (last.y - y).abs() <= 4.0
                        });
                        self.last_down = Some(PlotPress {
                            x,
                            y,
                            at_ms: now_ms,
                        });
                        if double {
                            self.last_down = None;
                            fx.or(self.open_dialog(waves, k));
                            return fx;
                        }
                        self.drag = grabbed.unwrap_or(Drag::Pan { pane: k, x, y });
                    }
                    Button::Left if x < p.x => self.drag = Drag::PanY { pane: k, y },
                    Button::Right if in_plot && grabbed.is_some() => {
                        fx.or(self.open_dialog(waves, k));
                    }
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
                            pane,
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
                            pane: k,
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
            InputEvent::PointerLeave => {
                fx.redraw |= self.measure && self.pointer.is_some();
                self.pointer = None;
            }
            _ => {}
        }
        fx
    }
}

/// The cursor line within [`GRAB_PX`] of a press at `(x, y)` in pane `k`'s plot (scales `sx`,
/// `sy`), as the drag that moves it: a vertical cursor (locked panes only: they show the shared
/// x), else a horizontal one (analog and histogram panes).
fn grab(waves: &Waves, k: u8, sx: &Scale, sy: &Scale, x: f32, y: f32) -> Option<Drag> {
    let p = usize::from(k);
    let props = waves.props(p);
    let near =
        |c: Option<f64>, at: f32, s: &Scale| c.is_some_and(|v| (s.px(v) - at).abs() <= GRAB_PX);
    if !props.own_x()
        && let Some(which) = [Cursor::A, Cursor::B]
            .into_iter()
            .find(|&c| near(waves.cursors()[c as usize], x, sx))
    {
        return Some(Drag::Cursor { pane: k, which });
    }
    if props.mode == PaneMode::Digital {
        return None;
    }
    let hc = waves.hcursors(p);
    [Cursor::A, Cursor::B]
        .into_iter()
        .find(|&c| near(hc[c as usize], y, sy))
        .map(|which| Drag::HCursor { pane: k, which })
}
