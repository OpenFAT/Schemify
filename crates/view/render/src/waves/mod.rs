//! Waveform panes (D23, spec `docs/spec/waves.md`): grid with engineering-unit ticks, decimated
//! traces (analog), logic lanes (digital) or value histograms, vertical and horizontal cursors,
//! the legend, the box-zoom band, the cursor readout table and the measurement tooltip. Generic
//! over [`Painter`], so the window, an embedded graph and the CPU golden images all draw through
//! the same code.
//!
//! Per frame and pane the cost is O(pixel columns) per trace step after
//! [`schemify_wave::decimate`] or [`schemify_wave::logic_runs`] (which scan the visible samples
//! once), O(samples) per histogram trace, plus shaping the tick and legend labels.
//!
//! Contents:
//! - `pub struct WaveLook` (`Default`: xschem's colours), `pub struct WaveScratch`
//! - `pub fn draw_window<P: Painter>(p, geo, waves, ui, look, text, s)`
//! - `pub fn draw_pane<P: Painter>(p, area, waves, k, active, drag, look, text, s)`

mod modes;

use std::mem;

use kurbo::{BezPath, Rect};
use peniko::Color;

use schemify_ui::graph;
use schemify_ui::parley::Layout;
use schemify_ui::waves::{self as ui_waves, Drag, Geometry, READOUT_HEADINGS, ReadoutRow, WaveUi};
use schemify_ui::{Rect as UiRect, TextSystem};
use schemify_wave::{Axis, Cursor, PaneMode, Run, Waves};

use crate::painter::{self, Chains, Painter};
use crate::widgets::{self, UiScratch};

/// Colors of the pane chrome (RGBA8). The default follows xschem's graphs: black plot, grey
/// grid, light labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaveLook {
    /// Pane background.
    pub bg: u32,
    /// Grid lines and plot frame.
    pub grid: u32,
    /// Tick labels.
    pub label: u32,
    /// Frame of the active pane (when there are several).
    pub active: u32,
    /// Cursor A and B.
    pub cursor: [u32; 2],
}

/// Buffers reused across frames (no allocation per frame after warm-up).
#[derive(Debug, Default)]
pub struct WaveScratch {
    pts: Vec<[f32; 2]>,
    chains: Chains,
    ticks: Vec<f64>,
    label: Layout<()>,
    text: String,
    path: BezPath,
    runs: Vec<Run>,
    bins: Vec<u32>,
    rows: Vec<ReadoutRow>,
    ui: UiScratch,
}

/// Tick label size in logical pixels.
const TICK_PX: f32 = 10.0;

/// Where a label sits relative to its anchor x.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

impl Default for WaveLook {
    fn default() -> Self {
        WaveLook {
            bg: 0x0000_00ff,
            grid: 0x3a3a_3aff,
            label: 0xb0b0_b0ff,
            active: 0x5a8c_d2ff,
            cursor: [0xffff_ffff, 0xff8c_ffff],
        }
    }
}

/// Draws every pane of the window into its layout rectangle, then the readout panel and, while
/// it is on, the measurement tooltip at the pointer.
///
/// # Panics
///
/// Panics when `geo` lays out more panes than `waves` holds.
pub fn draw_window<P: Painter>(
    p: &mut P,
    geo: &Geometry,
    waves: &Waves,
    ui: &WaveUi,
    look: &WaveLook,
    text: &mut TextSystem,
    s: &mut WaveScratch,
) {
    assert!(
        geo.panes.len() <= waves.panes().len(),
        "more pane rects than panes"
    );
    assert!(
        geo.panes.len() <= usize::from(u8::MAX) + 1,
        "pane indices are u8"
    );
    let several = geo.panes.len() > 1;
    for (k, &r) in geo.panes.iter().enumerate() {
        let active = several && ui.active_pane() as usize == k;
        draw_pane(p, r, waves, k as u8, active, ui.drag(), look, text, s);
    }
    if geo.readout.w > 0.0 && geo.readout.h > 0.0 {
        draw_readout(p, geo.readout, waves, ui.active_pane(), look, text, s);
    }
    if let (true, Some((x, y))) = (ui.measure(), ui.pointer()) {
        draw_measure(p, geo, waves, x, y, text, s);
    }
}

/// Draws pane `k` (its traces, grid, cursors, legend) filling `area`, in the pane's mode.
/// `drag` adds the box-zoom band when it belongs to this pane. An embedded graph calls this
/// with its own rectangle.
///
/// # Panics
///
/// Panics if `k` is not a pane of `waves`.
#[allow(
    clippy::too_many_arguments,
    reason = "a pane's data, rectangle, index and interaction state, its look, text and scratch"
)]
pub fn draw_pane<P: Painter>(
    p: &mut P,
    area: UiRect,
    waves: &Waves,
    k: u8,
    active: bool,
    drag: Drag,
    look: &WaveLook,
    text: &mut TextSystem,
    s: &mut WaveScratch,
) {
    assert!(usize::from(k) < waves.panes().len(), "no pane {k}");
    debug_assert!(area.w.is_finite(), "a laid-out pane");
    debug_assert!(area.h.is_finite(), "a laid-out pane");
    let pane = widgets::rect(area);
    p.fill_rect(pane, painter::rgba(look.bg));
    let pr = ui_waves::plot_area(area);
    if pr.w < 8.0 || pr.h < 8.0 {
        return;
    }
    let plot = widgets::rect(pr);
    let props = waves.props(usize::from(k));
    let (xa, ya) = (waves.x_of(usize::from(k)), waves.panes()[usize::from(k)]);
    let sx = xa.scale(pr.x, pr.x + pr.w);
    let sy = ya.scale(pr.y + pr.h, pr.y);
    let (grid, label) = (painter::rgba(look.grid), painter::rgba(look.label));
    let digital = props.mode == PaneMode::Digital;

    // Grid and tick labels (a digital pane has lanes, not a y scale).
    let nx = (pr.w / 90.0).clamp(2.0, 12.0) as u8;
    axis_ticks(xa, nx, &mut s.ticks);
    let ts = mem::take(&mut s.ticks);
    let digits = graph::tick_digits(&ts, xa.log);
    for &t in &ts {
        let x = f64::from(sx.px(t)).floor() + 0.5;
        if x < plot.x0 - 0.5 || x > plot.x1 + 0.5 {
            continue;
        }
        p.fill_rect(Rect::new(x - 0.5, plot.y0, x + 0.5, plot.y1), grid);
        label_at(
            p,
            text,
            s,
            &schemify_wave::format_si(t, digits),
            x,
            plot.y1 + 2.0,
            Align::Center,
            label,
        );
    }
    s.ticks = ts;
    if !digital {
        let ny = (pr.h / 26.0).clamp(2.0, 10.0) as u8;
        axis_ticks(ya, ny, &mut s.ticks);
        let ts = mem::take(&mut s.ticks);
        let digits = graph::tick_digits(&ts, ya.log);
        for &t in &ts {
            let y = f64::from(sy.px(t)).floor() + 0.5;
            if y < plot.y0 - 0.5 || y > plot.y1 + 0.5 {
                continue;
            }
            p.fill_rect(Rect::new(plot.x0, y - 0.5, plot.x1, y + 0.5), grid);
            let t = schemify_wave::format_si(t, digits);
            label_at(p, text, s, &t, plot.x0 - 4.0, y - 7.0, Align::Right, label);
        }
        s.ticks = ts;
    }
    painter::outline(p, &mut s.path, plot, 1.0, grid);

    // Traces, clipped to the plot.
    p.push_clip(plot);
    match props.mode {
        PaneMode::Analog => modes::draw_lines(p, waves, k, &sx, &sy, s),
        PaneMode::Digital => modes::draw_lanes(p, waves, k, &sx, plot, look, text, s),
        PaneMode::Histogram => modes::draw_histogram(p, waves, k, xa, &sx, &sy, plot, s),
    }
    // Vertical cursors span the plots that show the shared x; their tags sit on the x axis.
    for c in [Cursor::A, Cursor::B] {
        let Some(at) = waves.cursors()[c as usize].filter(|_| !props.own_x()) else {
            continue;
        };
        let x = f64::from(sx.px(at)).floor() + 0.5;
        if x < plot.x0 || x > plot.x1 {
            continue;
        }
        let color = painter::rgba(look.cursor[c as usize]);
        p.fill_rect(Rect::new(x - 0.5, plot.y0, x + 0.5, plot.y1), color);
        let tag = if c == Cursor::A { "A" } else { "B" };
        p.fill_rect(Rect::new(x - 6.0, plot.y0, x + 6.0, plot.y0 + 13.0), color);
        label_at(
            p,
            text,
            s,
            tag,
            x,
            plot.y0,
            Align::Center,
            painter::rgba(look.bg),
        );
    }
    // Horizontal cursors span the plot; their tags sit on the y axis.
    for c in [Cursor::A, Cursor::B] {
        let Some(at) = waves.hcursors(usize::from(k))[c as usize].filter(|_| !digital) else {
            continue;
        };
        let y = f64::from(sy.px(at)).floor() + 0.5;
        if y < plot.y0 || y > plot.y1 {
            continue;
        }
        let color = painter::rgba(look.cursor[c as usize]);
        p.fill_rect(Rect::new(plot.x0, y - 0.5, plot.x1, y + 0.5), color);
        let tag = if c == Cursor::A { "A" } else { "B" };
        p.fill_rect(Rect::new(plot.x0, y - 6.5, plot.x0 + 12.0, y + 6.5), color);
        label_at(
            p,
            text,
            s,
            tag,
            plot.x0 + 6.0,
            y - 6.5,
            Align::Center,
            painter::rgba(look.bg),
        );
    }
    if let Drag::Box {
        pane,
        x0,
        y0,
        x1,
        y1,
    } = drag
        && pane == k
    {
        let band = Rect::new(
            f64::from(x0.min(x1)),
            f64::from(y0.min(y1)),
            f64::from(x0.max(x1)),
            f64::from(y0.max(y1)),
        );
        p.fill_rect(band, Color::from_rgba8(120, 160, 255, 48));
        painter::outline(
            p,
            &mut s.path,
            band,
            1.0,
            Color::from_rgba8(150, 190, 255, 220),
        );
    }
    p.pop_clip();

    if digital {
        // Lane names in the y-label strip, in their trace's colour.
        let lh = plot.height() / traces_of(waves, k).count().max(1) as f64;
        debug_assert!(lh > 0.0, "a lane has height");
        for (j, i) in traces_of(waves, k).enumerate() {
            let y = plot.y0 + (j as f64 + 0.5) * lh - 7.0;
            let color = painter::rgba(waves.color(i));
            let name = ui_waves::lane_name(waves.src(i));
            label_at(p, text, s, name, plot.x0 - 4.0, y, Align::Right, color);
        }
    } else {
        // Legend: trace names in their colours, left to right until the plot's right edge.
        let mut lx = plot.x0 + 2.0;
        for i in traces_of(waves, k) {
            let mut t = mem::take(&mut s.text);
            ui_waves::legend_label(waves, i, &mut t);
            let color = painter::rgba(waves.color(i));
            let w = label_at(p, text, s, &t, lx, pane.y0 + 2.0, Align::Left, color);
            s.text = t;
            debug_assert!(w >= 0.0, "a label has a width");
            lx += w + 14.0;
            if lx > plot.x1 {
                break;
            }
        }
    }
    if active {
        painter::outline(
            p,
            &mut s.path,
            pane.inset(-0.5),
            1.0,
            painter::rgba(look.active),
        );
    }
}

// ---- private ----

/// Draws the cursor readout: the head lines of [`ui_waves::readout_head`], the column headings, then
/// the [`ui_waves::readout_rows`] that fit in `area`, each in its trace's colour.
fn draw_readout<P: Painter>(
    p: &mut P,
    area: UiRect,
    waves: &Waves,
    active: u8,
    look: &WaveLook,
    text: &mut TextSystem,
    s: &mut WaveScratch,
) {
    assert!(area.w > 0.0, "a readout panel has a width");
    assert!(area.h > 0.0, "a readout panel has a height");
    let r = widgets::rect(area);
    p.push_clip(r);
    let fg = painter::rgba(0xd0d0_d0ff);
    let row = 15.0;
    let mut y = r.y0 + 4.0;
    let head = ui_waves::readout_head(waves, active);
    // Columns: name 40%, then A, B and B−A at 20% each.
    let col = |k: usize| r.x0 + 6.0 + [0.0, 0.4, 0.6, 0.8][k] * (r.width() - 12.0);
    for (k, name) in ["A", "B"].into_iter().enumerate() {
        if head.shown[k] {
            let c = painter::rgba(look.cursor[k]);
            label_at(p, text, s, name, col(0), y, Align::Left, c);
        }
        label_at(p, text, s, &head.x[k], col(0) + 16.0, y, Align::Left, fg);
        y += row;
    }
    if let Some(t) = &head.dx {
        label_at(p, text, s, t, col(0), y, Align::Left, fg);
    }
    if let Some(t) = &head.dy {
        y += row;
        label_at(p, text, s, t, col(0), y, Align::Left, fg);
    }
    y += row + 4.0;
    for (k, h) in READOUT_HEADINGS.iter().enumerate() {
        label_at(
            p,
            text,
            s,
            h,
            col(k),
            y,
            Align::Left,
            painter::rgba(look.label),
        );
    }
    y += row;
    // A row whose bottom would pass the panel's is left out.
    let fit = ((r.y1 - y) / row).floor().max(0.0) as usize;
    let mut rows = mem::take(&mut s.rows);
    ui_waves::readout_rows(waves, fit, &mut rows);
    assert!(rows.len() <= fit, "the readout keeps to the rows that fit");
    for rr in &rows {
        let color = painter::rgba(rr.color);
        for (k, t) in rr.cells.iter().enumerate() {
            label_at(p, text, s, t, col(k), y, Align::Left, color);
        }
        y += row;
    }
    debug_assert!(rows.is_empty() || y <= r.y1, "a readout row overflows");
    s.rows = rows;
    p.pop_clip();
}

/// The measurement tooltip ([`ui_waves::measure_label`]) on a black box beside the pointer.
fn draw_measure<P: Painter>(
    p: &mut P,
    geo: &Geometry,
    waves: &Waves,
    x: f32,
    y: f32,
    text: &mut TextSystem,
    s: &mut WaveScratch,
) {
    debug_assert!(x.is_finite(), "a pointer on the screen");
    debug_assert!(y.is_finite(), "a pointer on the screen");
    let Some(t) = ui_waves::measure_label(geo, waves, x, y) else {
        return;
    };
    text.shape_into(&t, TICK_PX, &mut s.label);
    let w = f64::from(s.label.width());
    debug_assert!(w >= 0.0, "a shaped label has a width");
    let (bx, by) = (f64::from(x) + 12.0, f64::from(y) - 20.0);
    p.fill_rect(
        Rect::new(bx, by, bx + w + 8.0, by + 15.0),
        painter::rgba(0x0000_00ff),
    );
    widgets::draw_text(
        p,
        &s.label,
        bx + 4.0,
        by + 1.0,
        painter::rgba(0xffff_40ff),
        &mut s.ui,
    );
}

/// Tick positions for one axis: decades on a log axis, nice 1-2-5 steps otherwise.
fn axis_ticks(a: Axis, n: u8, out: &mut Vec<f64>) {
    assert!(n >= 2, "an axis shows at least two ticks");
    if a.log {
        graph::log_ticks(a.lo, a.hi, n, out);
    } else {
        graph::ticks(a.lo, a.hi, n, out);
    }
    debug_assert!(out.is_sorted(), "ticks run along the axis");
}

/// Shapes and draws `t` with its top at `y`, anchored at `x`; returns the width.
#[allow(
    clippy::too_many_arguments,
    reason = "a label's text, position, alignment and colour, with the text system and scratch"
)]
fn label_at<P: Painter>(
    p: &mut P,
    text: &mut TextSystem,
    s: &mut WaveScratch,
    t: &str,
    x: f64,
    y: f64,
    align: Align,
    color: Color,
) -> f64 {
    // ponytail: every tick, legend and readout label is shaped each frame (~0.3 ms for a
    // three-pane window). Cache by (text, size) in `TextSystem` if a profile ever shows it.
    debug_assert!(x.is_finite(), "a label anchored on the screen");
    debug_assert!(y.is_finite(), "a label anchored on the screen");
    text.shape_into(t, TICK_PX, &mut s.label);
    let w = f64::from(s.label.width());
    let x = match align {
        Align::Left => x,
        Align::Center => x - w / 2.0,
        Align::Right => x - w,
    };
    widgets::draw_text(p, &s.label, x, y, color, &mut s.ui);
    w
}

/// The traces drawn in pane `k`, in trace order.
fn traces_of(waves: &Waves, k: u8) -> impl Iterator<Item = usize> + '_ {
    assert!(usize::from(k) < waves.panes().len(), "no pane {k}");
    (0..waves.trace_count()).filter(move |&i| waves.pane_of(i) == k)
}
